use heapless::Vec;
use spin::Mutex;
use spin::Once;
use x86_64::instructions::port::Port;
use x86_64::instructions::segmentation::{CS, SS, Segment};
use x86_64::instructions::interrupts;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};
use x86_64::structures::paging::{page_table::PageTableFlags, PhysFrame};
use x86_64::structures::gdt::SegmentSelector;
use x86_64::{PhysAddr, PrivilegeLevel, VirtAddr};
use x86_64::registers::control::{Cr3, Cr3Flags};
use core::sync::atomic::{AtomicUsize, Ordering};

/// Счётчик тиков таймера — диагностика DOUBLE FAULT
pub static TIMER_TICKS: AtomicUsize = AtomicUsize::new(0);

/* IDT создаётся один раз через spin::Once и живёт в static — load() безопасен */
static IDT: Once<InterruptDescriptorTable> = Once::new();

pub fn init_idt() {
    let idt = IDT.call_once(|| {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        // #DF на собственном стеке (IST 1): даже при переполнении основного
        // стека ядра обработчик получает свежий стек — не будет triple fault
        let df = idt.double_fault.set_handler_fn(double_fault_handler);
        // unsafe: set_stack_index не проверяет диапазон, но индексы 1..=3
        // гарантированы константами инициализации TSS в gdt::init_gdt()
        unsafe { df.set_stack_index(crate::gdt::DOUBLE_FAULT_IST_INDEX) };
        // IRQ0 — таймер (вектор 0x20, PIC offset), свой стек IST 2
        let t = idt[0x20].set_handler_fn(timer_handler);
        unsafe { t.set_stack_index(crate::gdt::TIMER_IST_INDEX) };
        // IRQ1 — клавиатура (вектор 0x21, PIC offset), свой стек IST 3
        let k = idt[0x21].set_handler_fn(keyboard_handler);
        unsafe { k.set_stack_index(crate::gdt::KEYBOARD_IST_INDEX) };
        // #PF — page fault, нужен для диагностики проблем с маппингами
        // в user mode (отсутствует обработчик → DOUBLE FAULT).
        // Собственный IST-стек (IST 4): #PF может возникнуть в ring 3,
        // когда TSS.rsp0 (kernel stack) может быть проблемным — IST
        // гарантирует валидный стек для обработчика независимо от RSP0.
        let pf = idt.page_fault.set_handler_fn(page_fault_handler);
        unsafe { pf.set_stack_index(crate::gdt::PAGE_FAULT_IST_INDEX) };
        // int 0x80 — syscall из user mode (ring 3). DPL=3 разрешает
        // вызов int 0x80 из ring 3. Без DPL=3 → #GP при int 0x80 из user.
        // IST не используется — CPU переключается на TSS.rsp0 (kernel stack).
        //
        // Обработчик — asm-обёртка syscall_entry (naked): она сохраняет
        // ВСЕ регистры общего назначения на стек и вызывает Rust-обработчик
        // syscall_handler_rust с указателем на SyscallRegisters. Это нужно,
        // потому что InterruptStackFrame содержит только RIP/CS/RFLAGS/RSP/SS,
        // а номер syscall (eax) и аргументы (rdi/rsi/rdx) живут в GPR.
        //
        // set_handler_addr принимает сырой адрес функции — CPU переходит на
        // неё напрямую (без пролога), а корректный стек-фрейм она строит сама.
        unsafe {
            idt[0x80]
                .set_handler_addr(VirtAddr::new(syscall_entry as u64))
                .set_privilege_level(PrivilegeLevel::Ring3);
        }
        idt
    });

    // Загружаем IDT
    idt.load();

    // Инициализируем PIC (ремапим на 0x20-0x2F)
    init_pic();

    // Включаем прерывания
    interrupts::enable();
}

extern "x86-interrupt" fn breakpoint_handler(
    _stack_frame: InterruptStackFrame,
) {
    crate::println!("EXCEPTION: BREAKPOINT");
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _err_code: u64,
) -> ! {
    // Диагностика: RSP — стек прерванного контекста, CR2 — адрес при #PF.
    // Если CR2 осмысленный (не 0) → было page fault, а не повреждение iretq.
    let cr2: u64;
    unsafe {
        core::arch::asm!(
            "mov {cr2}, %cr2",
            cr2 = out(reg) cr2,
            options(nomem, nostack, att_syntax)
        );
    }
    crate::println!(
        "EXCEPTION: DOUBLE FAULT (ticks={}, RIP: 0x{:x}, RSP: 0x{:x}, err: 0x{:x}, CR2: 0x{:x})",
        TIMER_TICKS.load(Ordering::Relaxed),
        stack_frame.instruction_pointer,
        stack_frame.stack_pointer,
        _err_code,
        cr2
    );
    // Прямая serial-диагностика без terminal — гарантирует вывод даже
    // если println (terminal/lock) недоступен.
    crate::serial::serial_write_str_direct("\n[DF] direct: RIP=0x");
    crate::serial::write_hex(stack_frame.instruction_pointer.as_u64());
    crate::serial::serial_write_str_direct(" CR2=0x");
    crate::serial::write_hex(cr2);
    crate::serial::serial_write_str_direct("\n");
    // Дамп IRET-фрейма, который iretq пытался вытолкнуть.
    // stack_frame.stack_pointer — это RSP на момент #DF; на нём (или рядом)
    // лежит фрейм прерванного/фейлящего iretq контекста.
    // Читаем 6 qword'ов: при iretq-#GP/#PF цепочка #GP→#NP накапливает
    // промежуточные фреймы — дамп покажет, что именно там лежит.
    let p = stack_frame.stack_pointer.as_u64() as *const u64;
    for i in 0..6 {
        let q: u64 = unsafe { *p.add(i) };
        crate::println!("  IRET frame[{}] = 0x{:016x}", i, q);
    }
    crate::println!(
        "  DF frame: CS: 0x{:x}, RFLAGS: 0x{:x}, SS: 0x{:x}",
        stack_frame.code_segment.0,
        stack_frame.cpu_flags.bits(),
        stack_frame.stack_segment.0
    );
    // SS/CS, которые видел обработчик таймера при входе (последний тик)
    crate::println!(
        "  timer entry: SS=0x{:x}, CS=0x{:x}",
        LAST_TIMER_SS.load(Ordering::Relaxed),
        LAST_TIMER_CS.load(Ordering::Relaxed)
    );
    // Вместо зависания (hlt loop) — восстанавливаемся в shell.
    // kernel_panic восстановит kernel CR3, переключится на чистый
    // kernel стек и вызовет shell::shell(). Пользователь продолжит
    // работу, а не перезагрузит систему вручную.
    crate::gdt::kernel_panic("Double fault — recovering to kernel shell");
}

/* Увеличен буфер с 16 до 256 символов — клавиатура генерирует прерывания быстрее обработки */
/* Используем heapless::Vec для safe static allocation без volatile */
pub static KEYBOARD_BUFFER: Mutex<Vec<u8, 256>> = Mutex::new(Vec::new());

const PIC1: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

pub fn init_pic() {
    // Полная инициализация ОБОИХ контроллеров (master + slave).
    // Ранее инициализировался только master с указанием каскада на IRQ2,
    // но slave (PIC2) не проходил свою последовательность ICW — каскад
    // оставался в неопределённом состоянии.
    // ICW1: сигнал инициализации (0x11 = ICW4 needed, cascade mode)
    outb(PIC1, 0x11);
    outb(PIC2, 0x11);
    // ICW2: базовые оффсеты векторов прерываний
    outb(PIC1_DATA, 0x20); // master: IRQ0-7 → 0x20-0x27
    outb(PIC2_DATA, 0x28); // slave: IRQ8-15 → 0x28-0x2F
    // ICW3: каскадирование
    outb(PIC1_DATA, 0x04); // master: slave подключён к IRQ2
    outb(PIC2_DATA, 0x02); // slave: каскадный ID = 2
    // ICW4: режим 8086
    outb(PIC1_DATA, 0x01);
    outb(PIC2_DATA, 0x01);
    // Маска master: 0xFC = 0b11111100 — разрешаем ТОЛЬКО IRQ0 (таймер)
    // и IRQ1 (клавиатура). Остальные маскируем: без маски после init
    // все IRQ открыты, а IRQ8 (RTC) периодически срабатывает сам по себе
    // → непокрытое прерывание → #GP → DOUBLE FAULT.
    outb(PIC1_DATA, 0xFC);
    // Маска slave: 0xFF — маскируем все IRQ 8-15 (в т.ч. RTC IRQ8,
    // spurious IRQ15). Обработчики для них не зарегистрированы.
    outb(PIC2_DATA, 0xFF);
    // EOI для обоих контроллеров
    outb(PIC1, 0x20);
    outb(PIC2, 0x20);
}

fn outb(port: u16, value: u8) {
    let mut port_obj = Port::<u8>::new(port);
    unsafe { port_obj.write(value); }
}

/* Значения SS/CS в момент входа в обработчик — диагностика повреждения фрейма */
static LAST_TIMER_SS: AtomicUsize = AtomicUsize::new(0);
static LAST_TIMER_CS: AtomicUsize = AtomicUsize::new(0);

extern "x86-interrupt" fn timer_handler(_stack_frame: InterruptStackFrame) {
    // Диагностика: сохраняем фактический SS/CS в момент входа в обработчик
    // (через API крейта — чтение сегментных регистров, без raw asm).
    LAST_TIMER_SS.store(SS::get_reg().0 as usize, Ordering::Relaxed);
    LAST_TIMER_CS.store(CS::get_reg().0 as usize, Ordering::Relaxed);
    // Счётчик тиков для диагностики DOUBLE FAULT
    TIMER_TICKS.fetch_add(1, Ordering::Relaxed);

    // Вытесняющий планировщик: уменьшаем квант текущего процесса.
    // Если квант истёк — нужен context_switch (пока диагностика).
    if crate::process_manager::scheduler_tick() {
        // Квант истёк — в будущем здесь будет переключение контекста.
        // Пока просто сбрасываем квант (процесс продолжает работать).
        crate::process_manager::reset_quantum_if_running();
    }

    // Отправляем EOI в PIC (port 0x20) — без этого таймер не выдаст следующее прерывание
    outb(PIC1, 0x20);
}

extern "x86-interrupt" fn keyboard_handler(_stack_frame: InterruptStackFrame) {
    let mut port = Port::<u8>::new(0x60);
    let scancode = unsafe { port.read() };
    
    let c = scancode_to_char(scancode);
    
    if let Some(c) = c {
        let mut buffer = KEYBOARD_BUFFER.lock();
        // FIFO: push добавляет в конец
        if buffer.push(c as u8).is_err() {
            // Если буфер переполнен — символ теряется, но не ломает систему
            crate::println!("Keyboard buffer overflow, dropped scancode: 0x{:02x}", scancode);
        }
    }
    
    outb(PIC1, 0x20);
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    err_code: PageFaultErrorCode,
) {
    let cr2: u64;
    unsafe {
        core::arch::asm!(
            "mov {cr2}, %cr2",
            cr2 = out(reg) cr2,
            options(nomem, nostack, att_syntax)
        );
    }
    // Прямая serial-диагностика (без println/terminal/lock): #PF может
    // возникнуть в контексте, где terminal или lock повреждены.
    crate::serial::serial_write_str_direct("\n[PF] direct: CR2=0x");
    crate::serial::write_hex(cr2);
    crate::serial::serial_write_str_direct(" err=0x");
    crate::serial::write_hex(err_code.bits() as u64);
    crate::serial::serial_write_str_direct(" RIP=0x");
    crate::serial::write_hex(stack_frame.instruction_pointer.as_u64());
    crate::serial::serial_write_str_direct(" CS=0x");
    crate::serial::write_hex(stack_frame.code_segment.0 as u64);
    crate::serial::serial_write_str_direct("\n");

    crate::println!(
        "PAGE FAULT (ticks={}, err=0x{:x}, RIP=0x{:x}, CR2=0x{:x})",
        TIMER_TICKS.load(Ordering::Relaxed),
        err_code.bits(),
        stack_frame.instruction_pointer,
        cr2
    );
    crate::println!(
        "  SS=0x{:x}, CS=0x{:x}, RFLAGS=0x{:x}",
        stack_frame.stack_segment.0,
        stack_frame.code_segment.0,
        stack_frame.cpu_flags.bits()
    );

    // Проверяем: если CS.RPL == 3 (user mode) — переключаем CR3
    // на user P4 процесса. Это нужно, потому что iretq выполняется
    // с kernel CR3 (ядро маппировано), а user code изолирован
    // в своей P4. После переключения — возвращаемся в user mode.
    let cs_rpl = stack_frame.code_segment.rpl();
    if cs_rpl == x86_64::PrivilegeLevel::Ring3 {
        crate::println!("  → User mode PF, CR3 switch needed");
        // В текущей реализации: run_process переходит через user_mode_entry,
        // который переключает CR3 ДО iretq к user code.
        // Если мы здесь — значит user_mode_entry не вызван или сломан.
        crate::gdt::kernel_panic("Unexpected page fault in user mode");
    }

    // Если мы в kernel mode — это реальная ошибка
    crate::gdt::kernel_panic("Page fault in kernel mode");
}

/// Сохранённые регистры общего назначения при входе в int 0x80.
///
/// Порядок полей = порядок на стеке после push в syscall_entry:
/// последний push (r15) оказывается по наименьшему адресу → первое поле.
/// Структура точно повторяет layout стека, поэтому syscall_handler_rust
/// читает регистры как обычные поля структуры.
#[repr(C)]
pub struct SyscallRegisters {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rax: u64,
}

/// asm-обёртка системного вызова int 0x80.
///
/// Порядок действий:
///   1. Сохраняет ВСЕ GPR на стек (push в порядке, обратном полям
///      SyscallRegisters — чтобы сохранённые регистры были одним блоком);
///   2. Передаёт указатель на них в Rust-обработчик syscall_handler_rust
///      (первый аргумент по SysV ABI — rdi);
///   3. После возврата восстанавливает регистры из стека (модифицированные
///      значения возвращаются на свои места — так syscall может передать
///      результат в user space через rax);
///   4. iretq — по (возможно, модифицированному) IRET-фрейму. Для возврата
///      в kernel mode обработчик меняет фрейм, и iretq переводит CPU в ring 0.
#[unsafe(naked)]
unsafe extern "C" fn syscall_entry() -> ! {
    core::arch::naked_asm!(
        "push rax",
        "push rcx",
        "push rdx",
        "push rbx",
        "push rbp",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        // rdi = указатель на SyscallRegisters (первый аргумент Rust-функции)
        "mov rdi, rsp",
        "call {handler}",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rbp",
        "pop rbx",
        "pop rdx",
        "pop rcx",
        "pop rax",
        "iretq",
        handler = sym syscall_handler_rust,
    );
}

// ---------------------------------------------------------------------------
// НОВЫЙ КОД (бирюзовый, правило .kodaruls п.6): безопасная работа с
// аргументами системных вызовов — проверка диапазонов в user space и
// копирование данных между user и kernel (аналог Linux copy_from_user /
// copy_to_user).
//
// ВАЖНО про адресное пространство: обработчик syscall (int 0x80) вызывается
// с активной CR3 = P4 процесса (CPU не меняет CR3 при прерывании). Поэтому
// paging::flags_at()/translate() проверяют адреса именно в адресном
// пространстве процесса, а запись/чтение по user-адресам работает
// напрямую. Всё это безопасно только благодаря предварительной проверке
// user_range_valid().
// ---------------------------------------------------------------------------

/// Canonical user space (lower half): бит 47 = 0.
/// Всё выше — higher-half ядра (и лимине-маппинги), user-буферы там
/// запрещены, даже если страница замаплена.
const USER_SPACE_TOP: u64 = 0x0000_8000_0000_0000;

/// Проверяет, что адрес лежит в canonical user space.
fn addr_in_user_space(addr: u64) -> bool {
    addr < USER_SPACE_TOP
}

/// Проверяет диапазон [addr, addr+len) в user space процесса:
/// - не выходит за границы canonical lower half;
/// - не переполняет u64 (addr + len);
/// - каждая затронутая страница замаплена (PRESENT) и доступна из ring 3
///   (USER_ACCESSIBLE);
/// - если `need_write` — страницы доступны на запись (WRITABLE).
///
/// Вызывается ДО копирования/записи — защита от передачи ядру адресов
/// kernel-памяти, VGA-буфера или unmapped областей (аналог access_ok +
/// проверка PTE в Linux).
fn user_range_valid(addr: u64, len: usize, need_write: bool) -> bool {
    if len == 0 {
        return true;
    }
    if !addr_in_user_space(addr) {
        return false;
    }
    let end = match addr.checked_add(len as u64) {
        Some(e) => e,
        None => return false,
    };
    // Идём по страницам: каждая должна быть замаплена с нужными флагами.
    let mut page = addr & !0xFFF;
    while page < end {
        let flags = match crate::paging::flags_at(page) {
            Some(f) => f,
            None => return false,
        };
        if !flags.contains(PageTableFlags::PRESENT)
            || !flags.contains(PageTableFlags::USER_ACCESSIBLE)
        {
            return false;
        }
        if need_write && !flags.contains(PageTableFlags::WRITABLE) {
            return false;
        }
        page += 0x1000;
    }
    true
}

/// Копирует `len` байт из user space (адрес `addr`) в kernel-буфер `dst`.
/// Возвращает false, если диапазон не валиден (копирование не выполнено).
fn copy_from_user(dst: *mut u8, addr: u64, len: usize) -> bool {
    if !user_range_valid(addr, len, false) {
        return false;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(addr as *const u8, dst, len);
    }
    true
}

/// Копирует `len` байт из kernel-буфера `src` в user space (адрес `addr`).
/// Возвращает false, если диапазон не валиден (копирование не выполнено).
fn copy_to_user(src: *const u8, addr: u64, len: usize) -> bool {
    if !user_range_valid(addr, len, true) {
        return false;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(src, addr as *mut u8, len);
    }
    true
}

/// Rust-обработчик системного вызова. Вызывается из asm-обёртки
/// syscall_entry, когда все регистры уже сохранены.
///
/// `regs` — указатель на сохранённые GPR в момент входа в int 0x80.
/// Сразу после структуры SyscallRegisters на стеке лежит
/// InterruptStackFrame (SS, RSP, RFLAGS, CS, RIP), вытолкнутый CPU
/// при входе в обработчик.
///
/// Для возврата в kernel mode (вместо возврата в user) мы:
/// 1. Восстанавливаем kernel CR3 (адресное пространство ядра)
/// 2. Модифицируем IRET-фрейм: CS/SS → kernel selectors,
///    RIP → kernel_return_to_shell, RSP → KERNEL_RETURN_STACK top
/// 3. iretq в syscall_entry возвращает в kernel mode
fn syscall_handler_rust(regs: &mut SyscallRegisters) {
    // InterruptStackFrame расположен сразу за SyscallRegisters на стеке.
    // unsafe: фрейм реально лежит там (его положил CPU перед push в
    // syscall_entry), выравнивание 8 байт у обеих структур совпадает.
    let frame = unsafe {
        let ptr = (regs as *mut SyscallRegisters as *mut u8)
            .add(core::mem::size_of::<SyscallRegisters>());
        &mut *(ptr as *mut InterruptStackFrame)
    };

    // Номер системного вызова — в eax/rax (как в Linux: eax = номер syscall).
    let syscall_num = (regs.rax & 0xFFFF_FFFF) as u32;
    // Аргументы syscall — в rdi/rsi/rdx (SysV ABI).
    let arg1 = regs.rdi;
    let arg2 = regs.rsi;
    let arg3 = regs.rdx;

    // Диагностика. ВАЖНО: для read (#0) НЕ печатаем — read блокирующий,
    // и до нажатия клавиши вызывается многократно. Иначе лог/терминал
    // захлёстываются выводом, а сбор данных через nc замедляется.
    if syscall_num != 0 {
        crate::println!(
            "SYSCALL #{} from ring 3! (arg1=0x{:016x}, arg2=0x{:016x}, arg3=0x{:016x})",
            syscall_num, arg1, arg2, arg3
        );
        crate::println!(
            "  user RIP: 0x{:x}, RSP: 0x{:x}, CS: 0x{:x}, RFLAGS: 0x{:x}",
            frame.instruction_pointer,
            frame.stack_pointer,
            frame.code_segment.0,
            frame.cpu_flags.bits()
        );
    }

    // Диспетчер системных вызовов по номеру (eax).
    // Номера заимствованы у Linux (x86_64):
    //   0  — read(fd, buf, count) — чтение из stdin (блокирующее)
    //   1  — write(fd, buf, len) — вывод строки из user space
    //   60 — exit(code) — завершение процесса (возврат в kernel shell)
    //
    // ВАЖНО про контекст: обработчик вызван из int 0x80, а CPU при входе
    // в обработчик НЕ меняет CR3 — он всё ещё указывает на P4 процесса.
    // Поэтому user-адреса проверяются через paging::flags_at() ПО АКТИВНОЙ
    // P4 (user_range_valid = аналог access_ok), а сами данные копируются
    // между user и kernel через copy_from_user/copy_to_user — ядро НЕ работает
    // с user-буферами напрямую после проверки (защита от подмены
    // страниц во время копирования).
    match syscall_num {
        0 => {
            // read(fd, buf, count) — чтение из stdin (fd=0).
            // Реализация БЛОКИРУЮЩАЯ: ждём байт из буфера клавиатуры (IRQ1)
            // + COM1 (serial). Если данных нет вообще — засыпаем (hlt с IF=1;
            // RFLAGS из user mode 0x246: прерывания разрешены), просыпаемся
            // по таймеру/клавиатуре и проверяем снова, пока не появится байт.
            let fd = arg1;
            let buf = arg2 as *mut u8;
            let count = arg3 as usize;

            if fd != 0 {
                // Поддерживаем только stdin (fd=0)
                crate::println!("  read: unsupported fd {}", fd);
                regs.rax = 0xFFFF_FFFF_FFFF_FFFF; // -1: EBADF
            } else if count == 0 {
                regs.rax = 0;
            } else {
                // Ограничение: за один read выдаём не более 256 байт
                // (размер kernel-буфера ниже).
                let count = count.min(256);

                // 1. Проверяем, что user-буфер вообще можно писать.
                if !user_range_valid(buf as u64, count, true) {
                    crate::println!(
                        "  read: invalid user buffer at 0x{:x}",
                        buf as u64
                    );
                    regs.rax = 0xFFFF_FFFF_FFFF_FFFF; // -1: EFAULT
                } else {
                    // 2. Блокирующий сбор данных в kernel-буфер.
                    //    Источники: буфер клавиатуры (IRQ1) + COM1 (serial).
                    //    Если данных нет вообще — засыпаем (hlt) до
                    //    следующего прерывания (IRQ0/IRQ1 пробуждают CPU,
                    //    обработчики работают на IST-стеках), затем снова
                    //    проверяем ввод. Цикл прервётся, когда появится
                    //    хотя бы один байт.
                    let mut kbuf = [0u8; 256];
                    let mut n = 0usize;

                    while n == 0 {
                        {
                            let mut kb = KEYBOARD_BUFFER.lock();
                            while n < count {
                                match kb.first() {
                                    Some(b) => {
                                        kbuf[n] = *b;
                                        kb.remove(0);
                                        n += 1;
                                    }
                                    None => break,
                                }
                            }
                        }
                        while n < count && crate::serial::serial_has_data() {
                            kbuf[n] = crate::serial::serial_read();
                            n += 1;
                        }

                        if n == 0 {
                            // Нет данных — спим. КРИТИЧЕСКИ: int 0x80
                            // сбрасывает IF (прерывания запрещены внутри
                            // обработчика). hlt с IF=0 НЕ просыпается от
                            // maskable interrupts (таймер, клавиатура) —
                            // процесс зависнет навсегда. Поэтому СНАЧАЛА
                            // sti (разрешаем прерывания — они станут
                            // доставляться начиная со следующей инструкции),
                            // ЗАТЕМ hlt (если прерывание уже pending, оно
                            // сработает до hlt; иначе hlt ждёт следующего).
                            // Это стандартный safe pattern sti; hlt.
                            unsafe {
                                core::arch::asm!(
                                    "sti",
                                    "hlt",
                                    options(nomem, nostack, preserves_flags)
                                );
                            }
                        }
                    }

                    // 3. Копируем в user space. Диапазон уже проверен выше,
                    //    поэтому copy_to_user должна успешно завершиться.
                    if copy_to_user(kbuf.as_ptr(), buf as u64, n) {
                        regs.rax = n as u64; // прочитано байт
                    } else {
                        regs.rax = 0xFFFF_FFFF_FFFF_FFFF; // -1: EFAULT
                    }
                }
            }
        }
        1 => {
            // write(fd, buf, len)
            let fd = arg1;
            let buf = arg2 as *const u8;
            let len = arg3 as usize;

            if fd != 1 {
                // Поддерживаем только stdout (fd=1)
                crate::println!("  write: unsupported fd {}", fd);
                regs.rax = 0xFFFF_FFFF_FFFF_FFFF; // -1: EBADF
            } else {
                // Ограничение на длину: не больше 4096 байт (одна страница),
                // чтобы копирование/вывод не выполнялись бесконечно при
                // неверном (огромном) len из user space.
                let len = len.min(4096);

                // Копируем буфер из user space в kernel-буфер.
                // copy_from_user проверяет ВСЮ область (каждую страницу):
                // canonical lower-half, PRESENT, USER_ACCESSIBLE.
                // После успешной копии ядро работает только со своим
                // буфером — страницы user-процесса больше не читаются.
                let mut kbuf = [0u8; 4096];
                if !copy_from_user(kbuf.as_mut_ptr(), buf as u64, len) {
                    crate::println!(
                        "  write: invalid user buffer at 0x{:x}",
                        buf as u64
                    );
                    regs.rax = 0xFFFF_FFFF_FFFF_FFFF; // -1: EFAULT
                } else {
                    // Выводим байты из kernel-буфера. Непечатные символы
                    // (кроме \n \r 	) заменяем на '.' — бинарные данные
                    // не должны ломать терминал/серийный протокол.
                    for i in 0..len {
                        let b = kbuf[i];
                        let c = match b {
                            b'\n' | b'\r' | 0x09 | 0x20..=0x7e => b as char,
                            _ => '.',
                        };
                        crate::print!("{}", c);
                    }
                    regs.rax = len as u64; // возвращаем число записанных байт
                }
            }
        }
        60 => {
            // exit(code) — завершение процесса и возврат в kernel shell.
            // НЕ переходим в user mode: модифицируем IRET-фрейм и CR3 так,
            // чтобы iretq после syscall_entry вернул нас в kernel.
            let code = arg1;
            crate::println!(
                "  exit({}) — процесс завершён, возврат в kernel shell",
                code
            );
            regs.rax = 0; // успешное завершение syscall

            // 1. Восстанавливаем kernel CR3 (сейчас активна user P4 процесса)
            let kernel_p4 = crate::paging::kernel_p4_phys();
            if kernel_p4 != 0 {
                unsafe {
                    let frame = PhysFrame::containing_address(PhysAddr::new(kernel_p4));
                    Cr3::write(frame, Cr3Flags::empty());
                }
            }

            // 2. Модифицируем IRET-фрейм для возврата в kernel mode.
            //    ВАЖНО: это сопряжено с потерей user-стека/контекста —
            //    процесс завершается, его P4/фреймы остаются (утечка в
            //    учебном ядре; корректное освобождение — отдельная задача).
            //    Текущий kernel-стек (KERNEL_RETURN_STACK) — валиден.
            unsafe {
                let mut m = frame.as_mut();
                m.update(|f| {
                    // RIP → kernel_return_to_shell (возвращаемся в shell)
                    f.instruction_pointer =
                        VirtAddr::new(kernel_return_to_shell as *const () as u64);
                    // CS → kernel code selector (RPL=0, ring 0)
                    f.code_segment = SegmentSelector::new(
                        crate::gdt::KERNEL_CODE_SELECTOR >> 3,
                        PrivilegeLevel::Ring0,
                    );
                    // SS → kernel data selector (RPL=0)
                    f.stack_segment = SegmentSelector::new(
                        crate::gdt::KERNEL_DATA_SELECTOR >> 3,
                        PrivilegeLevel::Ring0,
                    );
                    // RSP → вершина KERNEL_RETURN_STACK (валидный kernel стек)
                    let stack_ptr =
                        core::ptr::addr_of!(crate::gdt::KERNEL_RETURN_STACK) as *const u8;
                    f.stack_pointer = VirtAddr::new(
                        stack_ptr as u64 + crate::gdt::KERNEL_RETURN_STACK.len() as u64
                    );
                });
            }
            // После возврата в syscall_entry pop'ы восстановят регистры,
            // затем iretq использует модифицированный фрейм: CPU переходит
            // в ring 0 на kernel_return_to_shell с kernel стеком.
        }
        _ => {
            crate::println!("  Unknown syscall number: {}", syscall_num);
            regs.rax = 0xFFFF_FFFF_FFFF_FFFF; // -1: неверный номер
        }
    }

    // ВАЖНО: обычные syscall (read/write) НЕ меняют CR3 и IRET-фрейм —
    // процесс продолжает работать в user mode. CPU не переключает CR3 при
    // входе в int 0x80, поэтому после iretq в syscall_entry мы окажемся
    // на прежнем месте в user code с той же P4 (контекст процесса цел).
    // Только exit() модифицирует фрейм и возвращает в kernel (см. выше).
}

/// Точка возврата из user mode в kernel.
/// Вызывается после iretq из syscall_handler (через модифицированный IRET-фрейм).
/// Перезапускает shell — пользователь может продолжить работу.
#[inline(never)]
fn kernel_return_to_shell() -> ! {
    crate::println!("Returned from user mode to kernel");
    crate::shell::shell();
    loop {}
}

fn scancode_to_char(scancode: u8) -> Option<char> {
    // Управляющие клавиши — специальные коды (не печатные символы).
    // Shell различает их по этим значениям.
    match scancode {
        // Enter
        0x1C => return Some('\n'),
        // Backspace
        0x0E => return Some('\u{8}'),
        // Escape
        0x01 => return Some('\u{1b}'),
        // Tab
        0x0F => return Some('\u{9}'),
        _ => {}
    }
    // Печатные символы US-раскладки (без Shift). Нижний регистр.
    let c = match scancode {
        0x1E => 'a', 0x30 => 'b', 0x2E => 'c', 0x20 => 'd', 0x12 => 'e',
        0x21 => 'f', 0x22 => 'g', 0x23 => 'h', 0x17 => 'i', 0x24 => 'j',
        0x25 => 'k', 0x26 => 'l', 0x32 => 'm', 0x31 => 'n', 0x18 => 'o',
        0x19 => 'p', 0x10 => 'q', 0x13 => 'r', 0x1F => 's', 0x14 => 't',
        0x16 => 'u', 0x2F => 'v', 0x11 => 'w', 0x2D => 'x', 0x15 => 'y',
        0x2C => 'z',
        0x02 => '1', 0x03 => '2', 0x04 => '3', 0x05 => '4', 0x06 => '5',
        0x07 => '6', 0x08 => '7', 0x09 => '8', 0x0A => '9', 0x0B => '0',
        // Знаки препинания (без Shift)
        0x29 => '`', 0x0C => '-', 0x0D => '=', 0x1A => '[', 0x1B => ']',
        0x2B => '\\', 0x27 => ';', 0x28 => '\'', 0x33 => ',', 0x34 => '.',
        0x35 => '/',
        // Пробел
        0x39 => ' ',
        _ => return None,
    };
    Some(c)
}
