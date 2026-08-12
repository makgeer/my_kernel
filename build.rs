use std::process::Command;
use std::env;
use std::path::PathBuf;

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let out_path = PathBuf::from(&out_dir);
    
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let boot_asm = PathBuf::from(manifest_dir).join("src").join("boot.asm");

    // Компиляция boot.asm в объектный файл
    let status = Command::new("nasm")
        .args(&[
            "-f", "elf64",
            boot_asm.to_str().unwrap(),
            "-o", &out_path.join("boot.o").to_string_lossy().to_string()
        ])
        .status()
        .expect("Failed to execute nasm");

    if !status.success() {
        panic!("NASM assembly failed");
    }

    println!("cargo:rustc-link-search=native={}", out_dir);
    println!("cargo:rustc-link-arg={}", out_path.join("boot.o").display());
    println!("cargo:rerun-if-changed=src/boot.asm");
}
