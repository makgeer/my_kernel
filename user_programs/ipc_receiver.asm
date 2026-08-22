; Тестовая программа IPC — получатель (запускается ПЕРВЫМ, PID=1).
; Вызывает ipc_recv → блокируется (нет сообщений) → переключение на
; отправителя. Когда отправитель пошлёт сообщение и завершится, планировщик
; вернёт управление сюда — процесс повторит int 0x80 → ipc_recv получит
; сообщение. Выводит его через write, затем exit(0).
;
; syscall ipc_recv (#41): ipc_recv(buf, len)
;   rdi = указатель на буфер, rsi = размер буфера.
;   Возвращает: rax = from_pid, rdx = длина сообщения.

BITS 64

section .data
buf:    times 32 db 0   ; буфер для приёма сообщений

section .text
global _start
_start:
    ; ipc_recv(buf, 32) — блокируется, если сообщений нет.
    mov eax, 41
    lea rdi, [rel buf]
    mov rsi, 32
    int 0x80

    ; Проверяем результат: rax = from_pid, rdx = длина
    cmp rax, 0xFFFFFFFFFFFFFFFF
    je .no_more          ; -1: нет сообщений (ошибка)

    ; Сохраняем длину сообщения (rdx) в r13 (callee-saved).
    mov r13, rdx

    ; write(1, buf, r13) — выводим содержимое
    mov eax, 1
    mov rdi, 1
    lea rsi, [rel buf]
    mov rdx, r13
    int 0x80

    ; exit(0)
    mov eax, 60
    xor rdi, rdi
    int 0x80

.no_more:
    ; exit(1) — не получили сообщение
    mov eax, 60
    mov rdi, 1
    int 0x80

hang:
    jmp hang
