// Драйвер UART 16550 (COM1) для отладки ядра через последовательный порт.
// Позволяет видеть весь вывод ядра в файле (bochs_serial.log) независимо
// от видеорежима, который включает загрузчик Limine.

use core::fmt;
use spin::Mutex;
use x86_64::instructions::port::Port;

/// Адрес порта COM1 — стандартный первый последовательный порт
pub const COM1: u16 = 0x3F8;

/// Инициализация UART 16550 (последовательность из OSDev Wiki / Blog OS)
pub fn init() {
    unsafe {
        // Отключаем прерывания UART (сброс)
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
        // Включаем прерывания приёма (RDI, bit 0) — UART генерирует IRQ4
        Port::<u8>::new(COM1 + 1).write(0x01);
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

/// Есть ли принятый байт в COM1?
/// LSR (порт 0x3FD) бит 0 = Data Ready — в буфере есть непрочитанный байт.
pub fn serial_has_data() -> bool {
    unsafe {
        let lsr = Port::<u8>::new(COM1 + 5).read();
        (lsr & 0x01) != 0
    }
}

/// Читает один байт из COM1. Перед вызовом проверять serial_has_data()!
pub fn serial_read() -> u8 {
    unsafe {
        Port::<u8>::new(COM1).read()
    }
}

/// Считывает байт из COM1, если данные есть (возвращает Option).
/// Не блокирует — возвращает None, если данных нет.
pub fn serial_read_nonblocking() -> Option<u8> {
    if serial_has_data() {
        Some(serial_read())
    } else {
        None
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

/// Запись одного «сырого» байта в COM1 без преобразования.
/// Нужна терминалу для отправки ESC (0x1B) и произвольных байтов напрямую.
pub fn serial_write_byte(byte: u8) {
    write_byte(byte);
}

/// Запись неотрицательного числа (0..=999) в десятичном виде поразрядно.
/// Используется терминалом для ANSI SGR-цветов и позиции курсора,
/// где нельзя форматировать строки (нет alloc).
pub fn serial_write_num(n: u16) {
    if n == 0 {
        write_byte(b'0');
        return;
    }
    // Число <= 999 — не более трёх цифр. Пишем старший разряд первым.
    let hundreds = (n / 100) % 10;
    let tens = (n / 10) % 10;
    let ones = n % 10;
    if hundreds != 0 {
        write_byte(b'0' + hundreds as u8);
        write_byte(b'0' + tens as u8);
    } else if tens != 0 {
        write_byte(b'0' + tens as u8);
    }
    write_byte(b'0' + ones as u8);
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
