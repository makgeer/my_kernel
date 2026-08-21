# Malinka — Учебное ядро ОС на Rust

**Malinka** — учебное ядро операционной системы для архитектуры x86_64 (bare-metal, `no_std` / `no_main`). Загружается через загрузчик [Limine](https://limine-bootloader.org/) в higher-half режиме (базовый адрес `0xFFFFFFFF80000000`). Ядро реализует базовую инфраструктуру ОС: вывод на framebuffer/VGA/serial, обработку прерываний (PIC, IDT, GDT/TSS/IST), кооперативный планировщик задач, пользовательские процессы (ring 3), системные вызовы, ELF64-загрузчик, встроенный скриптовый движок Rhai и интерактивный shell.

---

## Возможности

### Загрузка и вывод
- **Загрузчик Limine** (BIOS) — переход в long mode, higher-half, HHDM
- **Вывод**: VGA text buffer (0xB8000) + Serial COM1 (UART 16550) + Framebuffer (Limine, шрифт 8x16)
- **Ввод**: PS/2 клавиатура (IRQ1, US-раскладка) + Serial COM1

### Управление памятью
- **PMM**: bitmap-аллокатор физических фреймов 4K (`alloc_frame` / `dealloc_frame`)
- **Paging**: 4-уровневые таблицы страниц x86_64 (`map_page` / `unmap_page` / `translate`, `create_user_p4`, `map_page_in_p4`)
- **Heap-аллокатор**: 8 МБ на основе `linked_list_allocator`
- **Изолированные адресные пространства**: каждый процесс получает собственную P4-таблицу

### Прерывания и защита
- **GDT + TSS + IST**: отдельные стеки для обработчиков #DF (16 КБ), таймера (8 КБ), клавиатуры (8 КБ)
- **IDT**: breakpoint (#3), double_fault (#DF), page_fault (#PF), timer (IRQ0), keyboard (IRQ1), syscall (int 0x80)
- **PIC**: master + slave, ремап, маскирование (разрешены только IRQ0 и IRQ1)
- **#PF handler**: диагностика (CR2, RIP, CS/SS, RFLAGS), восстановление kernel CR3 при панике

### Пользовательские процессы (Ring 3)
- **create_process** — создание процесса с маппингом кода и стека
- **create_elf_process** — загрузка ELF64 (PT_LOAD сегменты, флаги страниц из p_flags)
- **run_process** — двухэтапный переход: `iretq → user_mode_entry → CR3::write(user P4) → iretq → ring 3`
- **Безопасные syscalls**: `copy_from_user` / `copy_to_user` + `user_range_valid`

### Системные вызовы (int 0x80, DPL=3)
| № | Имя | Назначение |
|---|-----|-----------|
| 0 | `read` | Блокирующее чтение из stdin (PS/2 + COM1) |
| 1 | `write` | Вывод строки в консоль (через `copy_from_user`) |
| 60 | `exit` | Завершение процесса |

### Скриптовый движок
- **Rhai** (no_std) — выполнение скриптов прямо в ядре через команду `script "<код>"`
- **Brush** — собственная bash-совместимая оболочка MVP: `set`/`unset`/`env`/`echo`/`help`/`exit`, подстановка `$VAR` и `$?`

### Shell — интерактивная оболочка
| Команда | Описание |
|---------|----------|
| `help` | Список команд |
| `echo <текст>` | Вывод текста |
| `about` | Информация об ОС |
| `tasks` | Запуск кооперативного планировщика |
| `clear` | Очистка экрана |
| `cursor` | Управление курсором |
| `color` | Демонстрация 16-цветной палитры SGR |
| `reboot` | Перезагрузка |
| `hlt` | Останов CPU |
| `test_alloc` | Тест аллокатора кучи |
| `test_pmm` | Тест физической памяти |
| `test_paging` | Тест страничной адресации |
| `test_process` | Тест создания процесса |
| `run_user` | Запуск user-программы (inline asm) |
| `run_elf` | Загрузка и запуск ELF64-программы |
| `run_syscall` | Тест read/write syscalls (2 PT_LOAD сегмента) |
| `script "<код>"` | Выполнение Rhai-скрипта |
| `brush` | Вход в bash-совместимую оболочку |

---

## Технологии и зависимости

| Компонент | Версия | Назначение |
|-----------|--------|------------|
| Rust (nightly) | — | Язык реализации, `#![feature(abi_x86_interrupt)]` |
| `x86_64` | 0.15.2 | API для работы с портами, сегментами, IDT, GDT, TSS |
| `spin` | 0.9.8 | `Mutex` и `Once` без аллокации (no_std) |
| `heapless` | 0.7.16 | `Vec<u8, 256>` для буфера клавиатуры без аллокации |
| `limine` | 0.6.5 | Limine Boot Protocol (запрос framebuffer, HHDM) |
| `linked_list_allocator` | 0.10.x | Heap-аллокатор (8 МБ, `LockedHeap`) |
| `rhai` | 1.25.1 | Скриптовый движок (no_std, `features = ["no_std"]`) |
| Limine (загрузчик) | 12.5.2 | Передаёт управление в higher-half, framebuffer, HHDM |
| NASM | — | Компиляция `boot.asm` и user-программ |
| GNU ld | — | Линковка user-программ |
| xorriso | — | Создание загрузочного ISO |
| Bochs | — | Эмулятор для тестирования |

---

## Структура проекта

```
malinka/
├── .cargo/
│   └── config.toml          — цель сборки x86_64-unknown-none, build-std, флаги линкера
├── Cargo.toml               — манифест пакета
├── build.rs                 — компиляция boot.asm через NASM (elf64)
├── linker.ld                — линкер-скрипт (higher-half, секция .requests для Limine)
├── make_iso.sh              — сборка ISO через xorriso + Limine (аргумент: no-fb)
├── build_user.sh            — сборка user-программы hello (nasm + ld) → hello.elf
├── build_syscall_test.sh    — сборка user-программы syscall_test (2 PT_LOAD сегмента)
├── bochsrc.bxrc             — конфигурация Bochs
├── iso/                     — файлы для ISO (kernel.elf, limine.conf, limine-bios.*)
├── limine-binary/           — исходные бинарники Limine
├── user_programs/           — пользовательские программы (ring 3)
│   ├── hello.asm            — тестовая программа: write() + exit()
│   ├── hello.ld             — линкер-скрипт (база 0x400000000000)
│   ├── hello.elf            — собранный статический ET_EXEC
│   ├── syscall_test.asm     — тест read/write syscalls (2 сегмента)
│   ├── syscall_test.ld      — линкер-скрипт для syscall_test
│   └── syscall_test.elf     — собранный ELF
├── arhiv/                   — архив неиспользуемых файлов (.bak, устаревшие скрипты)
├── plan.txt                 — план разработки
├── my_kernel.log            — лог всех изменений (с 2025-08-10)
├── KODA.md                  — технический контекст проекта
├── Documents.md             — подробная документация ОС
└── src/
    ├── main.rs              — точка входа _start_rust, инициализация подсистем, запуск shell
    ├── boot.asm             — 64-битный entry point (установка стека, вызов _start_rust)
    ├── vga.rs               — VgaWriter + макросы print!/println! (вывод в VGA + serial + framebuffer)
    ├── serial.rs            — драйвер UART 16550 (COM1): init, write, read, SerialWriter
    ├── vesa.rs              — вывод на framebuffer: шрифт 8x16, vesa_draw_char/ex, vesa_print_str
    ├── limine_fb.rs         — запросы Limine: FramebufferRequest, HhdmRequest
    ├── gdt.rs               — GDT + TSS + IST, kernel_panic
    ├── interrupts.rs        — IDT, PIC, syscall handler, copy_from/to_user, page_fault
    ├── scheduler.rs         — кооперативный планировщик (до 4 задач)
    ├── tasks.rs             — демо-задачи: task_hello, task_counter
    ├── shell.rs             — интерактивный shell: getchar, команды (run_elf, script, brush)
    ├── terminal.rs          — терминал 80x25 (ANSI, курсор, рендер)
    ├── allocator.rs         — heap-аллокатор 8 МБ (linked_list_allocator, LockedHeap)
    ├── pmm.rs               — PMM: bitmap-аллокатор физических фреймов 4K
    ├── paging.rs            — пейджинг: map_page/unmap_page/translate, create_user_p4
    ├── process.rs           — процессы: create_process, create_elf_process, run_process
    ├── elf.rs               — ELF64-загрузчик: парсер Ehdr/Phdr + загрузка PT_LOAD
    ├── script.rs            — встроенный скриптовый движок Rhai (no_std)
    └── brush.rs             — bash-совместимая оболочка MVP
```

---

## Сборка и запуск

### Требования

- **Rust nightly** (`rustup default nightly`)
- **Цель компиляции**: `x86_64-unknown-none` (`rustup target add x86_64-unknown-none`)
- **NASM** (ассемблер)
- **xorriso** (создание ISO)
- **Bochs** (эмулятор, собранный с поддержкой x86-64)

### Команды

```bash
# Клонировать репозиторий
git clone https://github.com/makgeer/my_kernel.git
cd my_kernel

# Проверка компиляции (без линковки)
cargo check

# Сборка (debug)
cargo build

# Сборка (release, с LTO)
cargo build --release

# Создание загрузочного ISO (с framebuffer)
./make_iso.sh

# Режим без framebuffer (только VGA 80x25 + serial, для Bochs term)
./make_iso.sh no-fb

# Сборка пользовательских ELF-программ
./build_user.sh              # hello.elf
./build_syscall_test.sh      # syscall_test.elf
```

### Запуск в Bochs

```bash
# Запуск строго вручную в системном терминале
./bochs-bin/bin/bochs -q -f bochsrc.bxrc
```

### Тестирование

`cargo test` не работает — цель `x86_64-unknown-none` (bare-metal) не имеет `libtest`. Тестирование проводится вручную через запуск в Bochs и анализ serial-вывода.

Встроенные тесты в shell: `test_alloc`, `test_pmm`, `test_paging`, `test_process`, `run_user`, `run_elf`, `run_syscall`, `script "print(40+2)"`, `brush`.

---

## Архитектура

### Поток загрузки

1. **Limine** загружает `kernel.elf` из ISO, переводит CPU в long mode (64-бит), мапит физическую память через HHDM, передаёт управление на `_start` (из `boot.asm`).
2. **boot.asm**: устанавливает RSP на `stack_top` (16 КБ BSS-стек), очищает регистры, вызывает `_start_rust`.
3. **main.rs `_start_rust`**:
   - `serial::init()` — инициализация COM1
   - `allocator::init_heap()` — heap 8 МБ
   - `pmm::init()` + `paging::init()` — PMM + пейджинг (HHDM, kernel P4)
   - Запрос framebuffer у Limine → `vesa_set_framebuffer` + `vesa_clear_screen`
   - `gdt::init_gdt()` — загрузка GDT + TSS (IST-стеки), установка CS/DS/SS
   - `interrupts::init_idt()` — загрузка IDT, инициализация PIC, `sti`
   - `add_task()` — регистрация демо-задач
   - `shell::shell()` — интерактивная оболочка

### Вывод

Макрос `print!` (в `vga.rs`) дублирует вывод в три устройства одновременно:
- **VgaWriter** → VGA text buffer `0xB8000`
- **SERIAL** → COM1 (serial)
- **VesaWriter** → Limine framebuffer (шрифт 8x16)

### Прерывания

- **PIC**: master (0x20-0x27) + slave (0x28-0x2F), маски — разрешены только IRQ0 (таймер) и IRQ1 (клавиатура)
- **IDT**: breakpoint (#3), double_fault (#DF, IST 1), page_fault (#PF), timer (IRQ0, IST 2), keyboard (IRQ1, IST 3), syscall (int 0x80, DPL=3)
- **IST-стеки**: `DOUBLE_FAULT_STACK` (16 КБ), `TIMER_STACK` (8 КБ), `KEYBOARD_STACK` (8 КБ)
- **#PF handler**: диагностика (CR2, RIP, CS/SS, RFLAGS); при PF в user mode — kernel_panic
- **syscall int 0x80**: asm-обёртка `syscall_entry` (naked) сохраняет все GPR → `syscall_handler_rust` (read/write/exit)
- **Клавиатура**: US-раскладка (нижний регистр), буфер `heapless::Vec<u8, 256>`

### Планировщик

Кооперативный, до 4 задач (`Mutex<[Option<Task>; 4]>`). `run_scheduler_once()` выполняет один проход всех задач — вызывается из shell командой `tasks`.

### Пользовательские процессы (Ring 3)

- **create_user_p4** копирует ТОЛЬКО higher-half (P4[256-511]) — копирование всего kernel P4 ломало ring 3
- **VGA buffer (0xB8000)** мапится в user P4 отдельно — обработчики прерываний при активной user CR3 вызывают `println!`
- **Двухэтапный переход**: `run_process → iretq → user_mode_entry → CR3::write(user P4) → iretq → ring 3`
- **ELF-загрузчик**: PT_LOAD сегменты мапятся по адресам из ELF header, флаги страниц из p_flags (PF_W→WRITABLE, !PF_X→NO_EXECUTE)
- **Безопасные syscalls**: `copy_from_user` / `copy_to_user` с проверкой валидности пользовательских адресов

### Ключевые архитектурные решения

- **Limine вместо GRUB/Multiboot2** — нативная поддержка framebuffer и HHDM в higher-half
- **Higher-half (0xFFFFFFFF80000000)** — Limine требует PHDRs в верхней половине адресного пространства
- **IST-стеки** — устраняют DOUBLE FAULT при переполнении стека ядра во время прерывания
- **SS::set_reg(data_sel)** — Limine оставляет свой SS (0x30), при прерывании CPU кладёт его в IRET-фрейм, а при `iretq` не находит в нашей GDT → #GP → #DF
- **PIC маскирование**: 0xFC (master) + 0xFF (slave) — без маски IRQ8 (RTC) периодически срабатывает без обработчика → DOUBLE FAULT

---

## План развития

1. **Планировщик процессов** — вытесняющий round-robin, переключение контекста между ring 3 процессами
2. **IPC** — каналы/почтовые ящики для обмена сообщениями между процессами
3. **Файловая система** — VFS + FAT32/ext2
4. **Защита памяти** — SMAP/SMEP, W^X, guard pages, KASLR
5. **Загружаемые модули** — драйверы как отдельные ELF-модули

---

## Лицензия

Apache 2.0 — свободное использование, модификация и распространение.

## Контакты

Автор: makgeer  
GitHub: https://github.com/makgeer

---

## English

**Malinka** is an educational x86_64 kernel written in Rust (bare-metal, `no_std` / `no_main`). It boots via the Limine bootloader in higher-half mode. The kernel implements basic OS infrastructure: framebuffer/VGA/serial output, interrupt handling (PIC, IDT, GDT/TSS/IST), a cooperative task scheduler, user-space processes (ring 3), system calls, an ELF64 loader, an embedded Rhai scripting engine, and an interactive shell.

### Quick start

```bash
git clone https://github.com/makgeer/my_kernel.git
cd my_kernel
rustup target add x86_64-unknown-none
./make_iso.sh no-fb
./bochs-bin/bin/bochs -q -f bochsrc.bxrc
```

### License

Apache 2.0 — free to use, modify, and distribute.
