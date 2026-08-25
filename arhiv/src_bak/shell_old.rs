// СТАРЫЙ КОД shell.rs - перенесён из arhiv/src_bak/shell_old.rs
// Дата переноса: 2026-08-25
// Причина: заменён на новую реализацию с read_key() и PS/2 scancodes

use crate::interrupts::KEYBOARD_BUFFER;
use crate::print;
use crate::println;

/// История команд — кольцевой буфер последних 25 команд.
/// Код выделен бирюзовым цветом (правило .kodaruls п.6).
const HISTORY_MAX: usize = 25;
const CMD_LEN: usize = 64;
static mut HISTORY: [[u8; CMD_LEN]; HISTORY_MAX] = [[0u8; CMD_LEN]; HISTORY_MAX];
static mut HISTORY_COUNT: usize = 0;
static mut HISTORY_HEAD: usize = 0; // индекс куда писать следующую команду

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
        // Bochs переводит терминал в raw mode (нет локального эха),
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
/// Поддерживает Backspace и историю команд (стрелки вверх/вниз).
/// Эхо печатается ВСЕГДА (и для serial, и для PS/2).
///
/// Код выделен бирюзовым цветом (правило .kodaruls п.6) — добавлена
/// история команд с навигацией по стрелкам.
pub fn read_line() -> [u8; 64] {
    let mut input_buf = [0u8; 64];
    let mut pos = 0;
    let mut input_len = 0;

    // Индекс для навигации по истории. start = HISTORY_COUNT означает
    // "новая пустая строка". При нажатии вверх уменьшаем, вниз — увеличиваем.
    let mut hist_idx: usize;

    unsafe {
        hist_idx = HISTORY_COUNT;
    }

    loop {
        let (c, _from_serial) = getchar();

        // Обработка escape-последовательностей (стрелки).
        // ESC [ A = вверх, ESC [ B = вниз, ESC [ C = вправо, ESC [ D = влево.
        // ВАЖНО: при вводе через COM1 (serial) нужно читать следующие
        // байты напрямую из serial-порта, минуя getchar(), который
        // проверяет PS/2 буфер ПЕРВЫМ — если между ESC и '['/'A'
        // сработает IRQ клавиатуры, PS/2 буфер не пуст и getchar()
        // вернёт PS/2-байт вместо serial-байта → escape не распознается.
        if c == '\u{1b}' {
            // Обработка escape-последовательностей (стрелки).
            // ESC [ A = вверх, ESC [ B = вниз, ESC [ C = вправо, ESC [ D = влево.
            // ВАЖНО: проверяем ОБА источника — serial и PS/2.
            // При serial-вводе: ESC и '['/'A' приходят из serial_read_nonblocking().
            // При PS/2-вводе: ESC уже в 'c', '['/'A' — в KEYBOARD_BUFFER.
            let (c2, c3) = if let Some(b1) = crate::serial::serial_read_nonblocking() {
                // Serial-ввод: ESC и следующие байты из serial
                if b1 == b'[' {
                    for _ in 0..5000 {
                        unsafe { core::arch::asm!("nop"); }
                    }
                    if let Some(b2) = crate::serial::serial_read_nonblocking() {
                        (b1 as char, b2 as char)
                    } else {
                        ('[', '\0')
                    }
                } else {
                    ('[', '\0')
                }
            } else {
                // PS/2-ввод: ESC уже в 'c', проверяем KEYBOARD_BUFFER
                let mut buffer = KEYBOARD_BUFFER.lock();
                let b1 = buffer.first().copied();
                if b1 == Some(b'[') {
                    buffer.remove(0);
                    let b2 = buffer.first().copied();
                    ('[', b2.unwrap_or(0) as char)
                } else {
                    ('[', '\0')
                }
            };
            if c2 == '[' {
                match c3 {
                    'A' => {
                        // Стрелка вверх — предыдущая команда из истории
                        unsafe {
                            if HISTORY_COUNT == 0 || hist_idx == 0 {
                                continue; // истории нет или уже в начале
                            }
                            hist_idx -= 1;
                            // cmd_idx — реальный индекс в массиве HISTORY
                            let cmd_idx = history_index(hist_idx);
                            // Очищаем текущий ввод
                            while pos > 0 {
                                pos -= 1;
                                crate::print!("\u{8} \u{8}");
                            }
                            input_len = 0;
                            // Копируем команду из истории
                            for i in 0..CMD_LEN {
                                if HISTORY[cmd_idx][i] == 0 { break; }
                                if pos < input_buf.len() - 1 {
                                    input_buf[pos] = HISTORY[cmd_idx][i];
                                    pos += 1;
                                    input_len += 1;
                                    crate::print!("{}", HISTORY[cmd_idx][i] as char);
                                }
                            }
                        }
                        continue;
                    }
                    'B' => {
                        // Стрелка вниз — следующая команда (или пустая строка)
                        unsafe {
                            if hist_idx >= HISTORY_COUNT {
                                // Новая пустая строка
                                while pos > 0 {
                                    pos -= 1;
                                    crate::print!("\u{8} \u{8}");
                                }
                                input_len = 0;
                                continue;
                            }
                            hist_idx += 1;
                            // Очищаем текущий ввод
                            while pos > 0 {
                                pos -= 1;
                                crate::print!("\u{8} \u{8}");
                            }
                            input_len = 0;
                            if hist_idx < HISTORY_COUNT {
                                let cmd_idx = history_index(hist_idx);
                                for i in 0..CMD_LEN {
                                    if HISTORY[cmd_idx][i] == 0 { break; }
                                    if pos < input_buf.len() - 1 {
                                        input_buf[pos] = HISTORY[cmd_idx][i];
                                        pos += 1;
                                        input_len += 1;
                                        crate::print!("{}", HISTORY[cmd_idx][i] as char);
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    'D' => {
                        // Стрелка влево — переместить курсор на символ влево
                        if pos > 0 {
                            pos -= 1;
                            crate::print!("\u{8}");
                        }
                        continue;
                    }
                    'C' => {
                        // Стрелка вправо — переместить курсор на символ вправо
                        if pos < input_len {
                            crate::print!("{}", input_buf[pos] as char);
                            pos += 1;
                        }
                        continue;
                    }
                    _ => {
                        // Другие escape-последовательности — игнорируем
                        continue;
                    }
                }
            }
            // ESC без '[' — игнорируем
            continue;
        }

        match c {
            // Enter — конец строки
            '\n' | '\r' => {
                println!();
                // Сохраняем команду в историю (если не пустая)
                if pos > 0 {
                    unsafe {
                        let slot = HISTORY_HEAD % HISTORY_MAX;
                        HISTORY[slot] = input_buf;
                        HISTORY_HEAD += 1;
                        if HISTORY_COUNT < HISTORY_MAX {
                            HISTORY_COUNT += 1;
                        }
                    }
                }
                break;
            }
            // Backspace
            '\u{8}' | '\u{7f}' => {
                if pos > 0 {
                    pos -= 1;
                    input_len -= 1;
                    crate::print!("\u{8} \u{8}");
                }
            }
            // Обычный печатный символ
            c => {
                if pos < input_buf.len() - 1 && c.is_ascii() {
                    input_buf[pos] = c as u8;
                    pos += 1;
                    input_len += 1;
                    crate::print!("{}", c);
                }
            }
        }
    }
    input_buf
}

/// Возвращает индекс в массиве HISTORY для смещения hist_idx от начала.
/// hist_idx=0 → самая свежая команда, hist_idx=COUNT-1 → самая старая.
///
/// Код выделен бирюзовым цветом (правило .kodaruls п.6).
///
/// ИСПРАВЛЕНИЕ: при hist_idx=0 нужно получить самый свежий слот,
/// который всегда = (HISTORY_HEAD - 1) % HISTORY_MAX.
/// При hist_idx=N сдвигаемся назад на N позиций в кольце.
fn history_index(hist_idx: usize) -> usize {
    unsafe {
        if HISTORY_COUNT == 0 {
            return 0;
        }
        if HISTORY_COUNT < HISTORY_MAX {
            // Буфер не заполнен: команды в слотах [0..COUNT).
            // hist_idx=0 → самый свежий = COUNT-1, hist_idx=COUNT-1 → самый старый = 0.
            HISTORY_COUNT - 1 - hist_idx
        } else {
            // Буфер полный: кольцо. Самый свежий слот = (HEAD - 1) % MAX.
            // Сдвигаемся назад на hist_idx позиций.
            ((HISTORY_HEAD as u64).wrapping_sub(hist_idx as u64) % HISTORY_MAX as u64) as usize
        }
    }
}
