; arrow_test — тест клавиатуры/COM1 из Ring 3.
; Блокирующе читает по одному байту через read(0, ...) и печатает его
; hex-код в виде "0xNN". Выход — по клавише 'q'.
; Сборка: ./build_all_user.sh

BITS 64

section .rodata
prompt:     db "arrow_test: press keys (q to quit)", 10
prompt_len  equ $ - prompt
hexdigits:  db "0123456789abcdef"

section .data
key:        db 0
out:        db "0x00", 10

section .text
global _start
_start:
    ; write(1, prompt, prompt_len)
    mov eax, 1
    mov edi, 1
    lea rsi, [rel prompt]
    mov edx, prompt_len
    int 0x80

.loop:
    ; read(0, &key, 1) — блокирующий ввод
    mov eax, 0
    xor edi, edi
    lea rsi, [rel key]
    mov edx, 1
    int 0x80

    ; Ошибка/EOF — завершаемся
    cmp rax, 1
    jne .exit

    movzx eax, byte [rel key]
    cmp al, 'q'
    je .exit

    ; out[2] = hex[(key >> 4) & 0xF], out[3] = hex[key & 0xF]
    mov edx, eax
    shr eax, 4
    and edx, 0x0F

    lea rbx, [rel hexdigits]
    mov al, [rbx + rax]
    mov byte [rel out + 2], al
    mov dl, [rbx + rdx]
    mov byte [rel out + 3], dl

    ; write(1, out, 5)
    mov eax, 1
    mov edi, 1
    lea rsi, [rel out]
    mov edx, 5
    int 0x80

    jmp .loop

.exit:
    mov eax, 60
    xor edi, edi
    int 0x80

.hang:
    jmp .hang
