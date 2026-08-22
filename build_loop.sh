#!/bin/bash
# Сборка user-программ для теста вытесняющего планировщика.
# Результат: loop_a.elf и loop_b.elf (ET_EXEC, x86_64, static).

set -e

cd "$(dirname "$0")/user_programs"

nasm -f elf64 loop_a.asm -o loop_a.o
ld -T hello.ld -o loop_a.elf loop_a.o -nostdlib -static

nasm -f elf64 loop_b.asm -o loop_b.o
ld -T hello.ld -o loop_b.elf loop_b.o -nostdlib -static

echo "OK: user_programs/loop_a.elf + loop_b.elf"
ls -l loop_a.elf loop_b.elf
