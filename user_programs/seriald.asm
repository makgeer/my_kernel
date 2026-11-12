; seriald — драйвер COM1 (UART 16550) в USER SPACE (Ring 3).
;
; Второй пример выноса драйвера: ядро отдаёт IRQ4 и порты 0x3F8..0x3FF,
; а приём байтов и передача их в консоль выполняются в ring 3.
;
; В отличие от keyboardd, этот драйвер можно проверить в headless-Bochs:
; ввод в `nc localhost 8888` приходит на COM1 и генерирует IRQ4.
;
; Протокол:
;   irq_claim(4)   — забираем IRQ4 у ядра (serial_irq_handler перестаёт
;                    читать UART и только будит драйвер);
;   irq_wait(4)    — ждём прерывание;
;   io_in(0x3FD)   — LSR: проверяем бит 0 (данные готовы);
;   io_in(0x3F8)   — читаем байт;
;   ipc_send(0, &b, 1) — отдаём байт в канал ввода ядра (shell).
;
; Выход — по Ctrl-C (0x03).
;
; Сборка: автоматически в build.rs.

BITS 64

section .rodata
banner:     db "seriald: COM1 driver in ring 3 (Ctrl-C to stop)", 10
banner_len  equ $ - banner
stop_msg:   db 10, "seriald: stopped, IRQ4 back to kernel", 10
stop_len    equ $ - stop_msg

section .data
byte_buf:   db 0

section .text
global _start
_start:
    ; Диагностика в консоль ядра (PID 1 — канал вывода).
    mov eax, 40
    mov edi, 1
    lea rsi, [rel banner]
    mov edx, banner_len
    int 0x80

    ; irq_claim(4) — забираем IRQ4 (COM1).
    mov eax, 44
    mov edi, 4
    int 0x80
    test rax, rax
    jnz .exit

.main_loop:
    ; irq_wait(4) — спим до прерывания UART.
    mov eax, 42
    mov edi, 4
    int 0x80
    test rax, rax
    jnz .main_loop

    ; Вычитываем все готовые байты из FIFO.
.drain:
    mov eax, 43
    mov edi, 0x3FD          ; LSR
    int 0x80
    cmp rax, 0xFF
    je .main_loop
    test al, 0x01           ; Data Ready
    jz .main_loop

    mov eax, 43
    mov edi, 0x3F8          ; RBR — принятый байт
    int 0x80
    cmp rax, 0xFF
    je .main_loop

    cmp al, 0x03            ; Ctrl-C — завершение
    je .exit

    ; ipc_send(0, &byte, 1) — байт в канал ввода ядра.
    mov byte [rel byte_buf], al
    mov eax, 40
    xor edi, edi
    lea rsi, [rel byte_buf]
    mov edx, 1
    int 0x80

    jmp .drain

.exit:
    mov eax, 40
    mov edi, 1
    lea rsi, [rel stop_msg]
    mov edx, stop_len
    int 0x80

    mov eax, 60
    xor edi, edi
    int 0x80

.hang:
    jmp .hang
