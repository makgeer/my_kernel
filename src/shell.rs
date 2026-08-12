use crate::interrupts::KEYBOARD_BUFFER;
use crate::println;

pub fn shell() {
    let mut input_buf = [0u8; 64];
    let mut pos = 0;

    loop {
        println!("\n> ");
        pos = 0;

        // Ждём символ из буфера (FIFO — берём с начала)
        loop {
            let mut buffer = KEYBOARD_BUFFER.lock();
            if let Some(byte) = buffer.first() {
                let c = *byte as char;
                if c == '\n' {
                    println!("\n");
                    buffer.remove(0);
                    break;
                }
                if pos < input_buf.len() - 1 {
                    input_buf[pos] = *byte;
                    pos += 1;
                    print!("{}", c);
                }
                buffer.remove(0);
            }
            drop(buffer);
        }

        // Обработка команды
        let cmd = core::str::from_utf8(&input_buf[..pos]).unwrap_or("");
        match cmd {
            "help" => println!("Commands: help, echo, clear"),
            _ => println!("Unknown command"),
        }
    }
}