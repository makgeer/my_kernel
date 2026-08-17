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
use crate::pmm;
use crate::paging;
use crate::gdt::{USER_CODE_SELECTOR, USER_DATA_SELECTOR};
use crate::println;

/// Базовый виртуальный адрес для пользовательского кода.
/// Lower-half (бит 47 = 0) — пользовательское пространство.
pub const USER_CODE_BASE: u64 = 0x4000_0000_0000; // 0x0000400000000000

/// Базовый виртуальный адрес для пользовательского стека.
/// Растёт вниз от вершины.
pub const USER_STACK_TOP: u64 = 0x4000_0001_0000; // 0x0000400000010000
pub const USER_STACK_SIZE: usize = 4 * 1024; // 4 страницы = 16 KB

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
/// 1. Создаёт новую P4 (create_user_p4) с копией kernel-маппингов
/// 2. Выделяет физические фреймы для кода и стека
/// 3. Мапит код в USER_CODE_BASE с PRESENT | USER_ACCESSIBLE
/// 4. Мапит стек ниже USER_STACK_TOP с PRESENT | WRITABLE | USER_ACCESSIBLE
/// 5. Копирует код в выделенные фреймы через HHDM
pub fn create_process(code: &[u8], entry_offset: u64) -> Result<Process, &'static str> {
    // 1. Создаём изолированную P4 таблицу
    let p4_phys = paging::create_user_p4()?;

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

/// Запускает процесс: переключает адресное пространство и переходит в ring 3.
///
/// Это высокоуровневая обёртка над enter_ring3:
/// 1. Переключает CR3 на P4 процесса
/// 2. Вызывает enter_ring3(entry_point, stack_top)
///
/// После возврата из user mode (через прерывание) нужно восстановить
/// kernel CR3 — это будет в обработчике syscall.
///
/// ВАЖНО: после Cr3::write НЕЛЬЗЯ вызывать println! — VGA буфер (0xB8000)
/// находится в lower-half (P4 index 0), который НЕ скопирован в user P4.
/// Любая запись в VGA → page fault → нет обработчика → double fault →
/// double fault handler тоже пишет в VGA → triple fault → Bochs reset.
pub fn run_process(proc: &Process) {
    // Печатаем ДО переключения CR3 — потом VGA будет недоступен.
    println!("Switching CR3 to user P4, entering ring 3...");

    // Переключаем CR3 на P4 процесса.
    // Флаги CR3: PCD=0, PWT=0 (normal caching).
    // ПОСЛЕ ЭТОЙ СТРОКИ НИКАКИХ println! — user P4 не содержит VGA mapping.
    unsafe {
        let frame = PhysFrame::containing_address(x86_64::PhysAddr::new(proc.p4_phys));
        Cr3::write(frame, x86_64::registers::control::Cr3Flags::empty());

        // Переходим в user mode напрямую через inline asm.
        // Формируем IRETQ-фрейм на текущем (kernel) стеке:
        // порядок (снизу вверх): SS, RSP, RFLAGS, CS, RIP
        // Инлайн избегает проблем с prologue extern "C" функции.
        //
        // ВАЖНО про RSP (stack_top - 8): iretq в ring 3 требует, чтобы
        // НОВЫЙ RSP (из фрейма) был выровнен на 16 байт (RSP mod 16 != 0).
        // USER_STACK_TOP = 0x400000010000 кратен 16 → iretq генерирует
        // #GP(0) → нет обработчика #GP → DOUBLE FAULT. Bochs строго
        // следует спецификации (QEMU это пропускал).
        // stack_top - 8 = 0x...FFF8 ≡ 8 (mod 16) — соответствует SysV ABI
        // (RSP % 16 == 8 при входе в программу) и лежит в замапленной
        // странице (верхняя страница стека 0x...F000..0x...FFFF).
        core::arch::asm!(
            "push {ss}",
            "push {sp}",
            "pushfq",
            "push {cs}",
            "push {rip}",
            "iretq",
            ss = in(reg) USER_DATA_SELECTOR,
            sp = in(reg) proc.stack_top - 8,
            cs = in(reg) USER_CODE_SELECTOR,
            rip = in(reg) proc.entry_point,
            options(noreturn)
        );
    }
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
