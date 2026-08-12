// Драйвер UART 16550 (COM1) для отладки ядра через последовательный порт.
// Позволяет видеть весь вывод ядра в файле (qemu.log) независимо
// от видеорежима, который включает загрузчик Limine.

use core::fmt;
use spin::Mutex;
use x86_64::instructions::port::Port;

/// Адрес порта COM1 — стандартный первый последовательный порт
pub const COM1: u16 = 0x3F8;

/// Инициализация UART 16550 (последовательность из OSDev Wiki / Blog OS)
pub fn init() {
    unsafe {
        // Отключаем прерывания UART
        Port::<u8>::new(COM1 + 1).write(0x00);
        // DLAB = 1 — переходим к установке делителя частоты
        Port::<u8>::new(COM1 + 3).write(0x80);
        // Делитель 0x03 — скорость 38400 бод
        Port::<u8>::new(COM1 + 0).write(0x03);
        Port::<u8>::new(COM1 + 1).write(0x00);
        // DLAB = 0, 8 бит данных, без чётности, 1 стоп-бит
        Port::<u8>::new(COM1 + 3).write(0x03);
        // Включаем FIFO: очистка очередей, буфер 14 байт
        Port::<u8>::new(COM1 + 2).write(0xC7);
        // DTR, RTS, OUT2 — сигналы готовности модема
        Port::<u8>::new(COM1 + 4).write(0x0B);
    }
}

/// Ожидание освобождения буфера передатчика и запись одного байта
fn write_byte(byte: u8) {
    unsafe {
        // Бит 0x20 регистра состояния линии = передатчик готов
        while Port::<u8>::new(COM1 + 5).read() & 0x20 == 0 {}
        Port::<u8>::new(COM1).write(byte);
    }
}

/// Запись строки в COM1 (переводит '\n' в CRLF для корректного отображения)
pub fn write_str(s: &str) {
    for b in s.bytes() {
        match b {
            b'\n' => {
                write_byte(b'\r');
                write_byte(b'\n');
            }
            _ => write_byte(b),
        }
    }
}

/// Обёртка для использования в макросах через fmt::Write
pub struct SerialWriter;

impl fmt::Write for SerialWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        write_str(s);
        Ok(())
    }
}

/// Статик для макросов print!/println!
pub static SERIAL: Mutex<SerialWriter> = Mutex::new(SerialWriter);
