#!/bin/bash
# Сборка пользовательской ELF-программы для ядра malinka.
# Результат: user_programs/hello.elf (ET_EXEC, x86_64, static).
# Ядро встраивает этот файл через include_bytes! (команда run_elf).

set -e

cd "$(dirname "$0")/user_programs"

nasm -f elf64 hello.asm -o hello.o
ld -T hello.ld -o hello.elf hello.o -nostdlib -static

echo "OK: user_programs/hello.elf"
ls -l hello.elf
file hello.elf
readelf -l hello.elf | grep -A1 LOAD
