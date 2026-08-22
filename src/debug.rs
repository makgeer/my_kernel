//! Отладочный модуль ядра (правило .kodaruls п.11).
//!
//! Сюда монтируется весь отладочный код: диагностический вывод,
//! вспомогательные функции для проверки подсистем. Исключение —
//! проверка функций самого ядра (см. src/tests.rs).
//!
//! Функции здесь НЕ должны зависеть от терминала/lock — они
//! используются в обработчиках исключений (#PF/#DF), где println!
//! может сам вызвать page fault / deadlock.

/// Запись 64-битного числа в 16-ричном виде (без префикса 0x).
/// Прямой посимвольный вывод без fmt/alloc — безопасен даже если
/// terminal/lock повреждены (используется в #PF/#DF диагностике).
pub fn write_hex(value: u64) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut written = false;
    for shift in (0..64).step_by(4).rev() {
        let nibble = ((value >> shift) & 0xF) as usize;
        if nibble != 0 || shift == 0 {
            crate::serial::serial_write_byte(HEX[nibble]);
            written = true;
        } else if written {
            crate::serial::serial_write_byte(HEX[nibble]);
        }
    }
}

/// Прямая диагностическая строка в COM1 без fmt/alloc и без lock.
/// Используется в обработчиках исключений (#PF/#DF), где println!
/// (terminal + lock) может сам вызвать page fault / deadlock.
pub fn serial_write_str_direct(s: &str) {
    for b in s.bytes() {
        match b {
            b'\n' => {
                crate::serial::serial_write_byte(b'\r');
                crate::serial::serial_write_byte(b'\n');
            }
            _ => crate::serial::serial_write_byte(b),
        }
    }
}
