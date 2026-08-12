use heapless::Vec;
use spin::Mutex;
use spin::Once;
use x86_64::instructions::port::Port;
use x86_64::instructions::interrupts;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

/* IDT создаётся один раз через spin::Once и живёт в static — load() безопасен */
static IDT: Once<InterruptDescriptorTable> = Once::new();

pub fn init_idt() {
    let idt = IDT.call_once(|| {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.double_fault.set_handler_fn(double_fault_handler);
        // IRQ0 — таймер (вектор 0x20, PIC offset)
        idt[0x20].set_handler_fn(timer_handler);
        // IRQ1 — клавиатура (вектор 0x21, PIC offset)
        idt[0x21].set_handler_fn(keyboard_handler);
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
    _stack_frame: InterruptStackFrame,
    _err_code: u64,
) -> ! {
    crate::println!("EXCEPTION: DOUBLE FAULT");
    loop {
        unsafe { core::arch::asm!("hlt"); }
    }
}

/* Увеличен буфер с 16 до 256 символов — клавиатура генерирует прерывания быстрее обработки */
/* Используем heapless::Vec для safe static allocation без volatile */
pub static KEYBOARD_BUFFER: Mutex<Vec<u8, 256>> = Mutex::new(Vec::new());

const PIC1: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;

pub fn init_pic() {
    outb(PIC1, 0x11);
    outb(PIC1_DATA, 0x20);
    outb(PIC1_DATA, 0x04);
    outb(PIC1_DATA, 0x01);
    outb(PIC1, 0x20);
}

fn outb(port: u16, value: u8) {
    let mut port_obj = Port::<u8>::new(port);
    unsafe { port_obj.write(value); }
}

extern "x86-interrupt" fn timer_handler(_stack_frame: InterruptStackFrame) {
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
    match scancode {
        0x02 => Some('1'),
        0x03 => Some('2'),
        0x04 => Some('3'),
        0x05 => Some('4'),
        0x06 => Some('5'),
        0x07 => Some('6'),
        0x08 => Some('7'),
        0x09 => Some('8'),
        0x0A => Some('9'),
        0x0B => Some('0'),
        _ => None,
    }
}
