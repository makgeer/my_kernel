# KODA.md — Контекст проекта malinka

## Обзор проекта

**malinka** — учебное ядро операционной системы на Rust для архитектуры x86_64 (bare-metal, `no_std` / `no_main`). Загружается через загрузчик **Limine** в higher-half режиме (базовый адрес `0xFFFFFFFF80000000`). Ядро реализует базовую инфраструктуру ОС: вывод на framebuffer/VGA/serial, обработку прерываний (PIC, IDT, GDT/TSS/IST), кооперативный планировщик задач и интерактивный shell.

### Текущее состояние

- Загрузка через Limine BIOS ✅
- Вывод: VGA text buffer (0xB8000) + Serial COM1 + Framebuffer (через Limine, шрифт 8x16) ✅
- GDT + TSS + IST (отдельные стеки для обработчиков прерываний) ✅
- IDT: breakpoint, double_fault, page_fault, timer (IRQ0), keyboard (IRQ1), syscall (int 0x80) ✅
- PIC: master + slave, полная инициализация, маски (только IRQ0 и IRQ1) ✅
- Прерывания стабильны (DOUBLE FAULT устранён) ✅
- Кооперативный планировщик (до 4 задач) ✅
- Heap-аллокатор (8 МБ, `linked_list_allocator`) ✅
- PMM: bitmap-аллокатор физических фреймов 4K (alloc_frame/dealloc_frame) ✅
- Paging: map_page/unmap_page/translate, create_user_p4, map_page_in_p4 ✅
- Пользовательские процессы: create_process, create_elf_process, run_process (ring 3) ✅
- Syscall: int 0x80 (DPL=3), asm-обёртка syscall_entry, write/exit ✅
- ELF64-загрузчик: парсер ELF64 + загрузка PT_LOAD сегментов (`src/elf.rs`) ✅
- User-программы: `user_programs/hello.asm` (NASM → статический ELF64) ✅
- Интерактивный shell: help, echo, about, tasks, clear, cursor, color, reboot, hlt, test_alloc, test_pmm, test_paging, test_process, run_user, run_elf ✅
- Ввод: PS/2 клавиатура (IRQ1) + Serial COM1 ✅
- Модуль терминала `terminal.rs` создан и компилируется, но **не интегрирован** в макросы `print!`/`println!`
- **Известная проблема**: `run_user` / `run_elf` — DOUBLE FAULT при входе в ring 3 (CR2=0x400000000000, маппинги проверены — замаплены корректно, причина в процессе отладки)

## Технологии и зависимости

| Компонент | Версия | Назначение |
|-----------|--------|------------|
| Rust (nightly) | — | Язык реализации, требуется `#![feature(abi_x86_interrupt)]` |
| `x86_64` | 0.15.2 | API для работы с портами, сегментами, IDT, GDT, TSS |
| `spin` | 0.9.8 | `Mutex` и `Once` без аллокации (no_std) |
| `heapless` | 0.7.16 | `Vec<u8, 256>` для буфера клавиатуры без аллокации |
| `limine` | 0.6.5 | Limine Boot Protocol (запрос framebuffer, HHDM) |
| `volatile` | 0.6.1 | Зарезервировано (не используется активно) |
| `linked_list_allocator` | 0.10.x | Heap-аллокатор (8 МБ, `LockedHeap`) |
| Limine (загрузчик) | 12.5.2 | Передаёт управление в higher-half, предоставляет framebuffer и HHDM |
| NASM | — | Компиляция `boot.asm` (entry point) и `user_programs/hello.asm` |
| GNU ld | — | Линковка user-программ (`build_user.sh`) |
| xorriso | — | Создание загрузочного ISO |
| Bochs | — | Эмулятор (запуск вручную: `./bochs-bin/bin/bochs -q -f bochsrc.bxrc`) |

## Структура проекта

```
malinka/
├── .cargo/
│   └── config.toml          — цель сборки x86_64-unknown-none, build-std, флаги линкера
├── .gitignore               — исключения (target, Bochs, .kodaruls, boot.iso, ...)
├── Cargo.toml               — манифест пакета
├── build.rs                 — компиляция boot.asm через NASM (elf64)
├── linker.ld                — линкер-скрипт (higher-half, секция .requests для Limine)
├── arhiv/                   — архив неиспользуемого: бэкапы (.bak), i686-unknown-none.json,
│                              debug_kernel.sh, limine.cfg, логи QEMU/Bochs, временные файлы
├── make_iso.sh              — сборка ISO через xorriso + Limine (аргумент: no-fb)
├── plan.txt                 — план разработки (задачи на текущую итерацию)
├── my_kernel.log            — лог всех изменений (только дописывается, с 2025-08-10)
├── build_user.sh            — сборка user-программы hello (nasm + ld) → hello.elf
├── iso/
│   ├── kernel.elf           — собранный бинарник ядра
│   ├── limine.conf          — конфигурация Limine (PROTOCOL, KERNEL_PATH, SERIAL, VERBOSE)
│   ├── limine-bios.sys      — бинарник Limine BIOS
│   └── limine-bios-cd.bin   — загрузочный образ Limine для CD
├── limine-binary/           — исходные бинарники Limine
├── user_programs/           — пользовательские программы (ring 3)
│   ├── hello.asm            — тестовая программа: write() + exit() через int 0x80
│   ├── hello.ld             — линкер-скрипт (база 0x400000000000 = USER_CODE_BASE)
│   └── hello.elf            — собранный статический ET_EXEC (include_bytes! в shell)
└── src/
    ├── main.rs              — точка входа _start_rust, инициализация подсистем, запуск shell
    ├── boot.asm             — 64-битный entry point (установка стека, вызов _start_rust)
    ├── vga.rs               — VgaWriter + макросы print!/println! (вывод в VGA + serial + framebuffer)
    ├── serial.rs            — драйвер UART 16550 (COM1): init, write, read, SerialWriter
    ├── vesa.rs              — вывод на framebuffer: шрифт 8x16, vesa_draw_char/ex, vesa_print_str, VesaWriter
    ├── limine_fb.rs         — запросы Limine: FramebufferRequest, HhdmRequest
    ├── gdt.rs               — GDT + TSS + IST (стеки для #DF, таймера, клавиатуры), kernel_panic
    ├── interrupts.rs        — IDT (breakpoint, double_fault, page_fault, timer, keyboard, syscall), PIC init
    ├── scheduler.rs         — кооперативный планировщик (до 4 задач, run_scheduler / run_scheduler_once)
    ├── tasks.rs             — демо-задачи: task_hello, task_counter
    ├── shell.rs             — интерактивный shell: getchar (PS/2 + COM1), команды (в т.ч. run_elf)
    ├── terminal.rs          — полноценный терминал 80x25 (ANSI, курсор, рендер) — НЕ интегрирован в print!
    ├── allocator.rs         — heap-аллокатор 8 МБ (linked_list_allocator, LockedHeap)
    ├── pmm.rs               — PMM: bitmap-аллокатор физических фреймов 4K (alloc_frame/dealloc_frame)
    ├── paging.rs            — пейджинг: map_page/unmap_page/translate, create_user_p4, map_page_in_p4
    ├── process.rs           — процессы: create_process, create_elf_process, run_process, user_mode_entry
    └── elf.rs               — ELF64-загрузчик: парсер Ehdr/Phdr + загрузка PT_LOAD сегментов
```

## Архитектура

### Поток загрузки

1. **Limine** загружает `kernel.elf` из ISO, переводит CPU в long mode (64-бит), мапит физическую память через HHDM, передаёт управление на `_start` (из `boot.asm`).
2. **boot.asm**: устанавливает RSP на `stack_top` (16 КБ BSS-стек), очищает регистры, вызывает `_start_rust`.
3. **main.rs `_start_rust`**:
   - `serial::init()` — инициализация COM1 (до первого вывода)
   - `allocator::init_heap()` — heap 8 МБ (до первых аллокаций)
   - `pmm::init()` + `paging::init()` — PMM + пейджинг (HHDM, kernel P4)
   - Запрос framebuffer у Limine → `vesa_set_framebuffer` + `vesa_clear_screen`
   - `gdt::init_gdt()` — загрузка своей GDT + TSS (IST-стеки), установка CS/DS/SS
   - `interrupts::init_idt()` — загрузка IDT, инициализация PIC, `sti`
   - `add_task()` — регистрация демо-задач
   - `shell::shell()` — интерактивная оболочка

### Вывод

Макрос `print!` (в `vga.rs`) дублирует вывод в три устройства одновременно:
- `VgaWriter` → VGA text buffer `0xB8000`
- `SERIAL` → COM1 (serial)
- `VesaWriter` → Limine framebuffer (рисует сразу, без буфера)

Модуль `terminal.rs` реализует полноценный терминал (offscreen-буфер 80x25, ANSI SGR, курсор, прокрутка) и рендер во все три устройства, но **пока не подключён** к `print!` — это следующая задача по плану.

### Прерывания

- **PIC**: master (0x20-0x27) + slave (0x28-0x2F), маски — разрешены только IRQ0 (таймер) и IRQ1 (клавиатура)
- **IDT**: breakpoint (#3), double_fault (#DF, IST 1), page_fault (#PF), timer (IRQ0, IST 2), keyboard (IRQ1, IST 3), syscall (int 0x80, DPL=3)
- **IST-стеки**: `DOUBLE_FAULT_STACK` (16 КБ), `TIMER_STACK` (8 КБ), `KEYBOARD_STACK` (8 КБ) — обработчики не зависят от глубины стека прерванного кода
- **#PF handler**: диагностика (CR2, RIP, CS/SS, RFLAGS); при PF в user mode — kernel_panic (восстановление kernel CR3 + HLT)
- **syscall int 0x80**: asm-обёртка `syscall_entry` (naked) сохраняет все GPR → `syscall_handler_rust` (write/exit)
- Клавиатура: US-раскладка (нижний регистр, без Shift), буфер `heapless::Vec<u8, 256>`

### Планировщик

Кооперативный, до 4 задач (`Mutex<[Option<Task>; 4]>`). `run_scheduler_once()` выполняет один проход всех задач — вызывается из shell командой `tasks`. Бесконечный `run_scheduler()` оставлен в коде, но не используется (shell и задачи кооперативны в одном потоке).

## Сборка и запуск

### Целевая платформа

- **Цель**: `x86_64-unknown-none` (bare-metal, `build-std = ["core", "compiler_builtins"]`)
- **Линкер**: `rust-lld` с флагами: `-Tlinker.ld -nostdlib -static -z execstack -m elf_x86_64 --no-pie`
- **Nightly Rust** требуется из-за `#![feature(abi_x86_interrupt)]`

### Команды

```bash
# Проверка компиляции (без линковки)
cargo check

# Сборка (debug)
cargo build

# Сборка (release, с LTO)
cargo build --release

# Создание загрузочного ISO (через make_iso.sh)
./make_iso.sh
# Режим без framebuffer (для Bochs term/ncurses):
./make_iso.sh no-fb

# Сборка пользовательской ELF-программы (для команды run_elf)
# → user_programs/hello.elf (встраивается в ядро через include_bytes!)
./build_user.sh

# или вручную:
cargo build --release
cp target/x86_64-unknown-none/release/malinka iso/kernel.elf
# затем xorriso для создания boot.iso (см. make_iso.sh)
```

### Запуск ядра (строго вручную в системном терминале!)

```bash
# Единственный способ запуска — Bochs (локальная сборка с --with-nogui/--with-term)
./bochs-bin/bin/bochs -q -f bochsrc.bxrc
```

### Тестирование

`cargo test` **не работает** — цель `x86_64-unknown-none` (bare-metal) не имеет `libtest`. Тестирование проводится вручную через запуск в Bochs и анализ serial-вывода. В проекте нет `#[test]`-функций.

## Правила разработки (.kodaruls)

Файл `.kodaruls` — локальный, **в git не хранится** (в `.gitignore`). Правила для справки:

1. **Одна операция в один момент времени** — никаких глобальных переписок, только точечные исправления.
2. **Никогда не переписывать файл целиком** — строго точечные исправления проблемного участка.
3. **Всегда проверять зависимый код и файлы** перед внесением изменений.
4. **Никогда не применять исправления без объяснения** происходящего в коде.
5. **Обязательное логирование** — каждое действие записывается в `my_kernel.log` (только дописывать, никаких перезаписей).
6. **Новый код выделять бирюзовым цветом** (до внесения в коммит) — для понимания внесённых изменений.
7. **Объяснять рациональность** любого предложения по применению.
8. **Эмулятор — только Bochs** (`./bochs-bin/bin/bochs`). QEMU полностью исключён из проекта: не использовать, не упоминать в скриптах, документации и логах.

### Дополнительные правила (из системного контекста)

- **Запуск ядра строго вручную** в системном терминале — не запускать Bochs из AI-ассистента.
- **Эмулятор — только Bochs** (`./bochs-bin/bin/bochs`). QEMU полностью исключён из проекта (правило 8 `.kodaruls`): не используется, не упоминается в скриптах, документации и логах.
- **Запрещено удалять любой код** (даже неиспользуемый) — требуется проверка на валидность и введение в основное тело программы. Удаление строго с разрешения человека.
- Бэкап-файлы (`*.bak`), неиспользуемые цели (`i686-unknown-none.json`) и устаревшие скрипты/логи перенесены в `arhiv/` — актуальный код от них очищен.

## Стиль кодирования

- **Язык комментариев**: русский (включая пояснения архитектуры и причин решений)
- **Именование**: `snake_case` для функций и переменных, `SCREAMING_SNAKE_CASE` для констант и статиков
- **Без аллокации**: `no_std`, `spin::Mutex` вместо `std::sync`, `heapless::Vec` вместо `alloc::vec::Vec`
- **Безопасность**: `unsafe` блоки снабжены комментариями-обоснованиями; `#[allow(...)]` для известного dead code (старый VESA BIOS, бэкапы)
- **Макросы**: `print!`/`println!` определены в `vga.rs`, экспортируются через `#[macro_export]`
- **Модули**: все модули объявлены в `main.rs` через `mod`

## Ключевые архитектурные решения (история)

- **Limine вместо GRUB/Multiboot2**: переход на Limine Boot Protocol для нативной поддержки framebuffer и HHDM в higher-half
- **Higher-half (0xFFFFFFFF80000000)**: Limine требует PHDRs в верхней половине адресного пространства
- **HHDM**: Limine отображает всю физическую память в higher-half; `fb.address()` уже возвращает виртуальный адрес (Limine сам применяет HHDM offset)
- **IST-стеки**: устраняют DOUBLE FAULT при переполнении стека ядра во время прерывания
- **SS::set_reg(data_sel)**: критически важно — Limine оставляет свой SS (0x30), при прерывании CPU кладёт его в IRET-фрейм, а при `iretq` не находит в нашей GDT → #GP → #DF
- **PIC маскирование**: 0xFC (master) + 0xFF (slave) — без маски IRQ8 (RTC) периодически срабатывает без обработчика → DOUBLE FAULT
- **Двойной ввод**: getchar() читает и PS/2 (порт 0x60), и COM1 — для работы shell как в графическом режиме, так и с `-serial stdio`
- **create_user_p4 копирует ТОЛЬКО higher-half (256-511)**: копирование всего kernel P4 ломало ring 3 — user-код мапился в kernel P3/P2/P1 без `USER_ACCESSIBLE`, и ring 3 блокировался на верхнем уровне иерархии → #PF
- **VGA buffer (0xB8000) мапится в user P4 отдельно**: обработчики прерываний (#PF/#DF/syscall) при активной user CR3 вызывают println!, который пишет в VGA; U/S=0 — ring 3 доступа не получит
- **run_process → iretq → user_mode_entry → CR3::write(user P4) → iretq → ring 3**: двухэтапный переход с промежуточной точкой в kernel, чтобы переключить адресное пространство ДО входа в user code
- **ELF-загрузчик**: PT_LOAD сегменты мапятся по адресам из ELF header (не фиксированный USER_CODE_BASE), флаги страниц из p_flags (PF_W→WRITABLE, !PF_X→NO_EXECUTE)

## План развития (из plan.txt)

1. **Интеграция терминала** — перенаправить `print!`/`println!` на `terminal::TERMINAL` вместо трёх независимых `write!`
2. **Тестирование терминала в Bochs** — ANSI-цвета, курсор, прокрутка, команды shell
3. **Команда `color` в shell** — демонстрация 16-цветной палитры через SGR (реализована, требует проверки)
4. **Оптимизация рендера** — dirty-флаг вместо полной перерисовки 80x25 на каждый `write!`
5. **Отладка ring 3** — устранить DOUBLE FAULT при входе в user mode (CR2=0x400000000000, маппинги проверены — замаплены корректно)
