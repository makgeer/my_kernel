/* Точка входа — _start из boot.asm, гарантированно первый объект */
ENTRY(_start)

SECTIONS {
    /* Higher-half: Limine требует PHDRs в верхней половине адресного
       пространства (>0x8000000000000000). Базовый адрес 0xFFFFFFFF80000000
       — стандартный higher-half для x86_64 ядер (2M ниже верха). */
    . = 0xFFFFFFFF80000000;

    /* Секция с запросами Limine — ДОЛЖНА быть сохранена */
    .requests : ALIGN(8) {
        KEEP(*(.requests))
        KEEP(*(.requests.start))
        KEEP(*(.requests.end))
    }

    .boot : ALIGN(4K) {
        /* Сохраняем любые загрузочные секции */
        KEEP(*(.multiboot2))
    }

    .text : ALIGN(4K) {
        *(.text*)
    }

    /* Секции с разными правами не должны делить одну страницу памяти.
       Выравнивание до 4K после каждой секции гарантирует, что следующий
       сегмент начинается с новой страницы. */
    . = ALIGN(4K);

    .rodata : ALIGN(4K) {
        *(.rodata*)
        *(.rodata.*)
    }

    . = ALIGN(4K);

    .data : ALIGN(4K) {
        *(.data*)
        /* .got автоматически создаётся линкером; размещаем его в .data,
           чтобы не создавать отдельный PHDR с правами RW на странице .rodata */
        *(.got*)
    }

    . = ALIGN(4K);

    .bss : ALIGN(4K) {
        *(.bss*)
        *(COMMON)
    }
    
    /DISCARD/ : {
        /* Отбрасываем ненужные секции */
        *(.eh_frame)
        *(.note.*)
    }
}
