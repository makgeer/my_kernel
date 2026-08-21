#!/bin/bash
# Сборка user-программы syscall_test (демонстрация read + ELF с несколькими
# PT_LOAD сегментами). Результат: user_programs/syscall_test.elf.
# Ядро встраивает файл через include_bytes! (команда run_syscall).

set -e

cd "$(dirname "$0")/user_programs"

nasm -f elf64 syscall_test.asm -o syscall_test.o
ld -T syscall_test.ld -o syscall_test.elf syscall_test.o -nostdlib -static

echo "OK: user_programs/syscall_test.elf"
ls -l syscall_test.elf
file syscall_test.elf
echo "--- Program headers (должно быть 2 LOAD: R E и RW) ---"
readelf -l syscall_test.elf | grep -A1 LOAD