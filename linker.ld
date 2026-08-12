/* Точка входа — _start из boot.asm, гарантированно первый объект */
ENTRY(_start)

SECTIONS {
    . = 2M;

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

    .rodata : ALIGN(4K) {
        *(.rodata*)
        *(.rodata.*)
    }

    .data : ALIGN(4K) {
        *(.data*)
    }

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

    .rodata : ALIGN(4K) {
        *(.rodata*)
    }

    .data : ALIGN(4K) {
        *(.data*)
        /* Limine requests section */
        KEEP(*(.requests))
        KEEP(*(.requests_start_marker))
        KEEP(*(.requests_end_marker))
    }

    .bss : ALIGN(4K) {
        *(.bss*)
        *(COMMON)
    }

    /DISCARD/ : {
        *(.eh_frame)
        *(.note*)
    }
}
