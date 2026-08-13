use heapless::Vec;
use spin::Mutex;
use spin::Once;
use x86_64::instructions::port::Port;
use x86_64::instructions::segmentation::{CS, SS, Segment};
use x86_64::instructions::interrupts;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};
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
    loop {
        unsafe { core::arch::asm!("hlt"); }
    }
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
