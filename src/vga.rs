use core::fmt;
use spin::Mutex;

const VGA_WIDTH: usize = 80;
const VGA_HEIGHT: usize = 25;
const VGA_COLOR: u8 = 0x0f;

pub struct VgaWriter {
    row: usize,
    col: usize,
    buffer: *mut u8,
}

unsafe impl Send for VgaWriter {}

impl VgaWriter {
    pub const fn new() -> Self {
        VgaWriter {
            row: 0,
            col: 0,
            buffer: 0xb8000 as *mut u8,
        }
    }

    fn write_char(&mut self, c: char) {
        match c {
            '\n' => {
                self.row += 1;
                self.col = 0;
                if self.row >= VGA_HEIGHT {
                    self.scroll();
                }
            }
            '\r' => {
                self.col = 0;
            }
            c => {
                if self.col >= VGA_WIDTH {
                    self.col = 0;
                    self.row += 1;
                    if self.row >= VGA_HEIGHT {
                        self.scroll();
                    }
                }
                let pos = (self.row * VGA_WIDTH + self.col) * 2;
                unsafe {
                    *self.buffer.offset(pos as isize) = c as u8;
                    *self.buffer.offset(pos as isize + 1) = VGA_COLOR;
                }
                self.col += 1;
            }
        }
    }

    fn scroll(&mut self) {
        unsafe {
            let src = self.buffer.offset((VGA_WIDTH * 2) as isize);
            let dst = self.buffer;
            core::ptr::copy(src, dst, VGA_WIDTH * (VGA_HEIGHT - 1) * 2);
            let last = self.buffer.offset((VGA_WIDTH * (VGA_HEIGHT - 1) * 2) as isize);
            core::ptr::write_bytes(last, 0, VGA_WIDTH * 2);
        }
        self.row = VGA_HEIGHT - 1;
        self.col = 0;
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        unsafe {
            core::ptr::write_bytes(self.buffer, 0, VGA_WIDTH * VGA_HEIGHT * 2);
        }
        self.row = 0;
        self.col = 0;
    }

    pub fn print(&mut self, s: &str) {
        for c in s.chars() {
            self.write_char(c);
        }
    }
}

impl fmt::Write for VgaWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.print(s);
        Ok(())
    }
}

pub static WRITER: Mutex<VgaWriter> = Mutex::new(VgaWriter::new());

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        // Интеграция терминала: единый вывод через terminal::TERMINAL
        // вместо трёх независимых write!. Терминал сам разбирает ANSI-коды
        // (цвета, курсор, прокрутка) и рендерит во все три устройства
        // (VGA + Serial + Framebuffer) через flush(). Старые цели (WRITER,
        // SERIAL, VesaWriter) остаются в коде — не удаляются, но больше
        // не вызываются из print!.
        let _ = write!($crate::terminal::TERMINAL.lock(), $($arg)*);
    }};
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}
