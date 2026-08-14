use crate::editor::EditorResult;
use crate::ui::keybindings::KeyPress;
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal::{self, Clear, ClearType, disable_raw_mode, enable_raw_mode},
};
use std::io::{stdout, Write};

pub struct MicroEditor {
    file_path: Option<String>,
    content: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,  // Позиция в символах, не в байтах
    offset_x: usize,
    offset_y: usize,
    screen_width: u16,
    screen_height: u16,
    should_quit: bool,
    modified: bool,
}

impl MicroEditor {
    pub fn new() -> Self {
        let (screen_width, screen_height) = terminal::size().unwrap_or((80, 24));
        
        Self {
            file_path: None,
            content: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
            offset_x: 0,
            offset_y: 0,
            screen_width,
            screen_height,
            should_quit: false,
            modified: false,
        }
    }

    pub fn open_file(&mut self, filename: &str) -> EditorResult<()> {
        match std::fs::read_to_string(filename) {
            Ok(content) => {
                self.file_path = Some(filename.to_string());
                self.content = if content.is_empty() {
                    vec![String::new()]
                } else {
                    content.lines().map(|s| s.to_string()).collect()
                };
                self.cursor_line = 0;
                self.cursor_col = 0;
                self.offset_x = 0;
                self.offset_y = 0;
                self.modified = false;
                Ok(())
            }
            Err(e) => {
                let error_msg = format!("Не удалось открыть файл {}: {}", filename, e);
                Err(error_msg)
            }
        }
    }

    pub fn run(&mut self) -> EditorResult<()> {
        self.setup_terminal()?;

        while !self.should_quit {
            self.draw()?;
            
            if let Some(key) = self.read_key()? {
                self.handle_key(key);
            }
        }

        self.cleanup_terminal()?;
        Ok(())
    }

    fn setup_terminal(&self) -> EditorResult<()> {
        enable_raw_mode()
            .map_err(|e| format!("Не удалось включить raw mode: {}", e))?;
        
        execute!(
            stdout(),
            Hide,
            Clear(ClearType::All),
        ).map_err(|e| format!("Не удалось скрыть курсор: {}", e))?;

        Ok(())
    }

    fn cleanup_terminal(&self) -> EditorResult<()> {
        execute!(
            stdout(),
            Show,
            MoveTo(0, 0),
            ResetColor,
        ).map_err(|e| format!("Не удалось показать курсор: {}", e))?;

        disable_raw_mode()
            .map_err(|e| format!("Не удалось выключить raw mode: {}", e))?;

        Ok(())
    }

    fn draw(&self) -> EditorResult<()> {
        execute!(stdout(), MoveTo(0, 0), Clear(ClearType::All))
            .map_err(|e| format!("Ошибка очистки экрана: {}", e))?;

        // Рисуем содержимое файла
        for (line_num, line) in self.content.iter().enumerate().skip(self.offset_y).take(self.screen_height as usize - 2) {
            if line_num >= self.content.len() {
                break;
            }

            // Работаем с символами, а не байтами
            let display_chars: Vec<char> = line.chars().collect();
            let start_col = self.offset_x.min(display_chars.len());
            let visible_chars: String = display_chars[start_col..]
                .iter()
                .take(self.screen_width as usize)
                .collect();

            execute!(
                stdout(),
                MoveTo(0, (line_num - self.offset_y) as u16),
                Print(&visible_chars)
            ).map_err(|e| format!("Ошибка отрисовки строки: {}", e))?;
        }

        // Рисуем статус бар
        let status_line = self.screen_height - 1;
        let file_info = if self.modified {
            format!("*{}", self.file_path.as_ref().map_or("Новый файл".to_string(), |p| p.clone()))
        } else {
            self.file_path.as_ref().map_or("Новый файл".to_string(), |p| p.clone())
        };
        
        let cursor_info = format!("Строка: {}, Колонка: {}", self.cursor_line + 1, self.cursor_col + 1);
        let status = format!("{} | {} | Ctrl+Q для выхода", file_info, cursor_info);
        
        execute!(
            stdout(),
            MoveTo(0, status_line),
            SetBackgroundColor(Color::Blue),
            SetForegroundColor(Color::White),
            Clear(ClearType::CurrentLine),
            Print(&status[..status.len().min(self.screen_width as usize)])
        ).map_err(|e| format!("Ошибка отрисовки статус бара: {}", e))?;

        // Позиционируем курсор
        let cursor_screen_x = (self.cursor_col.saturating_sub(self.offset_x)) as u16;
        let cursor_screen_y = (self.cursor_line.saturating_sub(self.offset_y)) as u16;
        
        execute!(
            stdout(),
            MoveTo(cursor_screen_x, cursor_screen_y),
            ResetColor,
        ).map_err(|e| format!("Ошибка позиционирования курсора: {}", e))?;

        stdout().flush().map_err(|e| format!("Ошибка сброса буфера: {}", e))?;

        Ok(())
    }

    fn handle_key(&mut self, key: KeyPress) {
        match key {
            // ✅ ИСПРАВЛЕНО: Правильная проверка Ctrl+Q
            KeyPress::Ctrl('q') => {
                self.should_quit = true;
            }
            // ✅ Сохранение: Ctrl+S
            KeyPress::Ctrl('s') => {
                if let Err(e) = self.save_file() {
                    eprintln!("Ошибка сохранения: {}", e);
                } else {
                    self.modified = false;
                }
            }
            // Навигация вверх
            KeyPress::Up => {
                if self.cursor_line > 0 {
                    self.cursor_line -= 1;
                    // Убеждаемся, что курсор не выходит за пределы строки
                    let line_len = self.content[self.cursor_line].chars().count();
                    self.cursor_col = self.cursor_col.min(line_len);
                }
            }
            // Навигация вниз
            KeyPress::Down => {
                if self.cursor_line < self.content.len() - 1 {
                    self.cursor_line += 1;
                    let line_len = self.content[self.cursor_line].chars().count();
                    self.cursor_col = self.cursor_col.min(line_len);
                }
            }
            // Навигация влево
            KeyPress::Left => {
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                } else if self.cursor_line > 0 {
                    self.cursor_line -= 1;
                    self.cursor_col = self.content[self.cursor_line].chars().count();
                }
            }
            // Навигация вправо
            KeyPress::Right => {
                let line_len = self.content[self.cursor_line].chars().count();
                if self.cursor_col < line_len {
                    self.cursor_col += 1;
                } else if self.cursor_line < self.content.len() - 1 {
                    self.cursor_line += 1;
                    self.cursor_col = 0;
                }
            }
            // Home - в начало строки
            KeyPress::Home => {
                self.cursor_col = 0;
            }
            // End - в конец строки
            KeyPress::End => {
                self.cursor_col = self.content[self.cursor_line].chars().count();
            }
            // Backspace
            KeyPress::Backspace => {
                if self.cursor_col > 0 {
                    let line = &mut self.content[self.cursor_line];
                    let chars: Vec<char> = line.chars().collect();
                    
                    // Удаляем символ перед курсором
                    let mut new_line = String::new();
                    for (i, c) in chars.iter().enumerate() {
                        if i != self.cursor_col - 1 {
                            new_line.push(*c);
                        }
                    }
                    self.content[self.cursor_line] = new_line;
                    self.cursor_col -= 1;
                    self.modified = true;
                } else if self.cursor_line > 0 {
                    // Объединяем с предыдущей строкой
                    let current_line = self.content.remove(self.cursor_line);
                    self.cursor_line -= 1;
                    self.cursor_col = self.content[self.cursor_line].chars().count();
                    self.content[self.cursor_line].push_str(&current_line);
                    self.modified = true;
                }
            }
            // Delete
            KeyPress::Delete => {
                let line = &mut self.content[self.cursor_line];
                let chars: Vec<char> = line.chars().collect();
                
                if self.cursor_col < chars.len() {
                    let mut new_line = String::new();
                    for (i, c) in chars.iter().enumerate() {
                        if i != self.cursor_col {
                            new_line.push(*c);
                        }
                    }
                    self.content[self.cursor_line] = new_line;
                    self.modified = true;
                }
            }
            // Enter - разделить строку
            KeyPress::Enter => {
                let line = &mut self.content[self.cursor_line];
                let chars: Vec<char> = line.chars().collect();
                
                let left: String = chars[..self.cursor_col].iter().collect();
                let right: String = chars[self.cursor_col..].iter().collect();
                
                self.content[self.cursor_line] = left;
                self.content.insert(self.cursor_line + 1, right);
                
                self.cursor_line += 1;
                self.cursor_col = 0;
                self.modified = true;
            }
            // Tab
            KeyPress::Tab => {
                let line = &mut self.content[self.cursor_line];
                let chars: Vec<char> = line.chars().collect();
                
                let mut new_line = String::new();
                for (i, c) in chars.iter().enumerate() {
                    if i == self.cursor_col {
                        new_line.push_str("    "); // 4 пробела вместо таба
                    }
                    new_line.push(*c);
                }
                if self.cursor_col >= chars.len() {
                    new_line.push_str("    ");
                }
                
                self.content[self.cursor_line] = new_line;
                self.cursor_col += 4;
                self.modified = true;
            }
            // Обычный символ
            KeyPress::Char(c) => {
                let line = &mut self.content[self.cursor_line];
                let chars: Vec<char> = line.chars().collect();
                
                let mut new_line = String::new();
                for (i, ch) in chars.iter().enumerate() {
                    if i == self.cursor_col {
                        new_line.push(c);
                    }
                    new_line.push(*ch);
                }
                if self.cursor_col >= chars.len() {
                    new_line.push(c);
                }
                
                self.content[self.cursor_line] = new_line;
                self.cursor_col += 1;
                self.modified = true;
            }
            _ => {}
        }

        // Обновляем скроллинг
        self.update_scroll();
    }

    fn update_scroll(&mut self) {
        // Вертикальный скроллинг
        if self.cursor_line < self.offset_y {
            self.offset_y = self.cursor_line;
        } else if self.cursor_line >= self.offset_y + self.screen_height as usize - 2 {
            self.offset_y = self.cursor_line.saturating_sub(self.screen_height as usize - 3);
        }

        // Горизонтальный скроллинг
        if self.cursor_col < self.offset_x {
            self.offset_x = self.cursor_col;
        } else if self.cursor_col >= self.offset_x + self.screen_width as usize {
            self.offset_x = self.cursor_col.saturating_sub(self.screen_width as usize - 1);
        }
    }

    fn read_key(&self) -> EditorResult<Option<KeyPress>> {
        if event::poll(std::time::Duration::from_millis(100))
            .map_err(|e| format!("Ошибка опроса событий: {}", e))?
        {
            if let Event::Key(key_event) = event::read()
                .map_err(|e| format!("Ошибка чтения события: {}", e))?
            {
                let key_press = self.convert_key_event(key_event);
                return Ok(key_press);
            }
        }
        Ok(None)
    }

    // ✅ НОВЫЙ МЕТОД: Правильная конвертация KE события в KeyPress
    fn convert_key_event(&self, key_event: KeyEvent) -> Option<KeyPress> {
        match key_event {
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => Some(KeyPress::Ctrl(c)),
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers: KeyModifiers::ALT,
                ..
            } => Some(KeyPress::Alt(c)),
            KeyEvent {
                code: KeyCode::Char(c),
                ..
            } => Some(KeyPress::Char(c)),
            KeyEvent {
                code: KeyCode::Up, ..
            } => Some(KeyPress::Up),
            KeyEvent {
                code: KeyCode::Down, ..
            } => Some(KeyPress::Down),
            KeyEvent {
                code: KeyCode::Left, ..
            } => Some(KeyPress::Left),
            KeyEvent {
                code: KeyCode::Right, ..
            } => Some(KeyPress::Right),
            KeyEvent {
                code: KeyCode::Home, ..
            } => Some(KeyPress::Home),
            KeyEvent {
                code: KeyCode::End, ..
            } => Some(KeyPress::End),
            KeyEvent {
                code: KeyCode::Enter, ..
            } => Some(KeyPress::Enter),
            KeyEvent {
                code: KeyCode::Backspace, ..
            } => Some(KeyPress::Backspace),
            KeyEvent {
                code: KeyCode::Delete, ..
            } => Some(KeyPress::Delete),
            KeyEvent {
                code: KeyCode::Tab, ..
            } => Some(KeyPress::Tab),
            KeyEvent {
                code: KeyCode::Esc, ..
            } => Some(KeyPress::Esc),
            KeyEvent {
                code: KeyCode::PageUp, ..
            } => Some(KeyPress::PageUp),
            KeyEvent {
                code: KeyCode::PageDown, ..
            } => Some(KeyPress::PageDown),
            KeyEvent {
                code: KeyCode::F(n), ..
            } => Some(KeyPress::F(n)),
            _ => None,
        }
    }

    pub fn save_file(&self) -> EditorResult<()> {
        if let Some(ref path) = self.file_path {
            let content = self.content.join("\n");
            std::fs::write(path, content)
                .map_err(|e| format!("Не удалось сохранить файл {}: {}", path, e))?;
            Ok(())
        } else {
            Err("Файл не имеет пути для сохранения".to_string())
        }
    }

    pub fn save_file_as(&mut self, filename: &str) -> EditorResult<()> {
        self.file_path = Some(filename.to_string());
        self.save_file()
    }
}
