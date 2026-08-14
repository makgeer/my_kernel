# KODA.md — Контекст проекта my_kernel

## Обзор проекта

**my_kernel** — учебное ядро операционной системы на Rust для архитектуры x86_64 (bare-metal, `no_std` / `no_main`). Загружается через загрузчик **Limine** в higher-half режиме (базовый адрес `0xFFFFFFFF80000000`). Ядро реализует базовую инфраструктуру ОС: вывод на framebuffer/VGA/serial, обработку прерываний (PIC, IDT, GDT/TSS/IST), кооперативный планировщик задач и интерактивный shell.

### Текущее состояние

- Загрузка через Limine BIOS ✅
- Вывод: VGA text buffer (0xB8000) + Serial COM1 + Framebuffer (через Limine, шрифт 8x16) ✅
- GDT + TSS + IST (отдельные стеки для обработчиков прерываний) ✅
- IDT: breakpoint, double_fault, timer (IRQ0), keyboard (IRQ1) ✅
- PIC: master + slave, полная инициализация, маски (только IRQ0 и IRQ1) ✅
- Прерывания стабильны (DOUBLE FAULT устранён) ✅
- Кооперативный планировщик (до 4 задач) ✅
- Интерактивный shell: help, echo, about, tasks, clear ✅
- Ввод: PS/2 клавиатура (IRQ1) + Serial COM1 ✅
- Модуль терминала `terminal.rs` создан и компилируется, но **не интегрирован** в макросы `print!`/`println!`

## Технологии и зависимости

| Компонент | Версия | Назначение |
|-----------|--------|------------|
| Rust (nightly) | — | Язык реализации, требуется `#![feature(abi_x86_interrupt)]` |
| `x86_64` | 0.15.2 | API для работы с портами, сегментами, IDT, GDT, TSS |
| `spin` | 0.9.8 | `Mutex` и `Once` без аллокации (no_std) |
| `heapless` | 0.7.16 | `Vec<u8, 256>` для буфера клавиатуры без аллокации |
| `limine` | 0.6.5 | Limine Boot Protocol (запрос framebuffer, HHDM) |
| `volatile` | 0.6.1 | Зарезервировано (не используется активно) |
| Limine (загрузчик) | 12.5.2 | Передаёт управление в higher-half, предоставляет framebuffer и HHDM |
| NASM | — | Компиляция `boot.asm` (entry point, 64-бит) |
| xorriso | — | Создание загрузочного ISO |
| QEMU | — | Эмуляция (запуск вручную в системном терминале) |

## Структура проекта

```
my_kernel/
├── .cargo/
│   └── config.toml          — цель сборки x86_64-unknown-none, build-std, флаги линкера
├── .kodaruls                 — правила разработки проекта (7 пунктов)
├── Cargo.toml                — манифест пакета
├── build.rs                  — компиляция boot.asm через NASM (elf64)
├── linker.ld                 — линкер-скрипт (higher-half, секция .requests для Limine)
├── i686-unknown-none.json    — неиспользуемая цель (проект на x86_64; оставлена)
├── debug_kernel.sh           — скрипт сборки+запуска через GRUB (устарел, используется Limine)
├── make_iso.sh               — сборка ISO через xorriso + Limine
├── plan.txt                  — план разработки (задачи на текущую итерацию)
├── my_kernel.log             — лог всех изменений (только дописывается, с 2025-08-10)
├── iso/
│   ├── kernel.elf            — собранный бинарник ядра
│   ├── limine.conf           — конфигурация Limine (PROTOCOL, KERNEL_PATH, SERIAL, VERBOSE)
│   ├── limine-bios.sys       — бинарник Limine BIOS
│   └── limine-bios-cd.bin    — загрузочный образ Limine для CD
├── limine-binary/            — исходные бинарники Limine
└── src/
    ├── main.rs               — точка входа _start_rust, инициализация подсистем, запуск shell
    ├── boot.asm              — 64-битный entry point (установка стека, вызов _start_rust)
    ├── vga.rs                — VgaWriter + макросы print!/println! (вывод в VGA + serial + framebuffer)
    ├── serial.rs             — драйвер UART 16550 (COM1): init, write, read, SerialWriter
    ├── vesa.rs               — вывод на framebuffer: шрифт 8x16, vesa_draw_char/ex, vesa_print_str, VesaWriter
    ├── limine_fb.rs          — запросы Limine: FramebufferRequest, HhdmRequest
    ├── gdt.rs                — GDT + TSS + IST (отдельные стеки для #DF, таймера, клавиатуры)
    ├── interrupts.rs         — IDT (breakpoint, double_fault, timer, keyboard), PIC init, раскладка PS/2
    ├── scheduler.rs          — кооперативный планировщик (до 4 задач, run_scheduler / run_scheduler_once)
    ├── tasks.rs              — демо-задачи: task_hello, task_counter
    ├── shell.rs              — интерактивный shell: getchar (PS/2 + COM1), read_line, команды
    ├── terminal.rs           — полноценный терминал 80x25 (ANSI, курсор, рендер VGA/serial/framebuffer) — НЕ интегрирован в print!
    ├── interrupts.rs.bak     — бэкап прерываний (не компилируется)
    ├── main_old.rs.bak       — бэкап main.rs (не компилируется)
    └── limine_fb.rs          — (см. выше, дублирования нет)
```

## Архитектура

### Поток загрузки

1. **Limine** загружает `kernel.elf` из ISO, переводит CPU в long mode (64-бит), мапит физическую память через HHDM, передаёт управление на `_start` (из `boot.asm`).
2. **boot.asm**: устанавливает RSP на `stack_top` (16 КБ BSS-стек), очищает регистры, вызывает `_start_rust`.
3. **main.rs `_start_rust`**:
   - `serial::init()` — инициализация COM1 (до первого вывода)
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
- **IDT**: breakpoint (#3), double_fault (#DF, IST 1), timer (IRQ0, IST 2), keyboard (IRQ1, IST 3)
- **IST-стеки**: `DOUBLE_FAULT_STACK` (16 КБ), `TIMER_STACK` (8 КБ), `KEYBOARD_STACK` (8 КБ) — обработчики не зависят от глубины стека прерванного кода
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
# или вручную:
cargo build --release
cp target/x86_64-unknown-none/release/my_kernel iso/kernel.elf
# затем xorriso для создания boot.iso (см. make_iso.sh)
```

### Запуск ядра (строго вручную в системном терминале!)

```bash
# Вариант 1: вывод через serial в терминал (рекомендуемый)
qemu-system-x86_64 -cdrom boot.iso -boot d -m 512M -cpu max -no-reboot -display none -serial stdio

# Вариант 2: вывод serial в файл
qemu-system-x86_64 -cdrom boot.iso -boot d -m 512M -cpu max -no-reboot -display none -serial file:qemu.log

# Вариант 3: VNC (виден framebuffer)
qemu-system-x86_64 -cdrom boot.iso -boot d -m 512M -cpu max -no-reboot -display none -vnc :0
```

> **Важно**: QEMU в данной среде скомпилирован без бэкендов GTK/SDL. Доступны: `none`, `curses`, `dbus`, `vnc`. Бэкенд `curses` не отображает графический framebuffer — только текстовый VGA-режим.

### Тестирование

`cargo test` **не работает** — цель `x86_64-unknown-none` (bare-metal) не имеет `libtest`. Тестирование проводится вручную через запуск в QEMU и анализ serial-вывода. В проекте нет `#[test]`-функций.

## Правила разработки (.kodaruls)

1. **Одна операция в один момент времени** — никаких глобальных переписок, только точечные исправления.
2. **Никогда не переписывать файл целиком** — строго точечные исправления проблемного участка.
3. **Всегда проверять зависимый код и файлы** перед внесением изменений.
4. **Никогда не применять исправления без объяснения** происходящего в коде.
5. **Обязательное логирование** — каждое действие записывается в `my_kernel.log` (только дописывать, никаких перезаписей).
6. **Новый код выделять бирюзовым цветом** (до внесения в коммит) — для понимания внесённых изменений.
7. **Объяснять рациональность** любого предложения по применению.

### Дополнительные правила (из системного контекста)

- **Запуск ядра строго вручную** в системном терминале — не запускать QEMU из AI-ассистента.
- **Запрещено удалять любой код** (даже неиспользуемый) — требуется проверка на валидность и введение в основное тело программы. Удаление строго с разрешения человека.
- Бэкап-файлы (`*.bak`) и неиспользуемые цели (`i686-unknown-none.json`) оставлены в репозитории.

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

## План развития (из plan.txt)

1. **Интеграция терминала** — перенаправить `print!`/`println!` на `terminal::TERMINAL` вместо трёх независимых `write!`
2. **Тестирование терминала в QEMU** — ANSI-цвета, курсор, прокрутка, команды shell
3. **Команда `color` в shell** — демонстрация 16-цветной палитры через SGR
4. **Оптимизация рендера** — dirty-флаг вместо полной перерисовки 80x25 на каждый `write!`
