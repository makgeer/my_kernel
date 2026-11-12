//! Пользовательские процессы — структура и запуск в ring 3.
//!
//! Каждый процесс имеет:
//! - собственную P4 таблицу (изолированное адресное пространство)
//! - код и данные, замапленные в lower-half (user space)
//! - стек пользователя
//!
//! Переход в ring 3 выполняет планировщик (timer_schedule_rust):
//! он подменяет IRET-фрейм на стеке обработчика таймера, и iretq
//! возвращает CPU в user mode с CR3 процесса.

use x86_64::structures::paging::page_table::PageTableFlags;
use x86_64::structures::paging::PhysFrame;
use x86_64::registers::control::{Cr3, Cr3Flags};
use crate::pmm;
use crate::paging;
use crate::println;
use crate::paging::copy_user_mapping_to_kernel;

/// Базовый виртуальный адрес для пользовательского кода.
/// Lower-half (бит 47 = 0) — пользовательское пространство.
pub const USER_CODE_BASE: u64 = 0x4000_0000_0000; // 0x0000400000000000

/// Базовый виртуальный адрес для пользовательского стека.
/// Растёт вниз от вершины.
pub const USER_STACK_TOP: u64 = 0x4000_0001_0000; // 0x0000400000010000
pub const USER_STACK_SIZE: usize = 4 * 1024; // 4 страницы = 16 KB

/// Guard pages: одна незараплененная страница сверху и снизу стека.
/// При переполнении — #PF (stack overflow detected), а не silent corruption.
pub const STACK_GUARD_PAGES: usize = 1;

/// VGA text buffer (0xB8000) — kernel-страница, которую нужно замапить
/// в P4 процесса. При активной user CR3 обработчики прерываний
/// (#PF, #DF, syscall) выполняются с ЭТОЙ P4 и вызывают println!,
/// который пишет в VGA. Мапится с U/S=0 — ring 3 доступа не получит
/// (U/S проверяется на уровне P1, где стоит только PRESENT|WRITABLE).
pub const VGA_BUFFER_ADDR: u64 = 0xB8000;

const PAGE_SIZE: u64 = 4096;

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

    // 3. Выделяем и мапим стек (растёт вниз от USER_STACK_TOP).
    // Guard page сверху (USER_STACK_TOP) — не мапим: при переполнении вверх → #PF.
    // Stack pages: от USER_STACK_TOP - 4K до USER_STACK_TOP - (stack_pages+1)*4K.
    // Guard page снизу: USER_STACK_TOP - (stack_pages+2)*4K — не мапим.
    let stack_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
    let stack_pages = USER_STACK_SIZE / 4096;

    // Верхний guard page: НЕ мапим (по адресу USER_STACK_TOP)
    // Это предотвращает переполнение стека вверх (push за пределами stack_top).

    // Мапим stack_pages страниц стека
    for i in 0..stack_pages {
        let phys = pmm::alloc_frame().ok_or("process: OOM for stack page")?;
        // Стек растёт вниз: верхняя страница у USER_STACK_TOP - 4K,
        // следующая ниже и т.д.
        let virt = USER_STACK_TOP - ((i + 1) as u64) * 4096;
        paging::map_page_in_p4(p4_phys, virt, phys, stack_flags)?;
    }

    // Нижний guard page: НЕ мапим (по адресу USER_STACK_TOP - (stack_pages+1)*4096)
    // Это предотвращает переполнение стека вниз (буферное переполнение).

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

    // 4. Выделяем и мапим стек (растёт вниз от USER_STACK_TOP).
    // Guard page сверху (USER_STACK_TOP) — не мапим: при переполнении → #PF.
    // Guard page снизу — не мапим.
    let stack_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
    let stack_pages = USER_STACK_SIZE / 4096;

    // Мапим stack_pages страниц стека (без guard pages — они НЕ мапятся)
    for i in 0..stack_pages {
        let phys = pmm::alloc_frame().ok_or("create_elf_process: OOM for stack page")?;
        // Стек растёт вниз: верхняя страница у USER_STACK_TOP - 4K,
        // следующая ниже и т.д.
        let virt = USER_STACK_TOP - ((i + 1) as u64) * 4096;
        paging::map_page_in_p4(p4_phys, virt, phys, stack_flags)?;
    }

    // Guard pages НЕ мапятся:
    // - Верхний: USER_STACK_TOP (не мапим)
    // - Нижний: USER_STACK_TOP - (stack_pages+1)*4096 (не мапим)
    // При переполнении стека → #PF → можно поймать и обработать.

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
/// Готовит процесс к запуску в ring 3 и возвращает управление в shell.
///
/// Процесс уже зарегистрирован в менеджере (spawn в create_*_process)
/// в состоянии Ready. Реальный переход в user mode выполняет таймер:
/// shell возвращается в цикл, входит в idle (sti; hlt), выставляет
/// SHELL_IDLE, и timer_schedule_rust переключается на процесс, сохранив
/// idle-контекст ядра. Возврат из процесса (exit/block) идёт через
/// take_idle_context — в ту же точку shell, без повторного вызова
/// shell() и без роста стека.
///
/// Раньше здесь был прямой iretq в user_mode_entry. При завершении
/// процесса управление возвращалось через kernel_return_to_shell,
/// который заново вызывал shell() с RSP на вершине KERNEL_RETURN_STACK.
/// Это давало рекурсию shell() и падение на границе стека (#DF).
pub fn run_process(proc: &Process) {
    println!("Scheduling ring 3 at entry 0x{:016x}...", proc.entry_point);

    // Диагностика маппингов в P4 процесса:
    // код, VGA и верхняя страница стека должны быть замаплены.
    let code_map = paging::check_mapping_in_p4(proc.p4_phys, proc.entry_point);
    let vga_map = paging::check_mapping_in_p4(proc.p4_phys, VGA_BUFFER_ADDR);
    let stack_map = paging::check_mapping_in_p4(proc.p4_phys, proc.stack_top - 8);
    println!(
        "  check_map: code=0x{:x?}, vga=0x{:x?}, stack=0x{:x?}",
        code_map, vga_map, stack_map
    );

    println!("  process will start from scheduler idle (PID registered)");
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
