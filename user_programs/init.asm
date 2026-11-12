; init — первая пользовательская программа (Ring 3).
; Запускается ядром из /bin/init.elf сразу после инициализации подсистем.
; Проверяет базовый путь user space (write + exit) и передаёт управление
; обратно в kernel shell. Сборка: автоматически в build.rs

BITS 64

section .rodata
msg:     db "init: user space ready, handing over to shell", 10
msg_len  equ $ - msg

section .text
global _start
_start:
    ; write(1, msg, msg_len)
    mov eax, 1
    mov edi, 1
    lea rsi, [rel msg]
    mov edx, msg_len
    int 0x80

    ; exit(0) — возврат в kernel shell
    mov eax, 60
    xor edi, edi
    int 0x80

.hang:
    jmp .hang
