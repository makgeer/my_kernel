pub mod privileges;
pub mod helpers;

pub use privileges::PrivilegeManager;
pub use helpers::*;

/// Утилиты для работы с файловой системой
pub mod filesystem {
    use std::path::Path;
    
    /// Получить размер файла в человеко-читаемом формате
    pub fn get_file_size(path: &Path) -> Result<String, String> {
        let metadata = std::fs::metadata(path)
            .map_err(|e| format!("Не удалось получить метаданные: {}", e))?;
            
        let size = metadata.len();
        Ok(crate::utils::human_readable_size(size)) // Явное указание пути
    }
    
    /// Получить свободное место на диске
    pub fn get_free_space(path: &Path) -> String {
        #[cfg(unix)]
        {
            use std::ffi::CString;
            use std::os::unix::ffi::OsStrExt;
            
            // Используем libc для получения информации о диске (statfs)
            unsafe {
                let c_path = match CString::new(path.as_os_str().as_bytes()) {
                    Ok(p) => p,
                    Err(_) => return "Ошибка пути".to_string(),
                };
                
                let mut stat: libc::statvfs = std::mem::zeroed();
                if libc::statvfs(c_path.as_ptr(), &mut stat) == 0 {
                    // Свободное место = блок_размер * свободные_блоки
                    let free_bytes = stat.f_bsize as u64 * stat.f_bavail as u64;
                    crate::utils::human_readable_size(free_bytes)
                } else {
                    "Недоступно".to_string()
                }
            }
        }
        #[cfg(target_os = "windows")]
        {
            // Для Windows используем WinAPI
            use std::ffi::CString;
            
            let path_str = path.to_string_lossy();
            let c_path = match CString::new(path_str.as_bytes()) {
                Ok(p) => p,
                Err(_) => return "Ошибка пути".to_string(),
            };
            
            #[cfg(windows)]
            unsafe {
                use winapi::um::fileapi::GetDiskFreeSpaceExA;
                use winapi::shared::minwindef::FALSE;
                
                let mut free_bytes: u64 = 0;
                let mut total_bytes: u64 = 0;
                let mut available_bytes: u64 = 0;
                
                if GetDiskFreeSpaceExA(
                    c_path.as_ptr() as *const i8,
                    &mut free_bytes as *mut u64 as *mut winapi::shared::minwindef::ULARGE_INTEGER,
                    &mut total_bytes as *mut u64 as *mut winapi::shared::minwindef::ULARGE_INTEGER,
                    &mut available_bytes as *mut u64 as *mut winapi::shared::minwindef::ULARGE_INTEGER,
                ) != FALSE
                {
                    crate::utils::human_readable_size(free_bytes)
                } else {
                    "Недоступно".to_string()
                }
            }
            #[cfg(not(windows))]
            {
                "Недоступно".to_string()
            }
        }
        #[cfg(not(any(unix, target_os = "windows")))]
        {
            "Недоступно".to_string()
        }
    }
    
    /// Список файлов в директории
    pub fn list_files(path: &Path) -> Result<Vec<FileInfo>, String> {
        let entries = std::fs::read_dir(path)
            .map_err(|e| format!("Не удалось прочитать директорию: {}", e))?;
        
        let mut files = Vec::new();
        
        for entry in entries {
            let entry = entry.map_err(|e| format!("Ошибка при чтении файла: {}", e))?;
            let path = entry.path();
            let metadata = entry.metadata()
                .map_err(|e| format!("Не удалось получить метаданные: {}", e))?;
            
            files.push(FileInfo {
                name: entry.file_name().to_string_lossy().to_string(),
                path,
                is_dir: metadata.is_dir(),
                size: metadata.len(),
            });
        }
        
        Ok(files)
    }
    
    /// Структура информации о файле
    pub struct FileInfo {
        pub name: String,
        pub path: std::path::PathBuf,
        pub is_dir: bool,
        pub size: u64,
    }
}

/// Получить размер в человеко-читаемом формате
pub fn human_readable_size(size: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if size == 0 {
        return "0 B".to_string();
    }
    
    let mut size = size as f64;
    let mut unit_index = 0;
    
    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }
    
    // Оптимизация: убираем .0 для целых чисел
    if size.fract() == 0.0 {
        format!("{:.0} {}", size, UNITS[unit_index])
    } else {
        format!("{:.1} {}", size, UNITS[unit_index])
    }
}

/// Утилиты для работы со временем
pub mod time {
    use std::time::{SystemTime, UNIX_EPOCH};
    
    /// Получить текущее время в формате timestamp
    pub fn current_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default() // Защита от паники
            .as_secs()
    }
    
    /// Форматировать время для отображения
    pub fn format_time(timestamp: u64) -> String {
        match chrono::DateTime::from_timestamp(timestamp as i64, 0) {
            Some(dt) => dt.format("%Y-%m-%d %H:%M:%S").to_string(),
            None => "Неизвестно".to_string(),
        }
    }
}
