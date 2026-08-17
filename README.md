Malinka — Микроядро на Rust
Malinka — это легковесное, безопасное и модульное микроядро, написанное на Rust. Проект создаётся с нуля как исследовательская ОС для x86_64 с акцентом на минимализм, производительность и изоляцию компонентов.

🚀 Возможности
✅ Микроядро — сервисы работают в userspace

✅ Безопасность — написано на Rust (no_std, безопасные абстракции)

✅ 64-битная архитектура — полная поддержка x86_64

✅ Загрузчик Limine — современный загрузочный протокол

✅ Framebuffer + VGA — вывод на экран и последовательный порт

✅ Прерывания — IDT, GDT, TSS, IST, PIC (IRQ0, IRQ1)

✅ Планировщик — кооперативный, с задачами

✅ Shell — интерактивная оболочка с командами

✅ Куча — аллокатор на основе linked_list_allocator

✅ PMM + Paging — физический менеджер памяти и страничная адресация

✅ Процессы — изолированные адресные пространства, переход в Ring 3

✅ Syscalls — поддержка int 0x80 (write/exit)

🛠 Установка и сборка
bash
# Клонировать репозиторий
git clone https://github.com/makgeer/my_kernel.git
cd my_kernel

# Установить цель для компиляции
rustup target add x86_64-unknown-none

# Собрать ядро (режим framebuffer)
cargo build --release --features fb

# Собрать ядро (только VGA)
cargo build --release --no-default-features

# Собрать ISO-образ
./make_iso.sh
📦 Зависимости
Rust nightly (последняя стабильная версия)

xorriso

mtools

Bochs (рекомендуется) или QEMU

🏃 Запуск
bash
# Bochs (рекомендуется)
./bochs-bin/bin/bochs -q -f bochsrc.bxrc

# QEMU (устаревший, поддержка ограничена)
qemu-system-x86_64 -cdrom boot.iso -boot d -m 512M -cpu max -no-reboot -display curses
🧪 Тестирование
bash
# В оболочке Malinka
> help                      # список команд
> about                     # информация об ОС
> echo <текст>              # вывод текста
> tasks                     # запуск планировщика
> test_alloc                # тест кучи
> test_pmm                  # тест физической памяти
> test_paging               # тест страничной адресации
> test_process              # тест создания процесса
> run_user                  # тест syscall
> clear                     # очистка экрана
📁 Структура проекта
text
my_kernel/
├── src/
│   ├── main.rs             # Точка входа
│   ├── boot.asm            # Загрузчик (64-бит)
│   ├── gdt.rs              # Глобальная таблица дескрипторов
│   ├── interrupts.rs       # IDT, PIC, обработчики
│   ├── serial.rs           # COM1-драйвер
│   ├── vga.rs              # VGA-буфер
│   ├── vesa.rs             # Framebuffer-вывод
│   ├── limine_fb.rs        # Запрос фреймбуфера у Limine
│   ├── terminal.rs         # Модуль терминала (ANSI/VGA/FB)
│   ├── shell.rs            # Интерактивная оболочка
│   ├── allocator.rs        # Аллокатор кучи
│   ├── pmm.rs              # Физический менеджер памяти
│   ├── paging.rs           # Страничная адресация
│   ├── process.rs          # Процессы и Ring 3
│   ├── scheduler.rs        # Кооперативный планировщик
│   └── tasks.rs            # Тестовые задачи
├── iso/                    # Файлы для ISO
├── bochs-bin/              # Bochs (скомпилированная версия)
├── linker.ld               # Линкер-скрипт
├── make_iso.sh             # Скрипт сборки ISO
├── bochsrc.bxrc            # Конфигурация Bochs
├── KODA.md                 # Контекст для AI-ассистента
├── plan.txt                # План развития
└── README.md               # Этот файл
📄 Лицензия
Apache 2.0 — свободное использование, модификация и распространение.

🤝 Контакты
Автор: makgeer

GitHub: https://github.com/makgeer

🇬🇧 English
Malinka — Rust Microkernel
Malinka is a lightweight, secure, and modular microkernel written in Rust. Built from scratch as a research OS for x86_64 with a focus on minimalism, performance, and component isolation.

🚀 Features
✅ Microkernel architecture — services run in userspace

✅ Memory safety — written in Rust (no_std, safe abstractions)

✅ 64-bit — full x86_64 support

✅ Limine bootloader — modern boot protocol

✅ Framebuffer + VGA — graphical and serial output

✅ Interrupts — IDT, GDT, TSS, IST, PIC (IRQ0, IRQ1)

✅ Cooperative scheduler — with tasks

✅ Shell — interactive command line

✅ Heap allocator — linked_list_allocator-based

✅ PMM + Paging — physical memory manager and page tables

✅ Processes — isolated address spaces, Ring 3 transitions

✅ Syscalls — int 0x80 (write/exit)

🛠 Build & Run
bash
git clone https://github.com/makgeer/my_kernel.git
cd my_kernel
rustup target add x86_64-unknown-none
cargo build --release --features fb
./make_iso.sh
./bochs-bin/bin/bochs -q -f bochsrc.bxrc
📄 License
Apache 2.0 — free to use, modify, and distribute.

🤝 Contact
Author: makgeer

GitHub: https://github.com/makgeer

🇫🇷 Français
Malinka — Micro-noyau en Rust
Malinka est un micro-noyau léger, sécurisé et modulaire écrit en Rust. Conçu de zéro comme un système d'exploitation de recherche pour x86_64, avec un accent sur le minimalisme, la performance et l'isolation des composants.

🚀 Fonctionnalités
✅ Micro-noyau — services en espace utilisateur

✅ Sécurité — écrit en Rust (no_std, abstractions sûres)

✅ 64-bit — support complet x86_64

✅ Chargeur Limine — protocole de démarrage moderne

✅ Framebuffer + VGA — sortie écran et port série

✅ Interruptions — IDT, GDT, TSS, IST, PIC (IRQ0, IRQ1)

✅ Ordonnanceur — coopératif avec tâches

✅ Shell — ligne de commande interactive

✅ Allocateur heap — basé sur linked_list_allocator

✅ PMM + Paging — gestionnaire de mémoire physique et pagination

✅ Processus — espaces d'adressage isolés, passage en Ring 3

✅ Syscalls — int 0x80 (write/exit)

🛠 Construction et exécution
bash
git clone https://github.com/makgeer/my_kernel.git
cd my_kernel
rustup target add x86_64-unknown-none
cargo build --release --features fb
./make_iso.sh
./bochs-bin/bin/bochs -q -f bochsrc.bxrc
📄 Licence
Apache 2.0 — utilisation, modification et distribution libres.

🤝 Contact
Auteur: makgeer

GitHub: https://github.com/makgeer

🇪🇸 Español
Malinka — Micronúcleo en Rust
Malinka es un micronúcleo ligero, seguro y modular escrito en Rust. Construido desde cero como un SO de investigación para x86_64, con enfoque en minimalismo, rendimiento y aislamiento de componentes.

🚀 Características
✅ Micronúcleo — servicios en espacio de usuario

✅ Seguridad — escrito en Rust (no_std, abstracciones seguras)

✅ 64 bits — soporte completo x86_64

✅ Cargador Limine — protocolo de arranque moderno

✅ Framebuffer + VGA — salida gráfica y por puerto serie

✅ Interrupciones — IDT, GDT, TSS, IST, PIC (IRQ0, IRQ1)

✅ Planificador — cooperativo con tareas

✅ Shell — línea de comandos interactiva

✅ Asignador de heap — basado en linked_list_allocator

✅ PMM + Paging — gestor de memoria física y paginación

✅ Procesos — espacios de direcciones aislados, paso a Ring 3

✅ Syscalls — int 0x80 (write/exit)

🛠 Compilación y ejecución
bash
git clone https://github.com/makgeer/my_kernel.git
cd my_kernel
rustup target add x86_64-unknown-none
cargo build --release --features fb
./make_iso.sh
./bochs-bin/bin/bochs -q -f bochsrc.bxrc
📄 Licencia
Apache 2.0 — uso, modificación y distribución libres.

🤝 Contacto
Autor: makgeer

GitHub: https://github.com/makgeer

🇩🇪 Deutsch
Malinka — Rust-Mikrokernel
Malinka ist ein leichtgewichtiger, sicherer und modularer Mikrokernel in Rust. Von Grund auf als Forschungs-OS für x86_64 entwickelt, mit Fokus auf Minimalismus, Leistung und Komponentenisolation.

🚀 Funktionen
✅ Mikrokernel — Dienste im Userspace

✅ Sicherheit — in Rust geschrieben (no_std, sichere Abstraktionen)

✅ 64-Bit — vollständige x86_64-Unterstützung

✅ Limine-Bootloader — modernes Bootprotokoll

✅ Framebuffer + VGA — Bildschirm- und serielle Ausgabe

✅ Interrupts — IDT, GDT, TSS, IST, PIC (IRQ0, IRQ1)

✅ Scheduler — kooperativ mit Aufgaben

✅ Shell — interaktive Befehlszeile

✅ Heap-Allokator — basierend auf linked_list_allocator

✅ PMM + Paging — physischer Speichermanager und Seitentabellen

✅ Prozesse — isolierte Adressräume, Ring-3-Wechsel

✅ Syscalls — int 0x80 (write/exit)

🛠 Bauen und Ausführen
bash
git clone https://github.com/makgeer/my_kernel.git
cd my_kernel
rustup target add x86_64-unknown-none
cargo build --release --features fb
./make_iso.sh
./bochs-bin/bin/bochs -q -f bochsrc.bxrc
📄 Lizenz
Apache 2.0 — freie Nutzung, Änderung und Verbreitung.

🤝 Kontakt
Autor: makgeer

GitHub: https://github.com/makgeer

🇵🇹 Português
Malinka — Micronúcleo em Rust
Malinka é um micronúcleo leve, seguro e modular escrito em Rust. Construído do zero como um SO de pesquisa para x86_64, com foco em minimalismo, desempenho e isolamento de componentes.

🚀 Funcionalidades
✅ Micronúcleo — serviços em espaço de usuário

✅ Segurança — escrito em Rust (no_std, abstrações seguras)

✅ 64 bits — suporte completo x86_64

✅ Carregador Limine — protocolo de inicialização moderno

✅ Framebuffer + VGA — saída gráfica e serial

✅ Interrupções — IDT, GDT, TSS, IST, PIC (IRQ0, IRQ1)

✅ Escalonador — cooperativo com tarefas

✅ Shell — linha de comando interativa

✅ Alocador de heap — baseado em linked_list_allocator

✅ PMM + Paging — gerenciador de memória física e paginação

✅ Processos — espaços de endereço isolados, transição para Ring 3

✅ Syscalls — int 0x80 (write/exit)

🛠 Construção e execução
bash
git clone https://github.com/makgeer/my_kernel.git
cd my_kernel
rustup target add x86_64-unknown-none
cargo build --release --features fb
./make_iso.sh
./bochs-bin/bin/bochs -q -f bochsrc.bxrc
📄 Licença
Apache 2.0 — uso, modificação e distribuição livres.

🤝 Contato
Autor: makgeer

GitHub: https://github.com/makgeer

🇨🇳 中文
Malinka — Rust 微内核
Malinka 是一个轻量、安全、模块化的 Rust 微内核。从零开始构建，作为 x86_64 的研究型操作系统，注重极简主义、性能和组件隔离。

🚀 特性
✅ 微内核架构 — 服务运行在用户空间

✅ 内存安全 — 使用 Rust 编写（no_std，安全抽象）

✅ 64 位 — 完整支持 x86_64

✅ Limine 引导程序 — 现代引导协议

✅ Framebuffer + VGA — 图形和串口输出

✅ 中断处理 — IDT、GDT、TSS、IST、PIC（IRQ0、IRQ1）

✅ 协作式调度器 — 支持任务

✅ Shell — 交互式命令行

✅ 堆分配器 — 基于 linked_list_allocator

✅ PMM + 分页 — 物理内存管理和页表

✅ 进程 — 隔离地址空间，Ring 3 切换

✅ 系统调用 — int 0x80（write/exit）

🛠 构建与运行
bash
git clone https://github.com/makgeer/my_kernel.git
cd my_kernel
rustup target add x86_64-unknown-none
cargo build --release --features fb
./make_iso.sh
./bochs-bin/bin/bochs -q -f bochsrc.bxrc
📄 许可证
Apache 2.0 — 自由使用、修改和分发。

🤝 联系方式
作者: makgeer

GitHub: https://github.com/makgeer

🇯🇵 日本語
Malinka — Rust マイクロカーネル
Malinka は、Rust で書かれた軽量で安全なモジュラー型マイクロカーネルです。x86_64 向けの研究用 OS としてゼロから構築され、ミニマリズム、パフォーマンス、コンポーネントの分離に焦点を当てています。

🚀 特徴
✅ マイクロカーネルアーキテクチャ — サービスはユーザースペースで動作

✅ メモリ安全性 — Rust で記述（no_std、安全な抽象化）

✅ 64 ビット — x86_64 を完全サポート

✅ Limine ブートローダ — モダンなブートプロトコル

✅ Framebuffer + VGA — グラフィカル出力とシリアル出力

✅ 割り込み — IDT、GDT、TSS、IST、PIC（IRQ0、IRQ1）

✅ 協調スケジューラ — タスク対応

✅ シェル — 対話型コマンドライン

✅ ヒープアロケータ — linked_list_allocator ベース

✅ PMM + ページング — 物理メモリ管理とページテーブル

✅ プロセス — 分離されたアドレス空間、Ring 3 への移行

✅ システムコール — int 0x80（write/exit）

🛠 ビルドと実行
bash
git clone https://github.com/makgeer/my_kernel.git
cd my_kernel
rustup target add x86_64-unknown-none
cargo build --release --features fb
./make_iso.sh
./bochs-bin/bin/bochs -q -f bochsrc.bxrc
📄 ライセンス
Apache 2.0 — 自由に使用、変更、再配布が可能です。

🤝 連絡先
作者: makgeer

GitHub: https://github.com/makgeer

This response is AI-generated, for reference only.
