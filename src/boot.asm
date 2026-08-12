; 64-битный entry point для x86_64
; Limine bootloader уже переводит CPU в 64-битный режим до передачи управления
; Этот код только настраивает стек и передаёт управление Rust

global _start
extern _start_rust

section .text
bits 64

_start:
    ; Limine уже настроил GDT, paging, и перевёл CPU в long mode.
    ; Нам нужно только установить стек и вызвать Rust entry point.
    
    ; Устанавливаем стек на вершину BSS-области (стек растёт вниз)
    lea rsp, [rel stack_top]
    
    ; Очищаем регистры, которые могут быть установлены загрузчиком
    xor eax, eax
    xor ebx, ebx
    xor ecx, ecx
    xor edx, edx
    xor esi, esi
    xor edi, edi
    xor r8, r8
    xor r9, r9
    xor r10, r10
    xor r11, r11
    xor r12, r12
    xor r13, r13
    xor r14, r14
    xor r15, r15
    
    ; Передаём управление Rust
    call _start_rust
    
    ; Если Rust вернул управление — останавливаем CPU
    cli
    hlt

section .bss
    align 16
    ; Буфер стека 16 КБ — достаточно для ранней инициализации
    stack_bottom:
        resb 16384
    stack_top:
        ; stack_top указывает на вершину стековой области
