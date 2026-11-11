#!/bin/bash
# Сборка user-программ для теста IPC.
# Результат: ipc_sender.elf и ipc_receiver.elf (ET_EXEC, x86_64, static).

set -e

cd "$(dirname "$0")/user_programs"

nasm -f elf64 ipc_sender.asm -o ipc_sender.o
ld -T hello.ld -o ipc_sender.elf ipc_sender.o -nostdlib -static

# ipc_receiver имеет .data (буфер сообщения) — нужен отдельный RW
# PT_LOAD, иначе W^X в ядре сделает код неисполняемым (NX).
nasm -f elf64 ipc_receiver.asm -o ipc_receiver.o
ld -T syscall_test.ld -o ipc_receiver.elf ipc_receiver.o -nostdlib -static

echo "OK: user_programs/ipc_sender.elf + ipc_receiver.elf"
ls -l ipc_sender.elf ipc_receiver.elf
