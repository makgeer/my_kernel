# Malinka — Учебное ядро ОС на Rust

**Malinka** — учебное ядро операционной системы для архитектуры x86_64 (bare-metal, `no_std` / `no_main`). Загружается через загрузчик [Limine](https://limine-bootloader.org/) в higher-half режиме (базовый адрес `0xFFFFFFFF80000000`). Ядро реализует базовую инфраструктуру ОС: вывод на framebuffer/VGA/serial, обработку прерываний (PIC, IDT, GDT/TSS/IST), вытесняющий round-robin планировщик процессов, пользовательские процессы (ring 3), системные вызовы, IPC, ELF64-загрузчик, встроенный скриптовый движок Rhai и интерактивный shell. При разработке данного проекта, огромный вклад был сделан проектом Koda.AI - огромное спасибо разработчикам, за их работу! Без их труда не состоялся бы этот проект.

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
- **GDT + TSS + IST**: отдельные стеки для обработчиков #DF (16 КБ), таймера, клавиатуры, #PF и serial (по 8 КБ)
- **IDT**: breakpoint (#3), double_fault (#DF), page_fault (#PF), timer (IRQ0), keyboard (IRQ1), serial (IRQ4), syscall (int 0x80)
- **PIC**: master + slave, ремап, маскирование (разрешены IRQ0, IRQ1 и IRQ4)
- **#PF handler**: диагностика (CR2, RIP, CS/SS, RFLAGS), обнаружение guard page, восстановление kernel CR3 при панике

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
| 24 | `yield` | Добровольная передача кванта планировщику |
| 39 | `getpid` | PID текущего процесса |
| 40 | `ipc_send` | Отправка сообщения процессу по PID (блокирующая при полном ящике) |
| 41 | `ipc_recv` | Приём сообщения (блокирующий) |
| 42 | `irq_wait` | Ожидание аппаратного прерывания (user-space драйверы) |
| 43 | `io_in` | Чтение порта ввода-вывода (белый список + capability) |
| 44 | `irq_claim` | Захват IRQ драйвером (только свободного) |
| 60 | `exit` | Завершение процесса |

### Планировщик и IPC
- **Вытесняющий round-robin**: до 16 процессов, квант 5 тиков PIT (100 Гц)
- **Переключение контекста**: `timer_schedule_rust` + IRET-фрейм, отдельный CR3 на процесс
- **Состояния**: Ready / Running / Blocked / Terminated; причина блокировки — `WaitKind`
  (Irq, IpcRecv, IpcSend), поэтому процесс будится только своим событием
- **Блокирующий IPC**: почтовые ящики на 4 сообщения; при переполнении отправитель
  засыпает и просыпается после `ipc_recv` получателя
- **Завершение**: `exit`/`kill` освобождают IRQ, будят ожидающих отправителей
  и переключают на следующий Ready-процесс
- **Idle-контекст**: shell, вытесненный таймером, продолжает работу с точки
  прерывания (без рекурсивного вызова `shell()`)

### Скриптовый движок
- **Rhai** (no_std) — выполнение скриптов прямо в ядре через команду `script "<код>"`
- **Brush** — собственная bash-совместимая оболочка MVP: `set`/`unset`/`env`/`echo`/`help`/`exit`, подстановка `$VAR` и `$?`

### Shell — интерактивная оболочка

Редактирование строки: Backspace (в том числе в середине строки),
стрелки влево/вправо, история последних 25 команд по стрелкам
вверх/вниз. Стрелки работают одинаково для PS/2 и COM1: скан-коды
`0xE0 0x48/0x50/0x4B/0x4D` и serial-последовательности `ESC [ A/B/C/D`
приводятся к одному виду.
| Команда | Описание |
|---------|----------|
| `help` | Список команд |
| `echo <текст>` | Вывод текста |
| `about` | Информация об ОС |
| `tasks` | Запуск кооперативного планировщика |
| `clear` | Очистка экрана |
| `cursor` | Управление курсором |
| `color` | Демонстрация 16-цветной палитры SGR |
| `reboot` / `rb` / `rebut` | Перезагрузка |
| `hlt` | Останов CPU |
| `exit` | Останов системы и выход из shell |
| `test_alloc` | Тест аллокатора кучи |
| `test_pmm` | Тест физической памяти |
| `test_paging` | Тест страничной адресации |
| `test_process` | Тест создания процесса |
| `run_user` | Запуск user-программы (inline asm) |
| `run_elf` | Загрузка и запуск ELF64-программы |
| `run_syscall` | Тест read/write syscalls (2 PT_LOAD сегмента) |
| `run <file>` | Загрузка и запуск ELF из VFS (`/bin/<file>`) |
| `run_multi` | Запуск нескольких процессов (тест вытеснения) |
| `test_ipc` | Тест IPC (отправитель + получатель) |
| `keyboardd` | User-space драйвер PS/2 (IRQ1 → IPC PID 0) |
| `seriald` | User-space драйвер COM1 (IRQ4 → IPC PID 0) |
| `arrow_test` | Тест клавиатуры из Ring 3 (hex-коды клавиш) |
| `ps` | Список процессов |
| `kill <pid>` | Завершить процесс по PID |
| `irqstat` | Владельцы IRQ (ядро или user-space драйвер) |
| `ipcstat` | Статистика IPC (sent/received/failed) |
| `ls`, `cat` | Просмотр initrd/VFS |
| `btrfs`, `btrfs-ls`, `btrfs-cat` | Работа с btrfs-диском |
| `script "<код>"` | Выполнение Rhai-скрипта |
| `brush` | Вход в bash-совместимую оболочку |

---

## Технологии и зависимости

| Компонент | Версия | Назначение |
|-----------|--------|------------|
| Rust (nightly) | — | Язык реализации, `#![feature(abi_x86_interrupt)]`; версия фиксируется `rust-toolchain.toml` |
| `x86_64` | 0.15.2 | API для работы с портами, сегментами, IDT, GDT, TSS |
| `spin` | 0.9.8 | `Mutex` и `Once` без аллокации (no_std) |
| `heapless` | 0.7.16 | `Vec<u8, 256>` для буфера клавиатуры без аллокации |
| `limine` | 0.6.5 | Limine Boot Protocol (запрос framebuffer, HHDM) |
| `linked_list_allocator` | 0.10.x | Heap-аллокатор (8 МБ, `LockedHeap`) |
| `rhai` | 1.25.1 | Скриптовый движок (no_std, `features = ["no_std"]`) |
| `btrfs-no-std` | 0.2 | Структуры дискового формата btrfs (no_std) |
| Limine (загрузчик) | 12.5.2 | Передаёт управление в higher-half, framebuffer, HHDM |
| NASM | — | Компиляция `boot.asm` и user-программ |
| GNU ld | — | Линковка user-программ |
| xorriso / genisoimage | — | Создание загрузочного ISO |
| Bochs | — | Эмулятор для тестирования |

---

## Структура проекта

```
malinka/
├── .cargo/
│   └── config.toml          — цель сборки x86_64-unknown-none, build-std, флаги линкера
├── Cargo.toml               — манифест пакета
├── build.rs                 — NASM: boot.asm + user_programs/*.asm → *.elf
├── linker.ld                — линкер-скрипт (higher-half, секция .requests для Limine)
├── make_iso.sh              — сборка ISO через xorriso/genisoimage + Limine (аргумент: no-fb)
├── bochsrc.bxrc             — конфигурация Bochs
├── iso/                     — файлы для ISO (kernel.elf, limine.conf, limine-bios.*)
├── limine-binary/           — исходные бинарники Limine
├── user_programs/           — пользовательские программы (ring 3)
│   ├── init.asm             — первая user-программа (write + exit), запускается ядром
│   ├── hello.asm            — тестовая программа: write() + exit()
│   ├── syscall_test.asm     — тест read/write syscalls (2 сегмента)
│   ├── loop_a/b.asm         — тест вытесняющего планировщика
│   ├── ipc_sender/receiver.asm — тест IPC
│   ├── arrow_test.asm       — тест клавиатуры (hex-коды клавиш)
│   ├── sysinfo.asm          — тест getpid (#39) и yield (#24)
│   ├── keyboardd.asm        — user-space драйвер PS/2 (IRQ1 → IPC PID 0)
│   ├── seriald.asm          — user-space драйвер COM1 (IRQ4 → IPC PID 0)
│   ├── hello.ld             — линкер-скрипт (база 0x400000000000)
│   └── syscall_test.ld      — линкер-скрипт с двумя PT_LOAD
├── arhiv/                   — архив неиспользуемых файлов (.bak, устаревшие скрипты)
├── plan.txt                 — план разработки
├── my_kernel.log            — лог всех изменений (с 2025-08-10)
├── KODA.md                  — технический контекст проекта
├── Documents.md             — подробная документация ОС
└── src/
    ├── main.rs              — точка входа _start_rust, инициализация подсистем, запуск shell
    ├── boot.asm             — 64-битный entry point (установка стека, вызов _start_rust)
    ├── video.rs             — VGA + framebuffer + макросы print!/println!
    ├── serial.rs            — драйвер UART 16550 (COM1): init, write, read, IRQ4
    ├── limine_fb.rs         — запросы Limine: FramebufferRequest, HhdmRequest
    ├── gdt.rs               — GDT + TSS + IST, kernel_panic
    ├── interrupts.rs        — IDT, PIC, syscall handler, copy_from/to_user, page_fault
    ├── scheduler.rs         — демо-задачи для команды tasks (кооперативный проход)
    ├── process_manager.rs   — вытесняющий round-robin, IPC-почтовые ящики, ps/kill
    ├── tasks.rs             — демо-задачи: task_hello, task_counter
    ├── shell.rs             — интерактивный shell: read_key, команды (run, ps, script, brush)
    ├── terminal.rs          — терминал 80x25 (ANSI, курсор, рендер)
    ├── allocator.rs         — heap-аллокатор 8 МБ (linked_list_allocator, LockedHeap)
    ├── pmm.rs               — PMM: bitmap-аллокатор физических фреймов 4K
    ├── paging.rs            — пейджинг: map_page/unmap_page/translate, create_user_p4
    ├── process.rs           — процессы: create_process, create_elf_process, run_process
    ├── elf.rs               — ELF64-загрузчик: парсер Ehdr/Phdr + загрузка PT_LOAD
    ├── fs.rs                — VFS: FsNode, монтирование, read_file
    ├── fs_initrd.rs         — initrd: встроенные ELF в /bin
    ├── ata.rs               — ATA PIO драйвер (primary/secondary)
    ├── btrfs.rs             — чтение btrfs: суперблок, chunk map, каталоги, файлы
    ├── tests.rs             — тесты планировщика и IPC
    ├── debug.rs             — безопасный вывод для #PF/#DF
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
```

Пользовательские ELF-программы (`user_programs/*.asm`) собираются
автоматически в `build.rs` перед компиляцией ядра (NASM + GNU ld).
Отдельные скрипты запускать не нужно: `cargo build` и `make_iso.sh`
всегда получают актуальные `.elf`.

Если nightly не выбран toolchain по умолчанию, make_iso.sh принимает
переопределение: `CARGO="cargo +nightly" ./make_iso.sh no-fb`.

### Запуск в Bochs

Два варианта конфигурации:

```bash
# 1. Headless: только serial, ввод/вывод через nc
./make_iso.sh no-fb
./bochs-bin/bin/bochs -q -f bochsrc.bxrc
nc localhost 8888

# 2. GUI (X11): окно с VGA-выводом и ввод с клавиатуры (PS/2)
./make_iso.sh                    # ISO должен быть собран С framebuffer
~/.local/bochs-gui/bin/bochs -q -f bochsrc-gui.bxrc
# COM1 пишется в bochs_gui_serial.log
```

**Важно:** режим ISO должен соответствовать конфигурации Bochs.
`bochsrc-gui.bxrc` включает VBE 1024x768x32 — в этом режиме текстовый
VGA-буфер `0xB8000` не виден, поэтому ISO обязан быть собран с
framebuffer (`./make_iso.sh`). И наоборот, для `bochsrc.bxrc` (nogui +
term) нужен `./make_iso.sh no-fb`. При несовпадении ядро работает и
пишет в serial, но экран остаётся пустым.

`./bochs-bin` собран только с `nogui`/`term` — X11-плагина в нём нет.
GUI-сборка ставится из исходников в `~/.local/bochs-gui` (без root):

```bash
curl -sL https://github.com/bochs-emu/Bochs/archive/refs/heads/master.tar.gz | tar xz
cd Bochs-master/bochs
./configure --prefix="$HOME/.local/bochs-gui" --with-x11 \
            --enable-x86-64 --enable-cpu-level=6 --enable-long-phy-address
make -j"$(nproc)" && make install
```

GUI нужен для проверки PS/2-драйверов (`keyboardd`) и стрелок: в
headless-режиме клавиатурного устройства нет, доступен только COM1.

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

- **PIC**: master (0x20-0x27) + slave (0x28-0x2F), маски — разрешены IRQ0 (таймер), IRQ1 (клавиатура) и IRQ4 (COM1)
- **IDT**: breakpoint (#3), double_fault (#DF, IST 1), page_fault (#PF), timer (IRQ0, IST 2), keyboard (IRQ1, IST 3), syscall (int 0x80, DPL=3)
- **IST-стеки**: `DOUBLE_FAULT_STACK` (16 КБ), `TIMER_STACK` (8 КБ), `KEYBOARD_STACK` (8 КБ)
- **#PF handler**: диагностика (CR2, RIP, CS/SS, RFLAGS); при PF в user mode — kernel_panic
- **syscall int 0x80**: asm-обёртка `syscall_entry` (naked) сохраняет все GPR → `syscall_handler_rust` (read/write/exit)
- **Клавиатура**: US-раскладка (нижний регистр), буфер `heapless::Vec<u8, 256>`

### Планировщик

Вытесняющий round-robin в `process_manager.rs`: до 16 процессов, квант
5 тиков PIT (100 Гц). `timer_schedule_rust` вызывается из IRQ0, сохраняет
IRET-фрейм текущего процесса и переключает CR3 + контекст на следующий
Ready-процесс. Внутри ring 0 вытеснение запрещено — syscall завершается
на kernel-стеке текущего процесса. Причина блокировки хранится в
`WaitKind` (Irq, IpcRecv, IpcSend), поэтому процесс будится только своим
событием. Команда `tasks` запускает отдельный кооперативный проход
демо-задач из `scheduler.rs` (не связан с процессами ring 3).

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

**Malinka** is an educational x86_64 kernel written in Rust (bare-metal, `no_std` / `no_main`). It boots via the Limine bootloader in higher-half mode. The kernel implements basic OS infrastructure: framebuffer/VGA/serial output, interrupt handling (PIC, IDT, GDT/TSS/IST), a preemptive round-robin process scheduler, user-space processes (ring 3), system calls, an ELF64 loader, an embedded Rhai scripting engine, and an interactive shell.

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
