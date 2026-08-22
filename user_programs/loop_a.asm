; Тестовая программа для вытесняющего планировщика.
; Печатает "A" 20 раз с задержкой, затем завершается через exit(0).
; Компиляция: nasm -f elf64 + ld (см. build_loop.sh).

BITS 64

section .rodata
msg:    db "A"
msg_len equ $ - msg

section .text
global _start
_start:
    mov r12, 20          ; счётчик итераций
.loop:
    ; write(1, msg, 1)
    mov eax, 1
    mov rdi, 1
    lea rsi, [rel msg]
    mov rdx, msg_len
    int 0x80

    ; Задержка: ~2 млн итераций пустого цикла (rbx — callee-saved,
    ; naked timer handler не портит callee-saved регистры корректно).
    mov rbx, 2000000
.delay:
    dec rbx
    jnz .delay

    dec r12
    jnz .loop

    ; exit(0)
    mov eax, 60
    xor rdi, rdi
    int 0x80

hang:
    jmp hang
