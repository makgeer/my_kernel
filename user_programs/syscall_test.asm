; Тестовая user-программа для демонстрации НОВЫХ системных вызовов:
;   read(0, buf, 1)  — неблокирующее чтение из stdin (PS/2 + COM1)
;   write(1, ...)    — вывод на stdout (с безопасным копированием)
;   exit(0)          — завершение и возврат в kernel shell
;
; ВАЖНО: программа имеет ТРИ секции — .rodata (R E), .data (RW),
; .text (R E) — это проверка загрузки ELF с несколькими PT_LOAD
; сегментами с разными правами (src/elf.rs обрабатывает их все).
;
; Сборка: ./build_syscall_test.sh
; Запуск в ядре: команда "run_syscall"

BITS 64

section .rodata
prompt:     db "Press a key:", 10
prompt_len  equ $ - prompt
got_msg:    db "You pressed: "
got_len     equ $ - got_msg
newline:    db 10

section .data
; Буфер для одного нажатого символа — лежит в WRITABLE сегменте,
; поэтому для него нужен отдельный PT_LOAD с флагом W.
key:        db 0

section .text
global _start
_start:
    ; write(1, prompt, prompt_len)
    mov eax, 1
    mov edi, 1
    lea rsi, [rel prompt]
    mov edx, prompt_len
    int 0x80

    ; read(0, key, 1) — БЛОКИРУЮЩИЙ ввод с клавиатуры/COM1.
    ; Ядро спит (hlt) до нажатия клавиши и возвращает 1 байт.
    mov eax, 0
    xor edi, edi          ; fd = 0 (stdin)
    lea rsi, [rel key]
    mov edx, 1
    int 0x80

    ; write(1, got_msg, got_len)
    mov eax, 1
    mov edi, 1
    lea rsi, [rel got_msg]
    mov edx, got_len
    int 0x80

    ; write(1, key, 1) — эхо нажатого символа
    mov eax, 1
    mov edi, 1
    lea rsi, [rel key]
    mov edx, 1
    int 0x80

    ; write(1, newline, 1)
    mov eax, 1
    mov edi, 1
    lea rsi, [rel newline]
    mov edx, 1
    int 0x80

    ; exit(0)
    mov eax, 60
    xor edi, edi
    int 0x80

.hang:
    jmp .hang