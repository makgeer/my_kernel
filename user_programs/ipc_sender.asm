; Тестовая программа IPC — отправитель (запускается ВТОРЫМ, PID=2).
; Отправляет 1 сообщение процессу PID=1 (получатель), затем exit(0).
; Компиляция: nasm -f elf64 + ld (см. build_ipc.sh).
;
; syscall ipc_send (#40): ipc_send(pid, buf, len)
;   rdi = pid получателя, rsi = указатель на данные, rdx = длина.

BITS 64

section .rodata
msg:    db "HELLO"
msg_len equ $ - msg

section .text
global _start
_start:
    ; ipc_send(1, msg, 5)
    mov eax, 40
    mov rdi, 1          ; PID получателя = 1 (процесс receiver)
    lea rsi, [rel msg]
    mov rdx, msg_len
    int 0x80

    ; exit(0)
    mov eax, 60
    xor rdi, rdi
    int 0x80

hang:
    jmp hang
