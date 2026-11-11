#!/bin/bash
# Сборка всех пользовательских ELF-программ из user_programs/*.asm.
# Результат: user_programs/<name>.elf (ET_EXEC, x86_64, static).
# Ядро встраивает их через include_bytes! (initrd) и запускает из shell.
#
# Требования: nasm, GNU ld.
# Запуск: ./build_all_user.sh

set -e

cd "$(dirname "$0")/user_programs"

for src in *.asm; do
    name="${src%.asm}"

    # Программы с секцией .data требуют ОТДЕЛЬНОГО PT_LOAD для данных:
    # ядро применяет W^X (writable → NO_EXECUTE), поэтому RWX-сегмент
    # сделал бы код неисполняемым. syscall_test.ld разделяет R E и RW.
    case "$name" in
        syscall_test|ipc_receiver|arrow_test|sysinfo|keyboardd|seriald)
            ld_script="syscall_test.ld" ;;
        *)
            ld_script="hello.ld" ;;
    esac

    nasm -f elf64 "$src" -o "$name.o"
    ld -T "$ld_script" -o "$name.elf" "$name.o" -nostdlib -static
    echo "OK: user_programs/$name.elf"
done

ls -l ./*.elf
