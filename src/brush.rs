//! Brush — встроенная bash-совместимая оболочка для ядра Malinka.
//!
//! Это НОВЫЙ КОД (бирюзовый, правило .kodaruls п.6).
//!
//! Крейт brush (crates.io, v0.4.0) — полноценная user-space оболочка
//! на Rust (требует std: файловая система, процессы, std::io). Внедрить
//! её в no_std ядро невозможно. Вместо этого здесь реализовано
//! СОБСТВЕННОЕ подмножество bash-совместимой оболочки, работающее
//! напрямую с нашим терминалом (print!/println!) и буферами ввода
//! (PS/2 клавиатура + COM1 serial через shell::getchar).
//!
//! MVP поддерживает:
//!   - переменные окружения: `set NAME value`, `$NAME`, `unset NAME`
//!   - выходной код последней команды: `$?`
//!   - интеграцию с существующими командами ядра (echo, help, и т.д.)
//!   - команду `exit` для возврата в основной shell
//!
//! Будущие расширения (не в MVP):
//!   - пайпы `cmd1 | cmd2`
//!   - условные операторы `if`/`else`/`fi`
//!   - циклы `for`/`while`
//!   - история команд (стрелки)

use alloc::string::String;
use alloc::string::ToString;
use alloc::collections::BTreeMap;
use spin::Mutex;

/// Глобальное окружение brush: NAME → value.
/// BTreeMap из alloc (не heapless) — переменных может быть много.
static ENV: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// Выходной код последней команды (0 = успех).
static LAST_EXIT_CODE: Mutex<u8> = Mutex::new(0);

/// Запускает интерактивную оболочку brush.
/// Возвращает управление в основной shell при команде `exit`.
pub fn run() {
    crate::println!();
    crate::println!("\x1b[36mbrush\x1b[0m — bash-compatible shell (MVP)");
    crate::println!("Type 'exit' to return to kernel shell");

    loop {
        crate::print!("brush$ ");
        let input = crate::shell::read_line();
        let line = core::str::from_utf8(&input)
            .unwrap_or("")
            .trim_end_matches('\0')
            .trim();

        if line.is_empty() {
            continue;
        }

        // exit — возврат в kernel shell
        if line == "exit" {
            crate::println!("Exiting brush...");
            return;
        }

        // Выполняем строку
        let code = execute_line(line);
        *LAST_EXIT_CODE.lock() = code;
    }
}

/// Выполняет одну строку brush. Возвращает exit code (0 = успех).
fn execute_line(line: &str) -> u8 {
    // 1. set NAME value — установка переменной
    if let Some(rest) = line.strip_prefix("set ") {
        let rest = rest.trim();
        // Формат: set NAME value (value — всё после NAME до конца строки)
        if let Some(space) = rest.find(' ') {
            let name = &rest[..space];
            let value = rest[space..].trim();
            ENV.lock().insert(name.to_string(), value.to_string());
            crate::println!("{}={}", name, value);
        } else {
            // set NAME (без значения) — пустая строка
            ENV.lock().insert(rest.to_string(), String::new());
        }
        return 0;
    }

    // 2. unset NAME — удаление переменной
    if let Some(rest) = line.strip_prefix("unset ") {
        let name = rest.trim();
        ENV.lock().remove(name);
        return 0;
    }

    // 3. env — вывести все переменные
    if line == "env" {
        let env = ENV.lock();
        for (k, v) in env.iter() {
            crate::println!("{}={}", k, v);
        }
        return 0;
    }

    // 4. echo — вывод с подстановкой $VAR и $?
    if let Some(rest) = line.strip_prefix("echo ") {
        let expanded = expand_vars(rest.trim());
        crate::println!("{}", expanded);
        return 0;
    }
    if line == "echo" {
        crate::println!();
        return 0;
    }

    // 5. help — список команд brush
    if line == "help" {
        crate::println!("brush commands:");
        crate::println!("  set NAME VALUE  - set environment variable");
        crate::println!("  unset NAME      - remove variable");
        crate::println!("  env             - list all variables");
        crate::println!("  echo TEXT       - print text ($VAR and $? supported)");
        crate::println!("  exit            - return to kernel shell");
        crate::println!("  help            - this help");
        return 0;
    }

    // 6. Неизвестная команда — пытаемся делегировать в kernel shell?
    //    Пока просто сообщаем.
    crate::println!("brush: unknown command: {}", line);
    127
}

/// Подставляет переменные окружения в строку.
///   $NAME  → значение переменной (или пусто, если не задана)
///   $?     → exit code последней команды (десятичное)
fn expand_vars(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' {
            match chars.peek() {
                Some('?') => {
                    chars.next();
                    let code = *LAST_EXIT_CODE.lock();
                    result.push_str(&code.to_string());
                }
                Some(ch) if ch.is_ascii_alphabetic() || *ch == '_' => {
                    // Собираем имя переменной: [A-Za-z_][A-Za-z0-9_]*
                    let mut name = String::new();
                    while let Some(&ch) = chars.peek() {
                        if ch.is_ascii_alphanumeric() || ch == '_' {
                            name.push(ch);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    let env = ENV.lock();
                    if let Some(val) = env.get(&name) {
                        result.push_str(val);
                    }
                }
                _ => {
                    result.push('$');
                }
            }
        } else {
            result.push(c);
        }
    }

    result
}