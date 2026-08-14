#[derive(Debug, Clone)]
pub struct CommandHistory {
    commands: Vec<String>,
    max_size: usize,
    current_index: usize,
    history_file: std::path::PathBuf,
}

impl CommandHistory {
    pub fn new(max_size: usize) -> Self {
        let history_file = Self::get_history_file();
        
        let mut history = Self {
            commands: Vec::with_capacity(max_size),
            max_size,
            current_index: 0,
            history_file,
        };
        
        // Загружаем историю из файла при инициализации
        let _ = history.load_from_file();
        
        history
    }
    
    /// Получает путь к файлу истории
    /// Поддерживает Linux, macOS, Windows
    fn get_history_file() -> std::path::PathBuf {
        let config_dir = if cfg!(target_os = "windows") {
            // Windows: %APPDATA%\smart_term
            std::env::var("APPDATA")
                .ok()
                .map(|p| std::path::PathBuf::from(p).join("smart_term"))
                .unwrap_or_else(|| std::path::PathBuf::from("smart_term"))
        } else {
            // Unix-like: ~/.local/share/smart_term или ~/.config/smart_term
            if let Ok(home) = std::env::var("HOME") {
                let path = std::path::PathBuf::from(&home).join(".local/share/smart_term");
                // Если .local не существует, используем .config
                if !path.parent().map_or(false, |p| p.exists()) {
                    std::path::PathBuf::from(&home).join(".config/smart_term")
                } else {
                    path
                }
            } else {
                std::path::PathBuf::from(".smart_term_history")
            }
        };
        
        // Создаём директорию если её нет
        let _ = std::fs::create_dir_all(&config_dir);
        
        config_dir.join("history.json")
    }
    
    /// Загружает историю из файла
    fn load_from_file(&mut self) -> Result<(), String> {
        if !self.history_file.exists() {
            return Ok(()); // Файл не существует - это нормально
        }
        
        let content = std::fs::read_to_string(&self.history_file)
            .map_err(|e| format!("Ошибка чтения истории: {}", e))?;
        
        // Парсим JSON
        match serde_json::from_str::<Vec<String>>(&content) {
            Ok(commands) => {
                // Берём последние max_size команд
                let start = commands.len().saturating_sub(self.max_size);
                self.commands = commands[start..].to_vec();
                self.current_index = self.commands.len();
                Ok(())
            }
            Err(e) => {
                // Если JSON повреждён, просто начинаем заново
                eprintln!("Предупреждение: история повреждена ({}), начинаем заново", e);
                self.commands.clear();
                Ok(())
            }
        }
    }
    
    /// Сохраняет историю в файл
    fn save_to_file(&self) -> Result<(), String> {
        // Создаём директорию если её нет
        if let Some(parent) = self.history_file.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Ошибка создания директории: {}", e))?;
        }
        
        // Сохраняем как JSON
        let json = serde_json::to_string_pretty(&self.commands)
            .map_err(|e| format!("Ошибка сериализации: {}", e))?;
        
        std::fs::write(&self.history_file, json)
            .map_err(|e| format!("Ошибка сохранения истории: {}", e))?;
        
        Ok(())
    }
    
    pub fn add(&mut self, command: String) {
        let command = command.trim().to_string();
        
        // Не добавляем пустые команды и дубликаты подряд
        if !command.is_empty() && self.commands.last() != Some(&command) {
            if self.commands.len() >= self.max_size {
                self.commands.remove(0);
            }
            self.commands.push(command);
        }
        self.current_index = self.commands.len();
        
        // Автосохранение после добавления каждой команды
        let _ = self.save_to_file();
    }
    
    pub fn get_previous(&mut self) -> Option<&String> {
        if self.current_index > 0 {
            self.current_index -= 1;
            self.commands.get(self.current_index)
        } else {
            None
        }
    }
    
    pub fn get_next(&mut self) -> Option<&String> {
        if self.current_index < self.commands.len() - 1 {
            self.current_index += 1;
            self.commands.get(self.current_index)
        } else {
            self.current_index = self.commands.len();
            None
        }
    }
    
    pub fn get_all(&self) -> &Vec<String> {
        &self.commands
    }
    
    pub fn clear(&mut self) {
        self.commands.clear();
        self.current_index = 0;
        let _ = self.save_to_file();
    }
    
    pub fn search(&self, query: &str) -> Vec<&String> {
        self.commands
            .iter()
            .filter(|cmd| cmd.contains(query))
            .collect()
    }
    
    pub fn len(&self) -> usize {
        self.commands.len()
    }
    
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
    
    pub fn get_by_index(&self, index: usize) -> Option<&String> {
        self.commands.get(index)
    }
    
    pub fn remove(&mut self, index: usize) -> Option<String> {
        if index < self.commands.len() {
            let removed = self.commands.remove(index);
            if self.current_index >= index {
                self.current_index = self.current_index.saturating_sub(1);
            }
            let _ = self.save_to_file();
            Some(removed)
        } else {
            None
        }
    }
    
    /// Сохраняет историю перед закрытием
    pub fn save_on_exit(&self) -> Result<(), String> {
        self.save_to_file()
    }
    
    /// Получает путь к файлу истории для информации
    pub fn get_history_path(&self) -> &std::path::Path {
        &self.history_file
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_history_file_path() {
        let history = CommandHistory::new(100);
        let path = history.get_history_path();
        
        // Проверяем, что путь содержит 'smart_term'
        assert!(path.to_string_lossy().contains("smart_term"));
        
        // Проверяем расширение файла
        assert_eq!(path.extension().and_then(|s| s.to_str()), Some("json"));
    }
    
    #[test]
    fn test_add_command() {
        let mut history = CommandHistory::new(100);
        history.add("ls -la".to_string());
        
        assert_eq!(history.len(), 1);
        assert_eq!(history.get_by_index(0), Some(&"ls -la".to_string()));
    }
    
    #[test]
    fn test_no_duplicates() {
        let mut history = CommandHistory::new(100);
        history.add("ls".to_string());
        history.add("ls".to_string());
        
        assert_eq!(history.len(), 1);
    }
    
    #[test]
    fn test_search() {
        let mut history = CommandHistory::new(100);
        history.add("ls -la".to_string());
        history.add("cd /home".to_string());
        history.add("ls -l".to_string());
        
        let results = history.search("ls");
        assert_eq!(results.len(), 2);
    }
    
    #[test]
    fn test_navigation() {
        let mut history = CommandHistory::new(100);
        history.add("cmd1".to_string());
        history.add("cmd2".to_string());
        history.add("cmd3".to_string());
        
        assert_eq!(history.get_previous(), Some(&"cmd3".to_string()));
        assert_eq!(history.get_previous(), Some(&"cmd2".to_string()));
        assert_eq!(history.get_next(), Some(&"cmd3".to_string()));
    }
}
