; Тестовая пользовательская программа для загрузчика ELF ядра malinka.
; Компиляция: nasm -f elf64 + ld (см. build_user.sh).
; Вызывает системные вызовы через int 0x80:
;   eax = номер (1 = write, 60 = exit)
;   rdi = arg1, rsi = arg2, rdx = arg3

BITS 64

section .rodata
msg:    db "Hello from ELF user program!", 10
msg_len equ $ - msg

section .text
global _start
_start:
    ; write(1, msg, msg_len)
    mov eax, 1
    mov rdi, 1
    lea rsi, [rel msg]
    mov rdx, msg_len
    int 0x80

    ; exit(0)
    mov eax, 60
    xor rdi, rdi
    int 0x80

hang:
    jmp hang
