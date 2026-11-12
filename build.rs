use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Ищет инструмент в PATH, затем в ~/.local/bin (пользовательская
/// установка без root). Возвращает имя/путь для Command::new.
fn find_tool(name: &str) -> String {
    if Command::new(name).arg("-v").output().is_ok() {
        return name.to_string();
    }
    if let Ok(home) = env::var("HOME") {
        let local = PathBuf::from(home).join(".local/bin").join(name);
        if local.exists() {
            return local.to_string_lossy().to_string();
        }
    }
    name.to_string()
}

/// Проверяет, что target новее всех зависимостей.
fn up_to_date(target: &Path, deps: &[&Path]) -> bool {
    let target_time = match fs::metadata(target).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return false,
    };
    deps.iter().all(|dep| {
        fs::metadata(dep)
            .and_then(|m| m.modified())
            .map(|t| target_time >= t)
            .unwrap_or(false)
    })
}

/// Собирает пользовательские ELF-программы из user_programs/*.asm.
///
/// Нужно до компиляции ядра: src/fs_initrd.rs и src/tests.rs встраивают
/// готовые .elf через include_bytes!. Раньше требовался ручной запуск
/// ./build_all_user.sh, и чистый клон без ELF не собирался.
fn build_user_programs(manifest_dir: &Path) {
    let dir = manifest_dir.join("user_programs");
    if !dir.is_dir() {
        return;
    }
    // Новый .asm в каталоге тоже должен вызывать перезапуск build.rs.
    println!("cargo:rerun-if-changed=user_programs");

    let nasm = find_tool("nasm");
    let ld = find_tool("ld");

    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let asm = entry.path();
        if asm.extension().and_then(|e| e.to_str()) != Some("asm") {
            continue;
        }
        let name = match asm.file_stem().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };

        // Программы с секцией .data линкуются через syscall_test.ld:
        // ядро применяет W^X, поэтому код (R E) и данные (RW) должны
        // быть отдельными PT_LOAD-сегментами.
        let ld_script_name = match name.as_str() {
            "syscall_test" | "ipc_receiver" | "arrow_test" | "sysinfo"
            | "keyboardd" | "seriald" => "syscall_test.ld",
            _ => "hello.ld",
        };
        let ld_script = dir.join(ld_script_name);
        let elf = dir.join(format!("{}.elf", name));
        let obj = dir.join(format!("{}.o", name));

        println!("cargo:rerun-if-changed={}", asm.display());
        println!("cargo:rerun-if-changed={}", ld_script.display());

        if up_to_date(&elf, &[&asm, &ld_script]) {
            continue;
        }

        let status = Command::new(&nasm)
            .args(["-f", "elf64"])
            .arg(&asm)
            .arg("-o")
            .arg(&obj)
            .status()
            .unwrap_or_else(|e| panic!("Failed to execute nasm ({}): {}", nasm, e));
        if !status.success() {
            panic!("NASM failed for {}", asm.display());
        }

        let status = Command::new(&ld)
            .arg("-T")
            .arg(&ld_script)
            .arg("-o")
            .arg(&elf)
            .arg(&obj)
            .args(["-nostdlib", "-static"])
            .status()
            .unwrap_or_else(|e| panic!("Failed to execute ld ({}): {}", ld, e));
        if !status.success() {
            panic!("LD failed for {}", elf.display());
        }
    }
}

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let out_path = PathBuf::from(&out_dir);

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let manifest_path = PathBuf::from(&manifest_dir);
    let boot_asm = manifest_path.join("src").join("boot.asm");

    // Пользовательские программы — до include_bytes! в основном крейте.
    build_user_programs(&manifest_path);

    // Компиляция boot.asm в объектный файл
    let nasm = find_tool("nasm");
    let status = Command::new(&nasm)
        .args([
            "-f",
            "elf64",
            boot_asm.to_str().unwrap(),
            "-o",
            &out_path.join("boot.o").to_string_lossy().to_string(),
        ])
        .status()
        .unwrap_or_else(|e| panic!("Failed to execute nasm ({}): {}", nasm, e));

    if !status.success() {
        panic!("NASM assembly failed");
    }

    println!("cargo:rustc-link-search=native={}", out_dir);
    println!("cargo:rustc-link-arg={}", out_path.join("boot.o").display());
    println!("cargo:rerun-if-changed=src/boot.asm");
}
