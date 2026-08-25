//! Пользовательские процессы — структура и запуск в ring 3.
//!
//! Каждый процесс имеет:
//! - собственную P4 таблицу (изолированное адресное пространство)
//! - код и данные, замапленные в lower-half (user space)
//! - стек пользователя
//!
//! Переход в ring 3 (user mode) выполняется через IRETQ:
//! CPU восстанавливает SS:RSP, RFLAGS, CS:RIP из стека ядра.
//! Устанавливая CS/SS с RPL=3, CPU переходит в ring 3.

use x86_64::structures::paging::page_table::PageTableFlags;
use x86_64::structures::paging::PhysFrame;
use x86_64::registers::control::{Cr3, Cr3Flags};
use x86_64::VirtAddr;
use crate::pmm;
use crate::paging;
use crate::gdt::{USER_CODE_SELECTOR, USER_DATA_SELECTOR};
use crate::println;
use crate::paging::copy_user_mapping_to_kernel;

/// Базовый виртуальный адрес для пользовательского кода.
/// Lower-half (бит 47 = 0) — пользовательское пространство.
pub const USER_CODE_BASE: u64 = 0x4000_0000_0000; // 0x0000400000000000

/// Базовый виртуальный адрес для пользовательского стека.
/// Растёт вниз от вершины.
pub const USER_STACK_TOP: u64 = 0x4000_0001_0000; // 0x0000400000010000
pub const USER_STACK_SIZE: usize = 4 * 1024; // 4 страницы = 16 KB

/// VGA text buffer (0xB8000) — kernel-страница, которую нужно замапить
/// в P4 процесса. При активной user CR3 обработчики прерываний
/// (#PF, #DF, syscall) выполняются с ЭТОЙ P4 и вызывают println!,
/// который пишет в VGA. Мапится с U/S=0 — ring 3 доступа не получит
/// (U/S проверяется на уровне P1, где стоит только PRESENT|WRITABLE).
pub const VGA_BUFFER_ADDR: u64 = 0xB8000;

const PAGE_SIZE: u64 = 4096;

/// Глобальный указатель на P4 текущего пользовательского процесса.
/// Используется page fault handler для переключения CR3 при входе
/// в user mode (iretq выполняется с kernel CR3, PF handler переключает).
static mut CURRENT_USER_P4: u64 = 0;

/// Точка входа пользовательского процесса (для user_mode_entry).
static mut USER_ENTRY_POINT: u64 = 0;

/// Вершина пользовательского стека (для user_mode_entry).
static mut USER_STACK_PTR: u64 = 0;

/// Структура процесса.
#[derive(Clone, Copy)]
pub struct Process {
    /// Физический адрес P4 таблицы процесса.
    pub p4_phys: u64,
    /// Точка входа (виртуальный адрес в user space).
    pub entry_point: u64,
    /// Вершина стека пользователя (виртуальный адрес).
    pub stack_top: u64,
}

/// Создаёт новый процесс с заданным кодом.
///
/// # Аргументы
/// - `code` — байты пользовательского кода (будут скопированы в user space)
/// - `entry_offset` — смещение точки входа от начала кода
///
/// # Возвращает
/// Структуру Process или сообщение об ошибке.
///
/// # Что делает
/// 1. Создаёт новую P4 (create_user_p4): копия higher-half kernel
///    маппингов (256-511), нижняя половина — чистая (user space)
/// 2. Мапит VGA buffer (0xB8000) с U/S=0 — для println из обработчиков
/// 3. Выделяет физические фреймы для кода и стека
/// 4. Мапит код в USER_CODE_BASE с PRESENT | USER_ACCESSIBLE
/// 5. Мапит стек ниже USER_STACK_TOP с PRESENT | WRITABLE | USER_ACCESSIBLE
/// 6. Копирует код в выделенные фреймы через HHDM
pub fn create_process(code: &[u8], entry_offset: u64) -> Result<Process, &'static str> {
    // 1. Создаём изолированную P4 таблицу
    let p4_phys = paging::create_user_p4()?;

    // VGA text buffer: нужен обработчикам прерываний (#PF, #DF, syscall),
    // которые при активной user P4 вызывают println!. U/S=0 — ring 3
    // получить доступ к VGA не сможет (U/S проверяется на P1).
    paging::map_page_in_p4(
        p4_phys,
        VGA_BUFFER_ADDR,
        VGA_BUFFER_ADDR,
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
    )?;

    // 2. Выделяем и мапим страницы для кода
    // Сколько страниц нужно для кода
    let code_pages = (code.len() + 4095) / 4096;
    if code_pages == 0 {
        return Err("process: empty code");
    }

    let code_flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;

    for i in 0..code_pages {
        let phys = pmm::alloc_frame().ok_or("process: OOM for code page")?;
        let virt = USER_CODE_BASE + (i as u64) * 4096;
        paging::map_page_in_p4(p4_phys, virt, phys, code_flags)?;

        // Копируем код в физический фрейм через HHDM
        let hhdm = paging::hhdm_offset();
        unsafe {
            let dst = (phys + hhdm) as *mut u8;
            let src_start = i * 4096;
            let copy_len = if src_start + 4096 <= code.len() {
                4096
            } else {
                code.len() - src_start
            };
            core::ptr::copy_nonoverlapping(
                code.as_ptr().add(src_start),
                dst,
                copy_len,
            );
            // Остаток страницы заполняем нулями (padding)
            if copy_len < 4096 {
                core::ptr::write_bytes(dst.add(copy_len), 0, 4096 - copy_len);
            }
        }
    }

    // 3. Выделяем и мапим стек (растёт вниз от USER_STACK_TOP)
    let stack_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
    let stack_pages = USER_STACK_SIZE / 4096;

    for i in 0..stack_pages {
        let phys = pmm::alloc_frame().ok_or("process: OOM for stack page")?;
        // Стек растёт вниз: верхняя страница у USER_STACK_TOP - 4K,
        // следующая ниже и т.д.
        let virt = USER_STACK_TOP - ((i + 1) as u64) * 4096;
        paging::map_page_in_p4(p4_phys, virt, phys, stack_flags)?;
    }

    let entry_point = USER_CODE_BASE + entry_offset;

    // Маппим user-код и стек в kernel P4 (copy_user_mapping_to_kernel).
    // Это нужно для двухэтапного перехода в ring 3: второй iretq в user_mode_entry
    // читает IRET-фрейм со стека и fetch-ит инструкцию по RIP с kernel CR3
    // (до переключения на user P4). Без этого маппинга instruction fetch
    // по user-адресу → #PF → DOUBLE FAULT. После iretq CPU переключит CR3
    // на user P4, и будут использованы user-таблицы.
    for i in 0..code_pages {
        let virt = USER_CODE_BASE + (i as u64) * 4096;
        // Для kernel P4: PRESENT | USER_ACCESSIBLE (без WRITABLE для кода)
        let kflags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
        if let Err(e) = copy_user_mapping_to_kernel(virt, p4_phys, kflags) {
            println!("  WARNING: copy_mapping(code[{}]) failed: {}", i, e);
        }
    }

    // Маппинг user стека в kernel P4: PRESENT | USER_ACCESSIBLE | WRITABLE
    for i in 0..stack_pages {
        let virt = USER_STACK_TOP - ((i + 1) as u64) * 4096;
        let kflags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::WRITABLE;
        if let Err(e) = copy_user_mapping_to_kernel(virt, p4_phys, kflags) {
            println!("  WARNING: copy_mapping(stack[{}]) failed: {}", i, e);
        }
    }

    println!(
        "Process created: P4=0x{:016x}, entry=0x{:016x}, stack_top=0x{:016x}",
        p4_phys, entry_point, USER_STACK_TOP
    );

    Ok(Process {
        p4_phys,
        entry_point,
        stack_top: USER_STACK_TOP,
    })
}

/// Создаёт новый процесс из ELF64-файла.
///
/// # Аргументы
/// - `elf_bytes` — сырые байты ELF64 (ET_EXEC, x86_64), например из
///   `include_bytes!` встроенного в ядро файла.
///
/// # Что делает
/// 1. Создаёт новую P4 (create_user_p4) с копией всех kernel-маппингов
/// 2. Загружает PT_LOAD сегменты через elf::load_elf (PMM + paging + копия данных)
/// 3. Выделяет и мапит пользовательский стек (USER_STACK_SIZE страниц)
/// 4. Возвращает Process с entry_point из ELF header (e_entry)
///
/// В отличие от create_process (сырые байты кода по фиксированному
/// USER_CODE_BASE), здесь виртуальные адреса сегментов берутся из самого
/// ELF — процесс может загрузиться по любому адресу, заданному линкером.
pub fn create_elf_process(elf_bytes: &[u8]) -> Result<Process, &'static str> {
    // 1. Создаём изолированную P4 таблицу
    let p4_phys = paging::create_user_p4()?;

    // VGA text buffer: нужен обработчикам прерываний при активной user P4
    // (см. комментарий в create_process). U/S=0 — ring 3 не получит доступ.
    paging::map_page_in_p4(
        p4_phys,
        VGA_BUFFER_ADDR,
        VGA_BUFFER_ADDR,
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
    )?;

    // 2. Загружаем сегменты ELF в адресное пространство процесса
    let load_result = crate::elf::load_elf(elf_bytes, p4_phys)?;

    // 3. Маппим ELF-сегменты и стек в kernel P4 (для iretq fetch в user_mode_entry)
    for &(seg_virt, seg_memsz) in &load_result.segments {
        let seg_page_start = seg_virt & !(PAGE_SIZE - 1);
        let seg_page_end = (seg_virt + seg_memsz + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        let mut page = seg_page_start;
        while page < seg_page_end {
            // Используем USER_ACCESSIBLE + WRITABLE для всех ELF-страниц
            let kflags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::WRITABLE;
            if let Err(e) = copy_user_mapping_to_kernel(page, p4_phys, kflags) {
                println!("  WARNING: copy_mapping(elf page 0x{:x}) failed: {}", page, e);
            }
            page += PAGE_SIZE;
        }
    }

    // 4. Выделяем и мапим стек (растёт вниз от USER_STACK_TOP)
    let stack_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
    let stack_pages = USER_STACK_SIZE / 4096;

    for i in 0..stack_pages {
        let phys = pmm::alloc_frame().ok_or("create_elf_process: OOM for stack page")?;
        // Стек растёт вниз: верхняя страница у USER_STACK_TOP - 4K,
        // следующая ниже и т.д.
        let virt = USER_STACK_TOP - ((i + 1) as u64) * 4096;
        paging::map_page_in_p4(p4_phys, virt, phys, stack_flags)?;
    }

    // 5. Маппим стек в kernel P4
    for i in 0..stack_pages {
        let virt = USER_STACK_TOP - ((i + 1) as u64) * 4096;
        let kflags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::WRITABLE;
        if let Err(e) = copy_user_mapping_to_kernel(virt, p4_phys, kflags) {
            println!("  WARNING: copy_mapping(stack[{}]) failed: {}", i, e);
        }
    }

    println!(
        "ELF process created: P4=0x{:016x}, entry=0x{:016x}, stack_top=0x{:016x}",
        p4_phys, load_result.entry_point, USER_STACK_TOP
    );

    // Регистрируем процесс в менеджере процессов (для вытесняющего
    // планировщика). PID присваивается менеджером. Если менеджер
    // переполнен — процесс всё равно создаётся, но не планируется.
    match crate::process_manager::spawn(
        "elf",
        p4_phys,
        load_result.entry_point,
        USER_STACK_TOP,
    ) {
        Ok(pid) => {
            println!("  Registered in process manager: PID={}", pid);
        }
        Err(e) => {
            println!("  WARNING: process manager full: {}", e);
        }
    }

    Ok(Process {
        p4_phys,
        entry_point: load_result.entry_point,
        stack_top: USER_STACK_TOP,
    })
}

/// Переход в ring 3 (user mode) через IRETQ.
///
/// # Что делает
/// 1. Кладёт на стек ядра IRETQ-фрейм:
///    - SS = USER_DATA_SELECTOR (RPL=3)
///    - RSP = stack_top
///    - RFLAGS (с IF=1 — прерывания разрешены)
///    - CS = USER_CODE_SELECTOR (RPL=3)
///    - RIP = entry_point
/// 2. Выполняет IRETQ — CPU переходит в ring 3
///
/// Обычная extern "C" функция (не naked): по SysV ABI аргументы приходят
/// в rdi (entry), rsi (stack). Компилятор не трогает эти регистры до asm!.
///
/// ВАЖНО: вызывающая сторона должна передавать `stack` выровненным так,
/// чтобы `stack mod 16 != 0` (см. комментарий в run_process про #GP при
/// iretq в ring 3). Передавайте `USER_STACK_TOP - 8`, а не сам `USER_STACK_TOP`.
///
/// # Безопасность
/// После IRETQ код выполняется в ring 3. Возврат в kernel mode
/// возможен только через прерывание (syscall/int).
#[allow(improper_ctypes_definitions)]
pub unsafe extern "C" fn enter_ring3(entry: u64, stack: u64) -> ! {
    // Формируем IRETQ-фрейм на текущем (kernel) стеке.
    // Порядок (снизу вверх): SS, RSP, RFLAGS, CS, RIP
    core::arch::asm!(
        "push rsi",        // SS = USER_DATA_SELECTOR (передан в rsi)
        "push rdx",        // RSP = user stack (передан в rdx, должен быть ≡ 8 mod 16)
        "pushfq",          // RFLAGS (IF уже установлен — sti в init_idt)
        "push rcx",        // CS = USER_CODE_SELECTOR (передан в rcx)
        "push rdi",        // RIP = entry point (передан в rdi)
        "iretq",
        in("rdi") entry,
        in("rsi") USER_DATA_SELECTOR,
        in("rdx") stack,
        in("rcx") USER_CODE_SELECTOR,
        options(noreturn)
    );
}

/// Загружает программу из VFS по пути и создаёт процесс.
///
/// Читает ELF-файл из initrd (см. fs_initrd.rs), затем создаёт
/// процесс через create_elf_process. Это позволяет shell запускать
/// программы по имени (run hello.elf) вместо жёстко зашитых
/// include_bytes в tests.rs.
pub fn load_program_from_fs(path: &str) -> Result<Process, &'static str> {
    let data = crate::fs::read_file(path)
        .ok_or("fs: file not found")?;
    if data.is_empty() {
        return Err("fs: empty file");
    }
    create_elf_process(&data)
}

/// Запускает процесс: переключает адресное пространство и переходит в ring 3.
///
/// Это высокоуровневая обёртка над enter_ring3:
/// 1. Переключает CR3 на P4 процесса
/// 2. Вызывает enter_ring3(entry_point, stack_top)
///
/// После возврата из user mode (через прерывание) нужно восстановить
/// kernel CR3 — это будет в обработчике syscall.
///
/// ВАЖНО: P4 процесса содержит higher-half kernel-маппинги (256-511:
/// ядро, HHDM, framebuffer) + VGA buffer (0xB8000, U/S=0). Поэтому после
/// переключения CR3 kernel-функции (println!, VGA-вывод) работают
/// корректно, а ring 3 изолирован в чистой lower-half (user space).
///
/// Порядок выполнения:
/// 1. run_process формирует IRETQ-фрейм на kernel-стеке
/// 2. iretq → user_mode_entry (с kernel CS/SS, kernel CR3)
/// 3. user_mode_entry переключает CR3 на user P4
/// 4. user_mode_entry формирует новый IRETQ-фрейм с USER CS/SS
/// 5. iretq → user code (с USER CS/SS, user CR3)
pub fn run_process(proc: &Process) {
    // Печатаем ДО перехода — потом kernel P4 неактивна.
    println!("Launching ring 3 at entry 0x{:016x}...", proc.entry_point);

    // Диагностика маппингов в P4 процесса (до переключения CR3):
    // код, VGA и верхняя страница стека должны быть замаплены.
    let code_map = paging::check_mapping_in_p4(proc.p4_phys, proc.entry_point);
    let vga_map = paging::check_mapping_in_p4(proc.p4_phys, VGA_BUFFER_ADDR);
    let stack_map = paging::check_mapping_in_p4(proc.p4_phys, proc.stack_top - 8);
    println!(
        "  check_map: code=0x{:x?}, vga=0x{:x?}, stack=0x{:x?}",
        code_map, vga_map, stack_map
    );

    // Сохраняем параметры процесса в статиках для user_mode_entry.
    unsafe {
        CURRENT_USER_P4 = proc.p4_phys;
        USER_ENTRY_POINT = proc.entry_point;
        USER_STACK_PTR = proc.stack_top;
    }

    // Отмечаем процесс как Running в менеджере процессов.
    // Без этого terminate_current() при exit() не найдёт процесс →
    // он останется в состоянии Ready и будет виден в ps после завершения.
    crate::process_manager::set_current_by_p4(proc.p4_phys);

    // iretq → user_mode_entry (kernel функция).
    // user_mode_entry переключит CR3 и перейдёт к user code.
    // order (снизу вверх): SS, RSP, RFLAGS, CS, RIP
    //
    // ВАЖНО: user_mode_entry — это kernel-функция (ring 0), поэтому
    // SS и CS должны быть KERNEL selectors, а RSP — kernel stack.
    // user_mode_entry сама создаст IRETQ-фрейм с USER selectors.
    unsafe {
        // Берём вершину KERNEL_RETURN_STACK для iretq
        let kernel_stack = crate::gdt::KERNEL_RETURN_STACK.0.as_ptr() as u64
            + crate::gdt::KERNEL_RETURN_STACK.0.len() as u64;
        core::arch::asm!(
            "push {ss}",
            "push {sp}",
            "pushfq",
            "push {cs}",
            "push {rip}",
            "iretq",
            ss = in(reg) crate::gdt::KERNEL_DATA_SELECTOR,
            sp = in(reg) kernel_stack,
            cs = in(reg) crate::gdt::KERNEL_CODE_SELECTOR,
            rip = in(reg) user_mode_entry as u64,
            options(noreturn)
        );
    }
}

/// Переход в user mode с переключением CR3.
/// Вызывается из iretq после run_process.
/// Переключает CR3 на user P4, затем iretq к user code.
#[allow(improper_ctypes_definitions)]
pub unsafe extern "C" fn user_mode_entry() -> ! {
    let p4_phys = CURRENT_USER_P4;
    let entry = USER_ENTRY_POINT;
    let stack = USER_STACK_PTR;

    // ДИАГНОСТИКА: маркер входа в user_mode_entry — подтверждает, что
    // iretq из run_process дошёл до этой функции. Также печатаем текущий
    // CR3 (должен быть kernel P4) и целевую P4 процесса. Если этой строки
    // нет в логе — iretq/фрейм osбиваются ещё ДО user_mode_entry.
    {
        let (cur_frame, _) = Cr3::read();
        crate::println!(
            "user_mode_entry: entered (cur CR3=0x{:016x}, target P4=0x{:016x}, entry=0x{:016x})",
            cur_frame.start_address().as_u64(),
            p4_phys,
            entry
        );
    }

    // Переключаем CR3 на user P4
    if p4_phys != 0 {
        let frame = PhysFrame::containing_address(x86_64::PhysAddr::new(p4_phys));
        Cr3::write(frame, x86_64::registers::control::Cr3Flags::empty());
        // После переключения сразу проверяем фактический CR3 — код ниже
        // (iretq к user code) исполняется уже в user адресном пространстве.
        let (new_frame, _) = Cr3::read();
        crate::println!(
            "user_mode_entry: CR3 switched (actual CR3=0x{:016x})",
            new_frame.start_address().as_u64()
        );
        // КРИТИЧЕСКИ: инвалидируем TLB после переключения CR3!
        // Без invlpg CPU использует stale entries из kernel P4,
        // что приводит к page fault при instruction fetch по USER_CODE_BASE.
        x86_64::instructions::tlb::flush(VirtAddr::new(USER_CODE_BASE));
    }

    // Теперь выполняем iretq с user CR3 → user code.
    // Формируем IRETQ-фрейм на текущем стеке:
    // SS, RSP, RFLAGS, CS, RIP
    core::arch::asm!(
        "push {ss}",
        "push {sp}",
        "pushfq",
        "push {cs}",
        "push {rip}",
        "iretq",
        ss = in(reg) USER_DATA_SELECTOR,
        sp = in(reg) stack - 8,
        cs = in(reg) USER_CODE_SELECTOR,
        rip = in(reg) entry,
        options(noreturn)
    );
}

/// Восстанавливает kernel CR3 после возврата из user mode.
/// Вызывается из обработчика syscall/int при возврате в kernel mode.
/// Сейчас используется в syscall_handler (interrupts.rs).
pub fn restore_kernel_cr3() {
    let kernel_p4 = paging::kernel_p4_phys();
    if kernel_p4 != 0 {
        unsafe {
            let frame = PhysFrame::containing_address(x86_64::PhysAddr::new(kernel_p4));
            Cr3::write(frame, Cr3Flags::empty());
        }
    }
}
