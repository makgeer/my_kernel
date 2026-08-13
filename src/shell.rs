use crate::interrupts::KEYBOARD_BUFFER;
use crate::print;
use crate::println;

/// Ожидает один символ из буфера клавиатуры (блокирующее чтение).
/// Возвращает (символ, источник):
///   true  = из serial (COM1)
///   false = из PS/2 клавиатуры
pub fn getchar() -> (char, bool) {
    loop {
        // Источник 1: PS/2 клавиатура (IRQ1)
        {
            let mut buffer = KEYBOARD_BUFFER.lock();
            if let Some(byte) = buffer.first() {
                let c = *byte as char;
                buffer.remove(0);
                return (c, false);
            }
        }
        // Источник 2: serial (COM1) — ввод через -serial stdio.
        // QEMU переводит терминал в raw mode (нет локального эха),
        // поэтому символы выводим сами в read_line().
        if crate::serial::serial_has_data() {
            let byte = crate::serial::serial_read();
            return (byte as char, true);
        }
        // Небольшая пауза, чтобы не раскручивать CPU вхолостую
        for _ in 0..1000 {
            unsafe { core::arch::asm!("nop"); }
        }
    }
}

/// Читает строку до Enter. Возвращает набранные символы.
/// Поддерживает Backspace (стирание последнего символа).
/// Эхо печатается ВСЕГДА (и для serial, и для PS/2): QEMU с
/// -serial stdio переводит терминал в raw mode, где нет локального эха,
/// поэтому символы выводим сами через print! (VGA+serial+framebuffer).
pub fn read_line() -> [u8; 64] {
    let mut input_buf = [0u8; 64];
    let mut pos = 0;

    loop {
        let (c, _from_serial) = getchar();
        match c {
            // Enter — конец строки (serial может прислать \r или \n)
            '\n' | '\r' => {
                println!();
                break;
            }
            // Backspace (0x08 или 0x7F DEL) — стираем последний символ
            '\u{8}' | '\u{7f}' => {
                if pos > 0 {
                    pos -= 1;
                    // Печатаем пробел поверх и откатываем курсор:
                    // \x08 — назад, пробел — затирает, \x08 — снова назад.
                    crate::print!("\u{8} \u{8}");
                }
            }
            // Обычный печатный символ — добавляем в буфер и эхо-сим
            c => {
                if pos < input_buf.len() - 1 && c.is_ascii() {
                    input_buf[pos] = c as u8;
                    pos += 1;
                    crate::print!("{}", c);
                }
            }
        }
    }
    input_buf
}

pub fn shell() {
    println!();
    println!("Welcome to MyKernel shell!");
    println!("Type 'help' for commands");

    loop {
        print!("> ");
        let input_buf = read_line();
        let cmd = core::str::from_utf8(&input_buf)
            .unwrap_or("")
            .trim_end_matches('\0')
            .trim();

        match cmd {
            "help" => {
                println!("Commands:");
                println!("  help    - show this help");
                println!("  echo    - echo a line");
                println!("  clear   - clear the screen");
                println!("  reboot  - reboot the system");
                println!("  hlt     - halt the CPU");
                println!("  about   - about this kernel");
                println!("  tasks   - run the task scheduler demo");
            }
            "about" => {
                println!("MyKernel v0.1.0");
                println!("x86_64 OS kernel written in Rust");
                println!("Bootloader: Limine");
            }
            "tasks" | "task" => {
                // Запускаем одну итерацию планировщика как демонстрацию
                crate::scheduler::run_scheduler_once();
            }
            "clear" => {
                crate::vesa::vesa_clear_screen(0x000000);
                crate::vga::WRITER.lock().clear();
            }
            "" => {
                // Пустая команда — просто перенос строки
            }
            _ => {
                if cmd.starts_with("echo ") {
                    println!("{}", &cmd[5..]);
                } else {
                    println!("Unknown command: {}", cmd);
                }
            }
        }
    }
}