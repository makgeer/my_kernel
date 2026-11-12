; sysinfo — проверка syscalls getpid (#39) и yield (#24) из Ring 3.
; Печатает свой PID в десятичном виде, отдаёт квант планировщику
; и сообщает о возврате. Сборка: автоматически в build.rs

BITS 64

section .rodata
pid_msg:    db "sysinfo: pid="
pid_len     equ $ - pid_msg
newline:    db 10
back_msg:   db "sysinfo: back from yield", 10
back_len    equ $ - back_msg

section .data
; Буфер для десятичного числа; заполняется справа налево.
numbuf:     times 20 db 0

section .text
global _start
_start:
    ; getpid() -> rax = PID
    mov eax, 39
    int 0x80

    ; write(1, pid_msg, pid_len)
    mov r12, rax            ; сохраняем PID (callee-saved)
    mov eax, 1
    mov edi, 1
    lea rsi, [rel pid_msg]
    mov edx, pid_len
    int 0x80

    ; Преобразуем PID в десятичную строку (справа налево).
    mov rax, r12
    lea rdi, [rel numbuf + 20]
    mov rcx, 10
.conv:
    xor rdx, rdx
    div rcx                 ; rax = частное, rdx = остаток
    add dl, '0'
    dec rdi
    mov [rdi], dl
    test rax, rax
    jnz .conv

    ; write(1, rdi, numbuf + 20 - rdi)
    lea rdx, [rel numbuf + 20]
    sub rdx, rdi
    mov rsi, rdi
    mov eax, 1
    mov edi, 1
    int 0x80

    ; Перевод строки
    mov eax, 1
    mov edi, 1
    lea rsi, [rel newline]
    mov edx, 1
    int 0x80

    ; yield() — добровольно отдать квант
    mov eax, 24
    int 0x80

    ; write(1, back_msg, back_len)
    mov eax, 1
    mov edi, 1
    lea rsi, [rel back_msg]
    mov edx, back_len
    int 0x80

    ; exit(0)
    mov eax, 60
    xor edi, edi
    int 0x80

.hang:
    jmp .hang
