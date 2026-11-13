use crate::interrupts::KEYBOARD_BUFFER;
use crate::print;
use crate::println;

/// История команд — кольцевой буфер последних 25 команд.
/// Код выделен бирюзовым цветом (правило .kodaruls п.6).
const HISTORY_MAX: usize = 25;
const CMD_LEN: usize = 64;
static mut HISTORY: [[u8; CMD_LEN]; HISTORY_MAX] = [[0u8; CMD_LEN]; HISTORY_MAX];
static mut HISTORY_COUNT: usize = 0;
static mut HISTORY_HEAD: usize = 0; // индекс куда писать следующую команду

/// Специальные клавиши, распознаваемые из ESC-последовательностей.
enum SpecialKey {
    Up,
    Down,
    Left,
    Right,
}

/// Байты, прочитанные при разборе ESC-последовательности, но не
/// составившие её. Возвращаются в общий поток ввода.
static PUSHBACK: spin::Mutex<heapless::Vec<u8, 8>> =
    spin::Mutex::new(heapless::Vec::new());

/// Можно ли читать COM1 напрямую из ядра.
///
/// Если IRQ4 заявлен user-space драйвером (seriald), UART принадлежит
/// ему: ядро читает байты только из KEYBOARD_BUFFER, куда драйвер
/// пересылает их через IPC. Иначе байты «уводились» бы у драйвера.
fn serial_direct_available() -> bool {
    crate::process_manager::irq_owner(4).is_none() && crate::serial::serial_has_data()
}

/// Возвращает байт, отложенный разбором ESC-последовательности.
fn push_back(byte: u8) {
    // insert(0, ...) сохраняет порядок: байты возвращаются в том же
    // порядке, в котором были прочитаны.
    let _ = PUSHBACK.lock().insert(0, byte);
}

/// Один байт ввода: сначала отложенные, затем COM1, затем PS/2-буфер.
fn poll_byte() -> Option<u8> {
    {
        let mut pb = PUSHBACK.lock();
        if !pb.is_empty() {
            return Some(pb.remove(0));
        }
    }
    if serial_direct_available() {
        return Some(crate::serial::serial_read());
    }
    let mut buffer = KEYBOARD_BUFFER.lock();
    if !buffer.is_empty() {
        return Some(buffer.remove(0));
    }
    None
}

/// Ждёт байт ввода бесконечно (первый байт нажатия).
///
/// sti; hlt вместо busy-wait: CPU спит до прерывания (IRQ1/IRQ4
/// положат байт в буфер), Bochs не крутит холостой цикл.
///
/// set_shell_idle(true) разрешает таймеру запускать user-процессы:
/// только здесь ядро действительно простаивает и не держит блокировки.
fn wait_byte() -> u8 {
    loop {
        if let Some(b) = poll_byte() {
            crate::process_manager::set_shell_idle(false);
            return b;
        }
        crate::process_manager::set_shell_idle(true);
        unsafe { core::arch::asm!("sti", "hlt", options(nomem, nostack)); }
        crate::process_manager::set_shell_idle(false);
    }
}

/// Ждёт байт не дольше одного тика PIT (10 мс).
///
/// Нужен для остатка ESC-последовательности: одиночное нажатие Esc не
/// должно «съедать» следующий символ, если он придёт позже.
fn wait_byte_timeout() -> Option<u8> {
    let start = crate::interrupts::TIMER_TICKS.load(core::sync::atomic::Ordering::Relaxed);
    loop {
        if let Some(b) = poll_byte() {
            crate::process_manager::set_shell_idle(false);
            return Some(b);
        }
        let now = crate::interrupts::TIMER_TICKS.load(core::sync::atomic::Ordering::Relaxed);
        if now.wrapping_sub(start) >= 1 {
            crate::process_manager::set_shell_idle(false);
            return None;
        }
        crate::process_manager::set_shell_idle(true);
        unsafe { core::arch::asm!("sti", "hlt", options(nomem, nostack)); }
        crate::process_manager::set_shell_idle(false);
    }
}

/// Разбирает ESC [ A/B/C/D (стрелки). Вызывается после чтения ESC.
///
/// Работает одинаково для serial (nc присылает ESC-последовательность)
/// и для PS/2 (keyboard_handler кладёт ту же последовательность в буфер).
fn parse_escape_sequence() -> Option<SpecialKey> {
    let second = wait_byte_timeout()?;
    if second != b'[' {
        push_back(second);
        return None;
    }
    let third = match wait_byte_timeout() {
        Some(b) => b,
        None => {
            push_back(b'[');
            return None;
        }
    };
    match third {
        b'A' => Some(SpecialKey::Up),
        b'B' => Some(SpecialKey::Down),
        b'C' => Some(SpecialKey::Right),
        b'D' => Some(SpecialKey::Left),
        _ => {
            // Не стрелка — возвращаем прочитанное в поток ввода.
            push_back(third);
            push_back(b'[');
            None
        }
    }
}

/// Читает одну «клавишу»: байт COM1/PS/2 или стрелку.
/// Возвращает Ok(char) для обычного символа (включая одиночный ESC)
/// либо Err(SpecialKey) для стрелки.
fn read_key() -> Result<char, SpecialKey> {
    let byte = wait_byte();
    if byte == 0x1b {
        if let Some(special) = parse_escape_sequence() {
            return Err(special);
        }
        return Ok('\u{1b}');
    }
    Ok(byte as char)
}

/// Читает строку до Enter. Возвращает набранные символы.
/// Поддерживает Backspace и историю команд (стрелки вверх/вниз).
/// Эхо печатается ВСЕГДА (и для serial, и для PS/2).
///
/// Код выделен бирюзовым цветом (правило .kodaruls п.6) — добавлена
/// история команд с навигацией по стрелкам.
pub fn read_line() -> [u8; 64] {
    let mut input_buf = [0u8; 64];
    let mut pos = 0;
    let mut input_len = 0;

    // Индекс для навигации по истории. start = HISTORY_COUNT означает
    // "новая пустая строка". При нажатии вверх уменьшаем, вниз — увеличиваем.
    let mut hist_idx: usize;

    unsafe {
        hist_idx = HISTORY_COUNT;
    }

    loop {
        let c = match read_key() {
            Ok(c) => c,
            Err(SpecialKey::Up) => {
                unsafe {
                    if HISTORY_COUNT == 0 || hist_idx == 0 {
                        continue;
                    }
                    hist_idx -= 1;
                    let cmd_idx = history_index(hist_idx);
                    while pos > 0 {
                        pos -= 1;
                        crate::print!("\u{8} \u{8}");
                    }
                    input_len = 0;
                    for i in 0..CMD_LEN {
                        if HISTORY[cmd_idx][i] == 0 { break; }
                        if pos < input_buf.len() - 1 {
                            input_buf[pos] = HISTORY[cmd_idx][i];
                            pos += 1;
                            input_len += 1;
                            crate::print!("{}", HISTORY[cmd_idx][i] as char);
                        }
                    }
                }
                continue;
            }
            Err(SpecialKey::Down) => {
                unsafe {
                    if hist_idx >= HISTORY_COUNT {
                        while pos > 0 {
                            pos -= 1;
                            crate::print!("\u{8} \u{8}");
                        }
                        input_len = 0;
                        continue;
                    }
                    hist_idx += 1;
                    while pos > 0 {
                        pos -= 1;
                        crate::print!("\u{8} \u{8}");
                    }
                    input_len = 0;
                    if hist_idx < HISTORY_COUNT {
                        let cmd_idx = history_index(hist_idx);
                        for i in 0..CMD_LEN {
                            if HISTORY[cmd_idx][i] == 0 { break; }
                            if pos < input_buf.len() - 1 {
                                input_buf[pos] = HISTORY[cmd_idx][i];
                                pos += 1;
                                input_len += 1;
                                crate::print!("{}", HISTORY[cmd_idx][i] as char);
                            }
                        }
                    }
                }
                continue;
            }
            Err(SpecialKey::Left) => {
                if pos > 0 {
                    pos -= 1;
                    crate::print!("\u{8}");
                }
                continue;
            }
            Err(SpecialKey::Right) => {
                if pos < input_len {
                    crate::print!("{}", input_buf[pos] as char);
                    pos += 1;
                }
                continue;
            }
        };

        match c {
            // Enter — конец строки
            '\n' | '\r' => {
                println!();
                if input_len > 0 {
                    unsafe {
                        let slot = HISTORY_HEAD % HISTORY_MAX;
                        // Копируем только введённые байты: иначе удалённые
                        // Backspace'ом символы останутся в истории.
                        for i in 0..CMD_LEN {
                            HISTORY[slot][i] = if i < input_len { input_buf[i] } else { 0 };
                        }
                        HISTORY_HEAD += 1;
                        if HISTORY_COUNT < HISTORY_MAX {
                            HISTORY_COUNT += 1;
                        }
                    }
                }
                break;
            }
            // Backspace — удаляет символ перед курсором.
            '\u{8}' | '\u{7f}' => {
                if pos > 0 {
                    // Сдвигаем хвост влево: Backspace работает и в середине
                    // строки (после навигации стрелками влево/вправо).
                    for i in (pos - 1)..input_len.saturating_sub(1) {
                        input_buf[i] = input_buf[i + 1];
                    }
                    input_len -= 1;
                    input_buf[input_len] = 0;
                    pos -= 1;
                    // Перерисовываем хвост и стираем последний символ.
                    for i in pos..input_len {
                        crate::print!("{}", input_buf[i] as char);
                    }
                    crate::print!(" \u{8}");
                }
                hist_idx = unsafe { HISTORY_COUNT };
            }
            // Одиночный ESC (не стрелка) — игнорируем.
            '\u{1b}' => {}
            // Обычный печатный символ
            c => {
                if pos < input_buf.len() - 1 && c.is_ascii() {
                    input_buf[pos] = c as u8;
                    pos += 1;
                    input_len += 1;
                    crate::print!("{}", c);
                }
                hist_idx = unsafe { HISTORY_COUNT };
            }
        }
    }
    // Хвост после input_len обнуляем: после навигации по истории и
    // Backspace в буфере могли остаться байты, которых нет в команде.
    for i in input_len..input_buf.len() {
        input_buf[i] = 0;
    }
    input_buf
}

/// Возвращает индекс в массиве HISTORY для смещения hist_idx от начала.
/// hist_idx=0 → самая свежая команда, hist_idx=COUNT-1 → самая старая.
///
/// Код выделен бирюзовым цветом (правило .kodaruls п.6).
///
/// ИСПРАВЛЕНИЕ: при hist_idx=0 нужно получить самый свежий слот,
/// который всегда = (HISTORY_HEAD - 1) % HISTORY_MAX.
/// При hist_idx=N сдвигаемся назад на N позиций в кольце.
fn history_index(hist_idx: usize) -> usize {
    unsafe {
        if HISTORY_COUNT == 0 {
            return 0;
        }
        if HISTORY_COUNT < HISTORY_MAX {
            // Буфер не заполнен: команды в слотах [0..COUNT).
            // hist_idx=0 → самый свежий = COUNT-1, hist_idx=COUNT-1 → самый старый = 0.
            HISTORY_COUNT - 1 - hist_idx
        } else {
            // Буфер полный: кольцо. Самый свежий слот = (HEAD - 1) % MAX.
            // Сдвигаемся назад на hist_idx позиций.
            ((HISTORY_HEAD as u64).wrapping_sub(hist_idx as u64) % HISTORY_MAX as u64) as usize
        }
    }
}

pub fn shell() {
    println!();
    println!("Welcome to Malinka shell!");
    println!("Type 'help' for commands");

    loop {
        print!("> ");
        let input_buf = read_line();
        let cmd = core::str::from_utf8(&input_buf)
            .unwrap_or("")
            .trim_end_matches('\0')
            .trim();

        match cmd {
            "help" => {
                println!("Commands:");
                println!("  help    - show this help");
                println!("  echo    - echo a line");
                println!("  clear   - clear the screen");
                println!("  reboot  - reboot the system (aliases: rb, rebut)");
                println!("  hlt     - halt the CPU");
                println!("  exit    - stop the system and leave the shell");
                println!("  about   - about this kernel");
                println!("  tasks   - run the task scheduler demo");
                println!("  cursor  - show terminal cursor position");
                println!("  color   - 16-color palette demo (ANSI SGR)");
                println!("  test_alloc - test heap allocator (Box/Vec/String)");
                println!("  test_pmm  - test physical memory manager (alloc/dealloc frames)");
                println!("  test_paging - test page mapping (map/unmap/translate)");
                println!("  test_process - create user process (P4, code, stack)");
                println!("  run_user   - create and run user process in ring 3");
                println!("  run_elf    - load and run ELF64 user program (ring 3)");
                println!("  run_syscall - ELF with read/write syscalls (multi-segment)");
                println!("  run_multi  - run 2 processes (loop_a + loop_b) for preemptive scheduler test");
                println!("  test_ipc   - run 2 processes (ipc_sender + ipc_receiver) for IPC test");
                println!("  arrow_test - log all keyboard bytes (hex) to debug Bochs arrows");
                println!("  ps         - list all processes (PID, state, name)");
                println!("  kill <pid> - terminate a process by PID");
                println!("  ls         - list files in VFS (initrd)");
                println!("  cat <file> - print file contents");
                println!("  run <file> - load and run ELF program from VFS");
                println!("  btrfs      - show btrfs disk status (ATA secondary)");
                println!("  btrfs-ls [path] - list btrfs directory");
                println!("  btrfs-cat <path> - print btrfs file contents");
            }
            "about" => {
                println!("Malinka v0.1.0");
                println!("x86_64 OS kernel written in Rust");
                println!("Bootloader: Limine");
            }
            "reboot" | "rb" | "rebut" => {
                println!("Rebooting...");
                // Порт 0x604 — клавиатурный контроллер, бит 0 (A20) + reset.
                // Порт 0xCF9 — южный мост (chipset reset port).
                unsafe {
                    // Способ 1: южный мост (CF9)
                    let mut port = x86_64::instructions::port::Port::<u8>::new(0xCF9);
                    let val = port.read();
                    port.write(val | 0x6); // bit1=A20, bit2=reset
                }
                // Способ 2: клавиатурный контроллер (fallback)
                loop {
                    unsafe {
                        let mut port = x86_64::instructions::port::Port::<u8>::new(0x64);
                        loop {
                            let st = port.read();
                            if st & 0x1 == 0 { break; }
                        }
                        port.write(0xFE); // keyboard reset
                    }
                    for _ in 0..100000 {
                        unsafe { core::arch::asm!("nop"); }
                    }
                }
            }
            "hlt" => {
                println!("Halting CPU...");
                loop {
                    unsafe { core::arch::asm!("hlt"); }
                }
            }
            "exit" => {
                // Shell — единственный поток ядра: «выход» означает
                // остановку системы. cli+hlt не реагирует на прерывания,
                // поэтому CPU гарантированно остаётся остановленным.
                println!("Exiting shell — system halted.");
                println!("Reset or close Bochs to continue.");
                loop {
                    unsafe { core::arch::asm!("cli", "hlt"); }
                }
            }
            "brush" => {
                // Вход во встроенную bash-совместимую оболочку.
                // Возврат в kernel shell — командой `exit` внутри brush.
                crate::brush::run();
            }
            "script" => {
                // Команда без аргументов — выводим usage
                println!("Usage: script \"<rhai code>\" (e.g. script \"print(40+2)\")");
            }
            "tasks" | "task" => {
                // Запускаем одну итерацию планировщика как демонстрацию
                crate::scheduler::run_scheduler_once();
            }
            "ps" => {
                // Список процессов в менеджере процессов (вытесняющий планировщик)
                crate::process_manager::list_processes();
            }
            "ipcstat" => {
                // Статистика IPC: отправлено / принято / ошибок.
                crate::process_manager::print_ipc_stats();
            }
            "irqstat" => {
                // Кто владеет аппаратными прерываниями (ядро или
                // user-space драйвер). Демонстрация выноса драйверов.
                for irq in 0..16u8 {
                    match crate::process_manager::irq_owner(irq) {
                        Some(pid) => println!("IRQ{:2}: user driver (PID={})", irq, pid),
                        None => println!("IRQ{:2}: kernel", irq),
                    }
                }
            }
            "keyboardd" => {
                // Запуск PS/2 драйвера в USER SPACE (Ring 3).
                // Драйвер забирает IRQ1 (irq_claim), ждёт прерывания
                // (irq_wait), читает порт 0x60 (io_in) и отправляет
                // символы в консоль ядра через IPC (ipc_send на PID 0).
                // Выход — Esc. После завершения IRQ1 возвращается ядру.
                println!("Loading /bin/keyboardd.elf (user-space PS/2 driver)...");
                match crate::process::load_program_from_fs("/bin/keyboardd.elf") {
                    Ok(proc) => {
                        println!("Launching keyboardd ring 3...");
                        crate::process::run_process(&proc);
                    }
                    Err(e) => println!("  FAILED: {}", e),
                }
            }
            "seriald" => {
                // Запуск драйвера COM1 в USER SPACE (Ring 3).
                // Проверяемый в headless-Bochs пример: ввод из nc идёт
                // на COM1 → IRQ4 → irq_wait → io_in(0x3F8) → ipc_send(0).
                // Выход — Ctrl-C.
                println!("Loading /bin/seriald.elf (user-space COM1 driver)...");
                match crate::process::load_program_from_fs("/bin/seriald.elf") {
                    Ok(proc) => {
                        println!("Launching seriald ring 3...");
                        crate::process::run_process(&proc);
                    }
                    Err(e) => println!("  FAILED: {}", e),
                }
            }
            // kill <pid>: match по всей строке не сработал бы для
            // команды с аргументом ("kill 2" != "kill"), поэтому нужен
            // guard по префиксу. Внутри PID берётся из cmd[5..].
            cmd if cmd == "kill" || cmd.starts_with("kill ") => {
                // Завершение процесса по PID (для отладки планировщика).
                // PID не равен индексу слота: ищем процесс по pid,
                // освобождаем его IRQ и будим ожидающих отправителей.
                if cmd.len() > 5 {
                    let pid_str = cmd[5..].trim();
                    match pid_str.parse::<usize>() {
                        Ok(pid) => {
                            let terminated =
                                crate::process_manager::terminate_by_pid(pid);
                            if terminated {
                                println!("  Process PID={} terminated", pid);
                            } else {
                                println!("  kill: PID {} not found", pid);
                            }
                        }
                        Err(_) => {
                            println!("  Usage: kill <pid>");
                        }
                    }
                } else {
                    println!("  Usage: kill <pid>");
                }
            }
            "cursor" => {
                // Диагностика: выводим логическую позицию курсора терминала.
                // Сравниваем с визуальным положением — если не совпадает,
                // проблема в рендере (draw_serial/draw_vga/draw_fb).
                let (r, c) = crate::terminal::TERMINAL.lock().position();
                println!("cursor: row={}, col={}", r, c);
            }
            "clear" => {
                // Очистка через терминал: clear_screen() очищает буфер 80x25,
                // сбрасывает курсор и шлёт ANSI-коды очистки в serial.
                // framebuffer/VGA перерисуются при следующем flush().
                crate::terminal::TERMINAL.lock().clear_screen();
                crate::terminal::TERMINAL.lock().flush();
            }
            "test_alloc" => {
                // Проверка аллокатора кучи: выделяем Box, Vec и String,
                // записываем/читаем данные — убеждаемся, что alloc работает.
                use alloc::boxed::Box;
                use alloc::string::String;
                use alloc::vec::Vec;

                // Box: одиночное значение на куче
                let b = Box::new(42u32);
                println!("Box: {} (0x{:x})", *b, &*b as *const u32 as usize);

                // Vec: динамический массив с ростом
                let mut v: Vec<u32> = Vec::new();
                for i in 0..100 {
                    v.push(i as u32);
                }
                let sum: u32 = v.iter().sum();
                println!("Vec: len={}, sum={}", v.len(), sum);

                // String: текст на куче
                let s = String::from("Hello from heap allocator!");
                println!("String: '{}' ({} bytes)", s, s.len());

                // Много мелких выделений подряд — стресс-тест free list
                let mut boxes = Vec::new();
                for i in 0..50 {
                    boxes.push(Box::new(i as u64));
                }
                let last = **boxes.last().unwrap();
                println!("Stress: {} Boxes allocated, last={}", boxes.len(), last);

                println!("Allocator test PASSED");
            }
            "test_pmm" => {
                // Проверка физического менеджера памяти:
                // выделяем 10 фреймов, выводим их адреса, освобождаем,
                // проверяем, что free_count вернулся к исходному.
                let before = crate::pmm::free_count();
                println!("Free frames before: {}", before);

                let mut frames: [u64; 10] = [0; 10];
                let mut allocated = 0;
                for i in 0..10 {
                    match crate::pmm::alloc_frame() {
                        Some(addr) => {
                            frames[i] = addr;
                            allocated += 1;
                            println!("  alloc[{}] = 0x{:016x}", i, addr);
                        }
                        None => {
                            println!("  alloc[{}] FAILED - out of memory!", i);
                            break;
                        }
                    }
                }

                let after_alloc = crate::pmm::free_count();
                println!("Free frames after alloc: {} (delta={})",
                         after_alloc, before as isize - after_alloc as isize);

                // Освобождаем все выделенные фреймы
                for i in 0..allocated {
                    crate::pmm::dealloc_frame(frames[i]);
                }

                let after_dealloc = crate::pmm::free_count();
                println!("Free frames after dealloc: {} (delta={})",
                         after_dealloc, after_dealloc as isize - after_alloc as isize);

                if after_dealloc == before {
                    println!("PMM test PASSED");
                } else {
                    println!("PMM test WARNING: frame count mismatch ({} != {})",
                             after_dealloc, before);
                }
            }
            "test_paging" => {
                // Проверка пейджинга: мапим тестовую виртуальную страницу
                // на свежий физический фрейм, пишем/читаем данные, анмапим.
                use x86_64::structures::paging::page_table::PageTableFlags;

                // Выбираем виртуальный адрес в неиспользуемой области
                // (выше HHDM, ниже ядра — пространство для user-маппингов)
                let test_virt: u64 = 0x0000_4000_0000_0000;
                println!("Test virtual address: 0x{:016x}", test_virt);

                // 1. Проверка translate до маппинга — должно быть None
                match crate::paging::translate(test_virt) {
                    Some(addr) => println!(
                        "  WARNING: page already mapped to 0x{:016x}", addr
                    ),
                    None => println!("  OK: page not mapped before map_page"),
                }

                // 2. Выделяем физический фрейм и мапим его
                let phys = match crate::pmm::alloc_frame() {
                    Some(addr) => addr,
                    None => {
                        println!("  FAILED: PMM out of memory");
                        return;
                    }
                };
                println!("  Allocated phys frame: 0x{:016x}", phys);

                // Флаги: PRESENT | WRITABLE (страница ядра, без USER_ACCESSIBLE)
                let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
                match crate::paging::map_page(test_virt, phys, flags) {
                    Ok(()) => println!("  OK: map_page succeeded"),
                    Err(e) => {
                        println!("  FAILED: map_page error: {}", e);
                        crate::pmm::dealloc_frame(phys);
                        return;
                    }
                }

                // 3. Проверка translate после маппинга
                match crate::paging::translate(test_virt) {
                    Some(addr) => {
                        if addr == phys {
                            println!(
                                "  OK: translate matches (0x{:016x})", addr
                            );
                        } else {
                            println!(
                                "  WARNING: translate mismatch: got 0x{:016x}, expected 0x{:016x}",
                                addr, phys
                            );
                        }
                    }
                    None => println!("  FAILED: translate returned None after map"),
                }

                // 4. Запись и чтение по замапленному виртуальному адресу
                unsafe {
                    let ptr = test_virt as *mut u64;
                    // Записываем маркерное значение
                    *ptr = 0xDEAD_BEEF_CAFE_BABE;
                    // Читаем обратно
                    let val = *ptr;
                    if val == 0xDEAD_BEEF_CAFE_BABE {
                        println!("  OK: write/read test passed (0x{:016x})", val);
                    } else {
                        println!(
                            "  FAILED: read mismatch: got 0x{:016x}",
                            val
                        );
                    }
                }

                // 5. Анмапим страницу
                match crate::paging::unmap_page(test_virt) {
                    Ok(()) => println!("  OK: unmap_page succeeded"),
                    Err(e) => println!("  FAILED: unmap_page error: {}", e),
                }

                // 6. Проверка translate после анмаппинга — должно быть None
                match crate::paging::translate(test_virt) {
                    Some(addr) => println!(
                        "  WARNING: still mapped to 0x{:016x} after unmap", addr
                    ),
                    None => println!("  OK: page not mapped after unmap_page"),
                }

                // Освобождаем физический фрейм
                crate::pmm::dealloc_frame(phys);

                println!("Paging test PASSED");
            }
            "test_process" => {
                // Проверка создания пользовательского процесса:
                // создаём P4, мапим код и стек, проверяем translate_in_p4.
                // Запуск в ring 3 — следующий шаг (нужен обработчик int 0x80).

                // Тестовый код: mov rax, 0x42; int 0x80; jmp $
                // B8 42 00 00 00  CD 80  EB FE
                let user_code: &[u8] = &[
                    0xB8, 0x42, 0x00, 0x00, 0x00, // mov eax, 0x42
                    0xCD, 0x80,                     // int 0x80
                    0xEB, 0xFE,                     // jmp $ (бесконечный цикл)
                ];

                println!("Creating user process with {} bytes of code...", user_code.len());

                match crate::process::create_process(user_code, 0) {
                    Ok(proc) => {
                        println!("  P4 phys: 0x{:016x}", proc.p4_phys);
                        println!("  Entry:   0x{:016x}", proc.entry_point);
                        println!("  Stack:   0x{:016x}", proc.stack_top);

                        // Проверяем translate_in_p4 для точки входа
                        match crate::paging::translate_in_p4(proc.p4_phys, proc.entry_point) {
                            Some(phys) => {
                                println!("  OK: entry translates to phys 0x{:016x}", phys);

                                // Проверяем, что код скопирован правильно
                                let hhdm = crate::paging::hhdm_offset();
                                unsafe {
                                    let code_ptr = (phys + hhdm) as *const u8;
                                    // Первая инструкция: 0xB8 (mov eax, imm32)
                                    if *code_ptr == 0xB8 {
                                        println!("  OK: code verified at phys (first byte 0x{:02x})", *code_ptr);
                                    } else {
                                        println!("  WARNING: code mismatch (first byte 0x{:02x})", *code_ptr);
                                    }
                                }
                            }
                            None => println!("  FAILED: entry point not mapped in P4"),
                        }

                        // Проверяем translate для стека
                        let stack_page = proc.stack_top - 4096;
                        match crate::paging::translate_in_p4(proc.p4_phys, stack_page) {
                            Some(phys) => println!("  OK: stack page translates to phys 0x{:016x}", phys),
                            None => println!("  FAILED: stack page not mapped in P4"),
                        }

                        // Проверяем изоляцию: kernel entry (0xFFFFFFFF80000000)
                        // должен быть замаплен в новой P4
                        let kernel_addr: u64 = 0xFFFFFFFF80000000;
                        match crate::paging::translate_in_p4(proc.p4_phys, kernel_addr) {
                            Some(_) => println!("  OK: kernel space mapped in user P4 (shared)"),
                            None => println!("  WARNING: kernel space NOT mapped in user P4!"),
                        }

                        // Проверяем, что user-код НЕ замаплен в kernel P4
                        // (изоляция: kernel не должен видеть user pages по этому адресу)
                        match crate::paging::translate(crate::process::USER_CODE_BASE) {
                            Some(_) => println!("  WARNING: user code visible in kernel P4"),
                            None => println!("  OK: user code isolated from kernel P4"),
                        }

                        println!("Process creation test PASSED");
                        println!("  (Ring 3 execution — next step, needs int 0x80 handler)");
                    }
                    Err(e) => {
                        println!("  FAILED: create_process error: {}", e);
                    }
                }
            }
            "run_user" => {
                // Запуск пользовательского процесса в ring 3.
                // Код демонстрирует системные вызовы write и exit:
                //   write(1, msg, 22)  — вывод строки из user space
                //   exit(0)            — завершение процесса, возврат в shell
                //
                // Смещения байт-кода (вычислены вручную):
                //   0:  B8 01 00 00 00           mov eax, 1          (write)
                //   5:  48 C7 C7 01 00 00 00     mov rdi, 1          (fd=stdout)
                //   12: 48 BE <8 байт>           mov rsi, msg_addr   (buf)
                //   22: 48 BA <8 байт>           mov rdx, 22         (len)
                //   32: CD 80                    int 0x80
                //   34: B8 3C 00 00 00           mov eax, 60         (exit)
                //   39: 48 C7 C7 00 00 00 00     mov rdi, 0          (code=0)
                //   46: CD 80                    int 0x80
                //   48: EB FE                    jmp $  (страховка)
                //   50: "Hello from user mode!\n" — строка (22 байта)
                // msg_addr = USER_CODE_BASE + 50 = 0x0000400000000032
                let msg_addr: u64 = crate::process::USER_CODE_BASE + 50;
                println!("User message at: 0x{:x}", msg_addr);
                let user_code: &[u8] = &[
                    // write(1, msg, 22)
                    0xB8, 0x01, 0x00, 0x00, 0x00,             // mov eax, 1
                    0x48, 0xC7, 0xC7, 0x01, 0x00, 0x00, 0x00, // mov rdi, 1
                    // mov rsi, msg_addr (little-endian)
                    // msg_addr = 0x0000400000000032 → байты LE:
                    //   32 00 00 00 | 00 40 00 00
                    // (0x40 — на байте 5, т.е. биты 40-47: 0x40 << 40 = 0x400000000000)
                    // ИСТОРИЯ ОШИБОК (позиция 0x40 в imm64):
                    //   байт 6: 0x40 << 48 = 0x0040000000000032 (не canonical)
                    //   байт 4: 0x40 << 32 = 0x0000004000000032 (не тот сегмент)
                    //   байт 5: 0x40 << 40 = 0x0000400000000032 ← ПРАВИЛЬНО
                    // Ориентир: в hello.asm (run_elf) lea rsi,[rel msg] даёт
                    // 0x40 на байте 5 — и ELF-вариант работает.
                    0x48, 0xBE,
                    0x32, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00,
                    // mov rdx, 22
                    0x48, 0xBA,
                    0x16, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                    0xCD, 0x80,                               // int 0x80
                    // exit(0)
                    0xB8, 0x3C, 0x00, 0x00, 0x00,             // mov eax, 60
                    0x48, 0xC7, 0xC7, 0x00, 0x00, 0x00, 0x00, // mov rdi, 0
                    0xCD, 0x80,                               // int 0x80
                    0xEB, 0xFE,                               // jmp $
                    // Строка для write
                    b'H', b'e', b'l', b'l', b'o', b' ',
                    b'f', b'r', b'o', b'm', b' ',
                    b'u', b's', b'e', b'r', b' ',
                    b'm', b'o', b'd', b'e', b'!', b'\n',
                ];

                println!("Creating and launching user process in ring 3...");

                match crate::process::create_process(user_code, 0) {
                    Ok(proc) => {
                        println!("Launching ring 3...");
                        // run_process переключает CR3 и вызывает enter_ring3.
                        // После int 0x80 → syscall_entry → syscall_handler_rust
                        // → kernel_return_to_shell.
                        crate::process::run_process(&proc);
                        // Сюда не вернёмся — enter_ring3 noreturn,
                        // возврат идёт через kernel_return_to_shell.
                    }
                    Err(e) => {
                        println!("  FAILED: create_process error: {}", e);
                    }
                }
            }
            "run_elf" => {
                // Загрузка и запуск ELF64-программы в ring 3.
                // ELF встраивается в ядро через include_bytes! на этапе сборки.
                // Точка входа берётся из ELF header (e_entry), сегменты
                // загружаются по адресам из PT_LOAD program headers.
                //
                // Сборка user-программы: автоматически в build.rs
                // → user_programs/hello.elf (ET_EXEC, x86_64, static)
                static HELLO_ELF: &[u8] = include_bytes!("../user_programs/hello.elf");

                println!("Loading ELF user program ({} bytes)...", HELLO_ELF.len());

                match crate::process::create_elf_process(HELLO_ELF) {
                    Ok(proc) => {
                        println!("Launching ELF ring 3 at entry 0x{:016x}...", proc.entry_point);
                        crate::process::run_process(&proc);
                        // Сюда не вернёмся — run_process noreturn,
                        // возврат идёт через kernel_return_to_shell.
                    }
                    Err(e) => {
                        println!("  FAILED: create_elf_process error: {}", e);
                    }
                }
            }
"run_syscall" => {
                // Загрузка и запуск user-программы syscall_test с НОВЫМИ
                // системными вызовами: read(0,...) — ввод с клавиатуры/COM1,
                // write(1,...) — вывод. Программа имеет ДВА PT_LOAD сегмента
                // (R E для кода, RW для данных) — проверка загрузчика ELF.
                //
                // Сборка: автоматически в build.rs
                // → user_programs/syscall_test.elf
                static SYSCALL_ELF: &[u8] = include_bytes!("../user_programs/syscall_test.elf");

                println!("Loading syscall_test ({} bytes)...", SYSCALL_ELF.len());

                match crate::process::create_elf_process(SYSCALL_ELF) {
                    Ok(proc) => {
                        println!("Launching syscall_test ring 3 (read/write demo)...");
                        crate::process::run_process(&proc);
                        // Сюда не вернёмся — run_process noreturn,
                        // возврат идёт через kernel_return_to_shell.
                    }
                    Err(e) => {
                        println!("  FAILED: create_elf_process error: {}", e);
                    }
                }
            }
            "run_multi" => {
                // Тест вытесняющего планировщика (правило .kodaruls п.12):
                // запускает 2 процесса через src/tests.rs::test_scheduler().
                // Ожидаемый вывод: чередование "A" и "B", затем возврат в shell.
                crate::tests::test_scheduler();
                // test_scheduler не возвращается (run_process noreturn).
                // Код ниже не выполняется — возврат через kernel_return_to_shell.
            }
            "test_ipc" => {
                // Тест IPC: отправитель шлёт 3 сообщения получателю.
                // Ожидаемый вывод: "MSG1HELLOWORLD", затем возврат в shell.
                crate::tests::test_ipc();
                // test_ipc не возвращается (run_process noreturn).
            }
            "arrow_test" => {
                // Тест клавиатуры: читает stdin, печатает hex-байты.
                // Программа берётся из initrd (/bin/arrow_test.elf), а не
                // через include_bytes! — проверяем VFS-путь загрузки ELF.
                println!("Loading /bin/arrow_test.elf from VFS...");
                match crate::process::load_program_from_fs("/bin/arrow_test.elf") {
                    Ok(proc) => {
                        println!("Launching arrow_test ring 3...");
                        crate::process::run_process(&proc);
                    }
                    Err(e) => {
                        println!("  FAILED: {}", e);
                    }
                }
            }
            "color" => {
                // Демонстрация 16-цветной палитры ANSI SGR.
                // Цвета печатаются на framebuffer (шрифт 8x16) + VGA + serial.
                println!("16-Color Palette Demo:");
                for normal_fg in 0..8u8 {
                    for bright_fg in 0..8u8 {
                        print!("\x1b[1m{:2}\x1b[0m ", normal_fg + bright_fg);
                    }
                    println!();
                }
                println!();
                // Цветные фоны
                println!("Background colors:");
                for bg in 0..8u8 {
                    print!("\x1b[4{}m  {:2}  \x1b[0m ", 40 + bg, bg);
                }
                println!();
                for bg in 8..16u8 {
                    print!("\x1b[10{}m  {:2}  \x1b[0m ", 40 + bg - 8, bg);
                }
                println!();
                // Комбинации
                println!();
                println!("Combinations:");
                println!("\x1b[31mRed text on default\x1b[0m");
                println!("\x1b[41mRed background\x1b[0m");
                println!("\x1b[31m\x1b[41mRed on Red\x1b[0m");
                println!("\x1b[90mBright black\x1b[0m");
                println!("\x1b[91mBright red\x1b[0m");
                println!("\x1b[92mBright green\x1b[0m");
                println!("\x1b[93mBright yellow\x1b[0m");
                println!("\x1b[94mBright blue\x1b[0m");
                println!("\x1b[95mBright magenta\x1b[0m");
                println!("\x1b[96mBright cyan\x1b[0m");
                println!("\x1b[97mBright white\x1b[0m");
            }
            "" => {
                // Пустая команда — просто перенос строки
            }
            _ => {
                if cmd.starts_with("echo ") {
                    println!("{}", &cmd[5..]);
                } else if cmd.starts_with("script ") {
                    // script "<rhai code>" — выполнение встроенного скрипта Rhai
                    let code = cmd[7..].trim_end_matches('\0').trim();
                    if code.is_empty() {
                        println!("Usage: script \"<rhai code>\"");
                    } else {
                        // Убираем кавычки, если они есть
                        let code = code.trim_matches('"');
                        crate::script::run_script(code);
                    }
                } else if cmd == "ls" || cmd.starts_with("ls ") {
                    // ls [path] — список файлов в VFS (initrd).
                    // Работает ТОЛЬКО с initrd, без fallback на btrfs.
                    let path = if cmd.len() > 3 { cmd[3..].trim() } else { "/" };
                    match crate::fs::read_dir(path) {
                        Some(children) => {
                            println!("  {} ({} entries):", path, children.len());
                            for node in children {
                                let typ = match node.node_type {
                                    crate::fs::FsNodeType::File => "FILE",
                                    crate::fs::FsNodeType::Directory => "DIR ",
                                };
                                println!("  {} {:10} {} bytes", typ, node.name, node.size());
                            }
                        }
                        None => {
                            println!("  ls: '{}' not found or not a directory", path);
                        }
                    }
                } else if cmd.starts_with("cat ") {
                    // cat <file> — вывод содержимого файла из VFS (initrd).
                    // Работает ТОЛЬКО с initrd, без fallback на btrfs.
                    let path = cmd[4..].trim();
                    match crate::fs::read_file(path) {
                        Some(data) => {
                            if data.is_empty() {
                                println!("  (empty file)");
                            } else {
                                // Выводим байты как символы (непечатные → '.')
                                for &b in data.iter() {
                                    let c = match b {
                                        b'\n' | b'\r' | 0x20..=0x7e => b as char,
                                        _ => '.',
                                    };
                                    crate::print!("{}", c);
                                }
                                println!();
                            }
                        }
                        None => {
                            println!("  cat: '{}' not found in VFS", path);
                        }
                    }
                } else if cmd.starts_with("run ") {
                    // run <file> — загрузка и запуск ELF из VFS (initrd).
                    // Работает ТОЛЬКО с initrd, без fallback на btrfs.
                    let path = cmd[4..].trim();
                    // Если путь не начинается с /, добавляем /bin/
                    let full_path = if path.starts_with('/') {
                        alloc::string::String::from(path)
                    } else {
                        alloc::format!("/bin/{}", path)
                    };
                    println!("Loading '{}' from VFS...", full_path);
                    match crate::process::load_program_from_fs(&full_path) {
                        Ok(proc) => {
                            println!("ELF loaded: entry=0x{:016x}, P4=0x{:x}", proc.entry_point, proc.p4_phys);
                            crate::process::run_process(&proc);
                            // noreturn — возврат через kernel_return_to_shell
                        }
                        Err(e) => {
                            println!("  FAILED: {}", e);
                        }
                    }
                } else if cmd == "btrfs" {
                    // btrfs — статус btrfs-диска (ATA secondary, 0x170).
                    // Перечитывает суперблок и выводит информацию о ФС.
                    // Код выделен бирюзовым цветом (правило .kodaruls п.6).
                    crate::ata::select_channel(0x170); // secondary
                    match crate::btrfs::read_superblock() {
                        Some(sb) => {
                            println!("btrfs: mounted OK");
                            println!("  root=0x{:x}, chunk_root=0x{:x}",
                                sb.root.get(), sb.chunk_root.get());
                            println!("  nodesize={}, sectorsize={}",
                                sb.nodesize.get(), sb.sectorsize.get());
                            println!("  total_bytes={}", sb.total_bytes.get());
                        }
                        None => {
                            println!("btrfs: mount failed (no superblock or bad magic)");
                        }
                    }
                } else if cmd == "btrfs-ls" || cmd.starts_with("btrfs-ls ") {
                    // btrfs-ls [path] — список файлов в каталоге btrfs-диска.
                    // Код выделен бирюзовым цветом (правило .kodaruls п.6).
                    let path = if cmd.len() > 9 { cmd[9..].trim() } else { "/" };
                    crate::ata::select_channel(0x170);
                    match crate::btrfs::list_dir(path) {
                        Some(entries) => {
                            println!("  {} ({} entries):", path, entries.len());
                            for e in entries {
                                let typ = if e.is_dir { "DIR " } else { "FILE" };
                                println!("  {} inode={}", typ, e.inode);
                                println!("       {}", e.name);
                            }
                        }
                        None => {
                            println!("  btrfs-ls: '{}' not found or not a directory", path);
                        }
                    }
                } else if cmd.starts_with("btrfs-cat ") {
                    // btrfs-cat <path> — вывод содержимого файла с btrfs-диска.
                    // Код выделен бирюзовым цветом (правило .kodaruls п.6).
                    let path = cmd[10..].trim();
                    crate::ata::select_channel(0x170);
                    match crate::btrfs::read_file(path) {
                        Some(data) => {
                            println!("  {} ({} bytes):", path, data.len());
                            for &b in data.iter() {
                                let c = match b {
                                    b'\n' | b'\r' | 0x20..=0x7e => b as char,
                                    _ => '.',
                                };
                                crate::print!("{}", c);
                            }
                            println!();
                        }
                        None => {
                            println!("  btrfs-cat: '{}' not found or unreadable", path);
                        }
                    }
                } else {
                    println!("Unknown command: {}", cmd);
                }
            }
        }
    }
}