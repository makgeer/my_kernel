// ============================================================================
// src/terminal.rs — ПЕРВОЕ ПРИЛОЖЕНИЕ ОС Malinka: полноценный терминал.
//
// НОВЫЙ КОД (до внесения в коммит) — по правилу 6 .kodaruls.
//
// Что умеет терминал:
//   - единый offscreen-буфер 80x25 ячеек (символ + foreground + background);
//   - 16-цветная палитра ANSI (SGR): сброс, цвета 30-37/40-47,
//     яркие 90-97/100-107, bold (1), extended 38;5;n / 48;5;n;
//   - курсор: позиция, видимость, мигание (инверсная ячейка);
//   - прокрутка при заполнении экрана;
//   - ANSI escape-коды (CSI): H/f (позиция), A/B/C/D (движение),
//     J (очистка экрана), K (очистка строки), G (колонка),
//     ?25h / ?25l (показать/скрыть курсор);
//   - рендер ВО ВСЕ три устройства сразу:
//       * VGA text buffer 0xB8000 (атрибут = bg<<4 | fg),
//       * Serial COM1 (ANSI-перерисовка экрана — работает как настоящий
//         терминал при Bochs -serial stdio),
//       * Framebuffer (шрифт 8x16, шаг 9 px по X) через vesa_draw_char_ex.
//
// Терминал станет единым источником правды для вывода: макросы
// print!/println! будут писать в него, а он сам распределяет вывод.
// ============================================================================

use core::fmt;
use spin::Mutex;

/// Ширина терминала в символах (совпадает с классическим VGA-режимом).
pub const COLS: usize = 80;
/// Высота терминала в символах.
pub const ROWS: usize = 25;

/// Палитра терминала: индексы 0..15 совпадают с VGA-атрибутами
/// (0-7 обычные, 8-15 яркие) и с ANSI SGR 30-37 / 90-97.
/// Значения — RGB для отрисовки на framebuffer.
const PALETTE: [u32; 16] = [
    0x000000, // 0  чёрный
    0xAA0000, // 1  красный
    0x00AA00, // 2  зелёный
    0xAA5500, // 3  коричневый
    0x0000AA, // 4  синий
    0xAA00AA, // 5  пурпурный
    0x00AAAA, // 6  голубой
    0xAAAAAA, // 7  светло-серый
    0x555555, // 8  тёмно-серый
    0xFF5555, // 9  ярко-красный
    0x55FF55, // 10 ярко-зелёный
    0xFFFF55, // 11 жёлтый
    0x5555FF, // 12 ярко-синий
    0xFF55FF, // 13 ярко-пурпурный
    0x55FFFF, // 14 ярко-голубой
    0xFFFFFF, // 15 белый
];

/// Одна ячейка экранного буфера.
#[derive(Clone, Copy)]
struct Cell {
    ch: u8,
    fg: u8,
    bg: u8,
}

impl Cell {
    /// Пустая ячейка: пробел, светло-серый на чёрном (как VGA по умолчанию).
    const fn blank() -> Self {
        Cell { ch: b' ', fg: 7, bg: 0 }
    }
}

/// Состояние ANSI-парсера (мини-VT100).
#[derive(PartialEq, Clone, Copy)]
enum AnsiState {
    /// Обычный текст.
    Normal,
    /// Получен ESC (0x1B) — ждём '[' (CSI) или что-то другое.
    Esc,
    /// Идёт разбор CSI-последовательности: собираем параметры.
    Csi,
}

/// Полноценный терминал ядра.
pub struct Terminal {
    /// Экранный буфер: строки x колонки.
    cells: [[Cell; COLS]; ROWS],
    /// Текущая строка курсора.
    row: usize,
    /// Текущая колонка курсора.
    col: usize,
    /// Текущий цвет символов (0..15).
    fg: u8,
    /// Текущий цвет фона (0..15).
    bg: u8,
    /// Виден ли курсор (по умолчанию да, управляется ANSI ?25h/l).
    cursor_visible: bool,
    /// Состояние ANSI-парсера.
    ansi: AnsiState,
    /// Буфер параметров CSI-последовательности.
    csi_params: [u16; 8],
    /// Сколько параметров уже накоплено.
    csi_len: usize,
    /// Приватный модификатор CSI ('?' перед параметрами, напр. ?25l).
    csi_private: bool,
    /// Авто-flush: вызывать flush() при обнаружении '\n' в write_str.
    /// По умолчанию true. Отключается для пакетного вывода (напр. list()
    /// в process_manager) — чтобы избежать N полных перерисовок экрана.
    auto_flush: bool,
}

impl Terminal {
    /// Создаёт пустой терминал с позицией курсора (0,0) и белым текстом.
    pub const fn new() -> Self {
        Terminal {
            cells: [[Cell::blank(); COLS]; ROWS],
            row: 0,
            col: 0,
            fg: 7,
            bg: 0,
            cursor_visible: true,
            ansi: AnsiState::Normal,
            csi_params: [0; 8],
            csi_len: 0,
            csi_private: false,
            auto_flush: true,
        }
    }

    /// Обрабатывает один символ: управляющие коды, ANSI, печать.
    ///
    /// ОПТИМИЗАЦИЯ (2026-08-17): каждый символ СРАЗУ пишется в serial (COM1)
    /// потоковым выводом. Раньше serial перерисовывался целиком (draw_serial:
    /// 2000 ячеек x ANSI-SGR на каждый '\n') — в Bochs это тормозило запуск
    /// ядра на минуты. Теперь serial получает обычный текст, а полная ANSI-
    /// перерисовка нужна только VGA/framebuffer (они пишут в память — быстро).
    pub fn put_char(&mut self, c: char) {
        match self.ansi {
            AnsiState::Normal => match c {
                '\u{1b}' => self.ansi = AnsiState::Esc,
                '\n' => {
                    self.crlf();
                    // write_str сам переводит '\n' в CRLF для терминала.
                    crate::serial::write_str("\n");
                }
                '\r' => {
                    self.col = 0;
                    crate::serial::serial_write_byte(b'\r');
                }
                '\u{8}' => {
                    self.backspace();
                    crate::serial::serial_write_byte(b'\x08');
                }
                                '\x09' => {
                    self.tab();
                    crate::serial::serial_write_byte(b'\x09');
                }
                c => {
                    self.print_char(c);
                    // Печатный символ — сразу в serial (потоковый вывод).
                    if c.is_ascii() && c >= '\u{20}' && c <= '\u{7e}' {
                        crate::serial::serial_write_byte(c as u8);
                    }
                }
            },
            AnsiState::Esc => {
                // После ESC ждём '[' (CSI). Остальные 2-символьные
                // последовательности игнорируем для простоты.
                if c == '[' {
                    self.csi_len = 0;
                    self.csi_private = false;
                    self.ansi = AnsiState::Csi;
                } else {
                    self.ansi = AnsiState::Normal;
                }
            }
            AnsiState::Csi => {
                self.consume_csi(c);
            }
        }
    }

    // ------------------------------------------------------------------
    // Примитивы работы с курсором и буфером
    // ------------------------------------------------------------------

    /// Перенос строки: вниз, при достижении низа — прокрутка.
    fn crlf(&mut self) {
        self.col = 0;
        if self.row + 1 >= ROWS {
            self.scroll();
        } else {
            self.row += 1;
        }
    }

    /// Backspace: откат колонки (не стирает символ — как в реальных терминалах).
    fn backspace(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        }
    }

    /// Табуляция: переход к ближайшей позиции, кратной 8.
    fn tab(&mut self) {
        self.col = (self.col / 8 + 1) * 8;
        if self.col >= COLS {
            self.crlf();
        }
    }

    /// Печатает печатный символ в текущую позицию с текущими цветами.
    fn print_char(&mut self, c: char) {
        // Печатный ASCII-диапазон 0x20..0x7E; прочее отображаем как пробел.
        let code = if c.is_ascii() && c >= '\u{20}' && c <= '\u{7e}' {
            c as u8
        } else {
            b' '
        };
        self.cells[self.row][self.col] = Cell {
            ch: code,
            fg: self.fg,
            bg: self.bg,
        };
        self.col += 1;
        if self.col >= COLS {
            self.crlf();
        }
    }

    /// Прокрутка экрана на одну строку вверх (нижняя строка очищается).
    fn scroll(&mut self) {
        for r in 1..ROWS {
            self.cells[r - 1] = self.cells[r];
        }
        self.cells[ROWS - 1] = [Cell::blank(); COLS];
        // Курсор остаётся на последней строке, колонка уже сброшена
        // вызывающим кодом (crlf).
        self.row = ROWS - 1;
    }

    /// Очищает весь экран и возвращает курсор в (0,0).
    pub fn clear_screen(&mut self) {
        self.cells = [[Cell::blank(); COLS]; ROWS];
        self.row = 0;
        self.col = 0;
        // Serial теперь не перерисовывается целиком — очищаем его явно
        // ANSI-кодами (ED 2J + CUP H), иначе старый текст останется.
        crate::serial::serial_write_byte(0x1b);
        crate::serial::write_str("[2J");
        crate::serial::serial_write_byte(0x1b);
        crate::serial::write_str("[H");
    }

    /// Диагностика: текущая позиция курсора терминала (строка, колонка).
    /// Позволяет сравнить логическую позицию терминала с визуальным
    /// положением курсора, который видит пользователь.
    pub fn position(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    /// Очищает часть экрана согласно коду ED: 0 — от курсора до конца,
    /// 2 — весь экран.
    fn clear_screen_mode(&mut self, mode: u16) {
        match mode {
            0 => {
                // Очищаем текущую строку от курсора и все нижние строки.
                for col in self.col..COLS {
                    self.cells[self.row][col] = Cell::blank();
                }
                for r in (self.row + 1)..ROWS {
                    self.cells[r] = [Cell::blank(); COLS];
                }
            }
            _ => self.clear_screen(),
        }
    }

    /// Очищает часть строки согласно коду EL: 0 — от курсора до конца,
    /// 2 — всю строку.
    fn clear_line_mode(&mut self, mode: u16) {
        match mode {
            0 => {
                for col in self.col..COLS {
                    self.cells[self.row][col] = Cell::blank();
                }
            }
            2 => {
                self.cells[self.row] = [Cell::blank(); COLS];
            }
            _ => {}
        }
    }

    /// Позиционирует курсор: CUP/CHA. Строки и колонки — 1-based.
    fn set_cursor(&mut self, row1: u16, col1: u16) {
        let r = if row1 == 0 { 1 } else { row1 as usize };
        let c = if col1 == 0 { 1 } else { col1 as usize };
        self.row = (r - 1).min(ROWS - 1);
        self.col = (c - 1).min(COLS - 1);
    }

    /// Двигает курсор по стрелкам CSI A/B/C/D (n — количество шагов).
    fn move_cursor(&mut self, dir: char, n: u16) {
        let n = n.max(1) as usize;
        match dir {
            'A' => self.row = self.row.saturating_sub(n),
            'B' => self.row = (self.row + n).min(ROWS - 1),
            'C' => self.col = (self.col + n).min(COLS - 1),
            'D' => self.col = self.col.saturating_sub(n),
            _ => {}
        }
    }

    // ------------------------------------------------------------------
    // ANSI CSI-парсер
    // ------------------------------------------------------------------

    /// Обрабатывает очередной символ внутри CSI-последовательности.
    fn consume_csi(&mut self, c: char) {
        match c {
            // '?' — приватный модификатор (перед параметрами, напр. ?25l)
            '?' if self.csi_len == 0 => self.csi_private = true,
            // Цифра — накапливаем текущий параметр.
            d if d.is_ascii_digit() => {
                if self.csi_len == 0 {
                    self.csi_len = 1;
                }
                let cur = self.csi_params[self.csi_len - 1];
                self.csi_params[self.csi_len - 1] = (cur * 10 + (d as u8 - b'0') as u16).min(999);
            }
            // ';' — завершаем текущий параметр, начинаем следующий.
            ';' => {
                if self.csi_len < self.csi_params.len() {
                    self.csi_len += 1;
                }
            }
            // Буква — финальный символ: выполняем команду.
            f => {
                self.finish_csi(f);
                self.ansi = AnsiState::Normal;
            }
        }
    }

    /// Параметр с дефолтом (CSI без явных значений трактуется как 1).
    fn csi_param(&self, idx: usize, default: u16) -> u16 {
        if idx < self.csi_len && self.csi_params[idx] != 0 {
            self.csi_params[idx]
        } else {
            default
        }
    }

    /// Выполняет команду CSI по финальному символу.
    fn finish_csi(&mut self, final_char: char) {
        // Приватные режимы: ?25h (показать курсор), ?25l (скрыть).
        if self.csi_private {
            if (final_char == 'h' || final_char == 'l') && self.csi_param(0, 1) == 25 {
                self.cursor_visible = final_char == 'h';
            }
            return;
        }

        match final_char {
            // SGR — установка атрибутов текста.
            'm' => self.apply_sgr(),
            // CUP (H) / HVP (f) — позиция курсора: row;col (1-based).
            'H' | 'f' => {
                let r = self.csi_param(0, 1);
                let c = self.csi_param(1, 1);
                self.set_cursor(r, c);
            }
            // CHA (G) — установка колонки.
            'G' => {
                let c = self.csi_param(0, 1);
                self.set_cursor(self.row as u16 + 1, c);
            }
            // Движение курсора: A вверх, B вниз, C вправо, D влево.
            'A' | 'B' | 'C' | 'D' => {
                let n = self.csi_param(0, 1);
                self.move_cursor(final_char, n);
            }
            // ED — очистка экрана: 0 до конца, 2 весь.
            'J' => {
                let mode = self.csi_param(0, 0);
                self.clear_screen_mode(mode);
            }
            // EL — очистка строки: 0 до конца, 2 вся.
            'K' => {
                let mode = self.csi_param(0, 0);
                self.clear_line_mode(mode);
            }
            // Прочие CSI игнорируем (пока не поддерживаются).
            _ => {}
        }
    }

    /// SGR: применяет последовательность параметров (по умолчанию 0).
    fn apply_sgr(&mut self) {
        // Пустой CSI m эквивалентен параметру 0.
        let count = self.csi_len.max(1);
        for i in 0..count {
            let p = self.csi_params[i];
            match p {
                0 => {
                    self.fg = 7;
                    self.bg = 0;
                }
                // Bold — повышаем интенсивность текста (0-7 -> 8-15).
                1 => {
                    if self.fg < 8 {
                        self.fg += 8;
                    }
                }
                // Обычные цвета текста.
                30..=37 => self.fg = (p - 30) as u8,
                // Обычные цвета фона.
                40..=47 => self.bg = (p - 40) as u8,
                // Яркие цвета текста.
                90..=97 => self.fg = (p - 90 + 8) as u8,
                // Яркие цвета фона.
                100..=107 => self.bg = (p - 100 + 8) as u8,
                // Extended color: 38;5;n / 48;5;n (индекс 0..15 из палитры).
                38 | 48 => {
                    if i + 2 < count && self.csi_params[i + 1] == 5 {
                        let idx = (self.csi_params[i + 2] % 16) as u8;
                        if p == 38 {
                            self.fg = idx;
                        } else {
                            self.bg = idx;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // ------------------------------------------------------------------
    // Рендер во все устройства
    // ------------------------------------------------------------------

    /// Полная перерисовка экрана терминала во все устройства.
    ///
    /// ОПТИМИЗАЦИЯ: Serial (COM1) больше НЕ перерисовывается целиком —
    /// он получает символы потоково в put_char (см. комментарий там).
    /// Остаются только VGA-буфер и Framebuffer (запись в память — быстро).
    pub fn flush(&mut self) {
        self.draw_vga();
        self.draw_fb();
        self.advance_blink();
    }

    /// Включает или отключает авто-flush при '\n' в write_str.
    /// При отключении вызывающий код должен сам вызвать flush() после
    /// завершения пакетного вывода — иначе текст не появится на экране.
    pub fn set_auto_flush(&mut self, enabled: bool) {
        self.auto_flush = enabled;
    }
}

/// Счётчик мигания курсора: меняет фазу примерно каждые 5 перерисовок.
static BLINK_COUNTER: Mutex<usize> = Mutex::new(0);

impl Terminal {
    /// Инкрементит счётчик мигания курсора.
    fn advance_blink(&self) {
        *BLINK_COUNTER.lock() += 1;
    }

    /// Видна ли сейчас курсорная метка (с учётом мигания).
    fn cursor_on(&self) -> bool {
        if !self.cursor_visible {
            return false;
        }
        (*BLINK_COUNTER.lock() / 5) % 2 == 0
    }

    /// Рисует экран в VGA text buffer (0xB8000).
    /// Атрибут: старший ниббл — фон, младший — текст.
    fn draw_vga(&mut self) {
        const VGA_BUF: *mut u16 = 0xb8000 as *mut u16;
        let cursor_on = self.cursor_on();
        for r in 0..ROWS {
            for c in 0..COLS {
                let mut cell = self.cells[r][c];
                // Инвертируем ячейку под курсором, чтобы он был виден.
                if cursor_on && r == self.row && c == self.col {
                    core::mem::swap(&mut cell.fg, &mut cell.bg);
                }
                let attr = (cell.bg << 4) | (cell.fg & 0x0f);
                let word = ((attr as u16) << 8) | cell.ch as u16;
                unsafe {
                    *VGA_BUF.offset((r * COLS + c) as isize) = word;
                }
            }
        }
        // Аппаратный курсор VGA (порты 0x3D4/0x3D5): синхронизируем его
        // с позицией терминала. Limine/Bochs после загрузки оставляют
        // аппаратный курсор в произвольной позиции (обычно правый нижний
        // угол) — это выглядит как «курсор смещён вправо вниз» от строки
        // ввода. Запись безвредна и в графическом режиме (курсор там
        // просто не отображается).
        unsafe {
            use x86_64::instructions::port::Port;
            let pos = self.row * COLS + self.col;
            let mut index = Port::<u8>::new(0x3D4);
            let mut data = Port::<u8>::new(0x3D5);
            // Старший байт позиции (регистр 0x0E)
            index.write(0x0E);
            data.write((pos >> 8) as u8);
            // Младший байт позиции (регистр 0x0F)
            index.write(0x0F);
            data.write((pos & 0xFF) as u8);
        }
    }

    /// Рисует экран в Serial (COM1) ANSI-последовательностями:
    /// очищаем экран, выводим каждую ячейку с её цветом SGR,
    /// затем возвращаем терминальный курсор на текущую позицию.
    ///
    /// ОПТИМИЗАЦИЯ 2026-08-17: функция ВРЕМЕННО не вызывается (flush не
    /// делает полную serial-перерисовку — символы пишутся потоково в
    /// put_char). Код сохранён: пригодится для отладки терминала/цветов.
    #[allow(dead_code)]
    fn draw_serial(&self) {
        use crate::serial::{serial_write_byte, serial_write_num, write_str};
        // Отключаем автоперенос строк (DECAWM ?7l) на время перерисовки.
        // Иначе после печати 80-го символа последней строки терминал
        // переносит курсор на несуществующую строку и прокручивает экран
        // вверх на 1 строку — приглашение визуально уходит выше, а курсор
        // после позиционирования оказывается «на строку ниже приглашения».
        serial_write_byte(0x1b);
        write_str("[?7l");
        // Сброс атрибутов, очистка экрана, курсор домой.
        serial_write_byte(0x1b);
        write_str("[0m");
        serial_write_byte(0x1b);
        write_str("[2J");
        serial_write_byte(0x1b);
        write_str("[H");
        for r in 0..ROWS {
            for c in 0..COLS {
                let cell = self.cells[r][c];
                // SGR: "\x1b[0;{fg+30};{bg+40}m" — числа пишем поразрядно.
                serial_write_byte(0x1b);
                serial_write_byte(b'[');
                serial_write_byte(b'0');
                serial_write_byte(b';');
                serial_write_num(cell.fg as u16 + 30);
                serial_write_byte(b';');
                serial_write_num(cell.bg as u16 + 40);
                serial_write_byte(b'm');
                serial_write_byte(cell.ch);
            }
            if r != ROWS - 1 {
                serial_write_byte(b'\r');
                serial_write_byte(b'\n');
            }
        }
        // Возвращаем курсор на текущую позицию: "\x1b[{r};{c}H".
        serial_write_byte(0x1b);
        serial_write_byte(b'[');
        serial_write_num((self.row + 1) as u16);
        serial_write_byte(b';');
        serial_write_num((self.col + 1) as u16);
        serial_write_byte(b'H');
        serial_write_byte(0x1b);
        write_str("[0m");
        // Включаем автоперенос обратно (DECAWM ?7h) — для обычного ввода.
        serial_write_byte(0x1b);
        write_str("[?7h");
    }

    /// Рисует экран на Framebuffer через шрифт 8x16 с шагом 9 px по X.
    fn draw_fb(&self) {
        let cursor_on = self.cursor_on();
        for r in 0..ROWS {
            for c in 0..COLS {
                let mut cell = self.cells[r][c];
                if cursor_on && r == self.row && c == self.col {
                    core::mem::swap(&mut cell.fg, &mut cell.bg);
                }
                let x = c * 9;
                let y = r * 16;
                crate::video::draw_char_ex(
                    cell.ch as char,
                    x,
                    y,
                    PALETTE[cell.fg as usize % 16],
                    PALETTE[cell.bg as usize % 16],
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Рендер SGR-цветов и позиции курсора в Serial выполняется напрямую
// поразрядной записью чисел (serial_write_byte / serial_write_num),
// поэтому const-таблицы строк здесь не нужны (см. draw_serial выше).
// ---------------------------------------------------------------------------

/// Глобальный экземпляр терминала. Все макросы вывода (print!/println!)
/// будут писать в него, а он сам распределяет вывод по VGA/Serial/Framebuffer.
pub static TERMINAL: Mutex<Terminal> = Mutex::new(Terminal::new());

/// Поддержка `write!`/`write_fmt` для терминала: каждый фрагмент строки
/// разбирается посимвольно (включая ANSI-коды).
///
/// ОПТИМИЗАЦИЯ: полная перерисовка (flush) выполняется только при
/// обнаружении '\n' — конца строки. Раньше flush вызывался на каждый
/// вызов write_str (а один println! вызывает write_str 3-4 раза),
/// что приводило к 3-4 полным перерисовкам экрана (80x25 = 2000 ячеек
/// с ANSI-SGR кодами) на каждую строку вывода. Это создавало лавину
/// serial-данных (~8 КБ на одну строку) и замедляло вывод.
///
/// Теперь: фрагменты без '\n' (например, ">" или "  alloc[0] = ")
/// просто копятся в буфере терминала без перерисовки. Перерисовка
/// происходит один раз при завершении строки.
impl fmt::Write for Terminal {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let mut need_flush = false;
        for c in s.chars() {
            if c == '\n' {
                need_flush = true;
            }
            self.put_char(c);
        }
    if need_flush && self.auto_flush {
        self.flush();
    }
        Ok(())
    }
}
