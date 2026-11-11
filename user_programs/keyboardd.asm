; keyboardd — драйвер PS/2 клавиатуры в USER SPACE (Ring 3).
;
; Демонстрирует микроядерную архитектуру: ядро только доставляет
; прерывание и разрешает доступ к порту 0x60, а вся логика разбора
; скан-кодов живёт в пользовательском процессе.
;
; Протокол:
;   1. irq_claim(1)  (#44) — заявить владение IRQ1; ядро перестаёт
;      само читать порт 0x60.
;   2. irq_wait(1)   (#42) — блокирующее ожидание прерывания клавиатуры.
;   3. io_in(0x60)   (#43) — прочитать скан-код (capability на порт).
;   4. Разбор: отпускание (бит 0x7) игнорируется, скан-код переводится
;      в ASCII по таблице US-раскладки.
;   5. ipc_send(0, &char, 1) (#40) — отправить символ консольному
;      сервису ядра; ядро печатает его и кладёт в буфер ввода shell.
;
; Выход — по клавише Esc (скан-код 0x01).
;
; Сборка: ./build_all_user.sh (линкуется через syscall_test.ld —
; .data должен быть в отдельном RW-сегменте из-за W^X).

BITS 64

section .rodata
; Таблица US-раскладки: индекс = скан-код (0x00..0x39), значение = ASCII.
; 0 = клавиша без символа (модификаторы, F-клавиши и т.п.).
scancode_map:
    db 0, 27, '1', '2', '3', '4', '5', '6', '7', '8', '9', '0', '-', '=', 8, 9
    db 'q', 'w', 'e', 'r', 't', 'y', 'u', 'i', 'o', 'p', '[', ']', 10, 0
    db 'a', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l', ';', 39, '`', 0, 92
    db 'z', 'x', 'c', 'v', 'b', 'n', 'm', ',', '.', '/', 0, '*', 0, ' '
MAP_LEN equ $ - scancode_map

banner:     db "keyboardd: PS/2 driver in ring 3 (Esc to stop)", 10
banner_len  equ $ - banner
stop_msg:   db 10, "keyboardd: stopped", 10
stop_len    equ $ - stop_msg
err_msg:    db "keyboardd: irq_claim failed", 10
err_len     equ $ - err_msg

section .data
char_buf:   db 0

section .text
global _start
_start:
    ; Сообщаем о старте в консоль ядра (PID 1 — канал вывода).
    mov eax, 40
    mov edi, 1
    lea rsi, [rel banner]
    mov edx, banner_len
    int 0x80

    ; irq_claim(1) — забираем IRQ1 у ядра.
    mov eax, 44
    mov edi, 1
    int 0x80
    test rax, rax
    jnz .claim_failed

.main_loop:
    ; irq_wait(1) — спим до прерывания клавиатуры.
    mov eax, 42
    mov edi, 1
    int 0x80
    test rax, rax
    jnz .main_loop          ; ошибка/спонтанное пробуждение — ждём снова

    ; io_in(0x60) — читаем скан-код.
    mov eax, 43
    mov edi, 0x60
    int 0x80
    cmp rax, 0xFF           ; -1 = доступ запрещён
    je .main_loop

    ; Отпускание клавиши: бит 0x80 установлен — пропускаем.
    test al, 0x80
    jnz .main_loop

    ; Esc (0x01) — завершаем драйвер, возвращая IRQ1 ядру.
    cmp al, 0x01
    je .exit

    ; Границы таблицы.
    movzx ebx, al
    cmp ebx, MAP_LEN
    jae .main_loop

    ; Перевод скан-кода в ASCII.
    lea rcx, [rel scancode_map]
    movzx eax, byte [rcx + rbx]
    test al, al
    jz .main_loop           ; клавиша без символа

    ; ipc_send(0, &char, 1) — отправляем символ в консоль ядра.
    mov byte [rel char_buf], al
    mov eax, 40
    xor edi, edi
    lea rsi, [rel char_buf]
    mov edx, 1
    int 0x80

    jmp .main_loop

.claim_failed:
    mov eax, 40
    mov edi, 1
    lea rsi, [rel err_msg]
    mov edx, err_len
    int 0x80
    jmp .exit

.exit:
    ; Возвращаем IRQ1 ядру: при завершении процесса ядро снимает
    ; владение IRQ (см. terminate_current в process_manager).
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
