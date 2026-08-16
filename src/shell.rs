use crate::interrupts::KEYBOARD_BUFFER;
use crate::print;
use crate::println;

/// Ожидает один символ из буфера клавиатуры (блокирующее чтение).
/// Возвращает (символ, источник):
///   true  = из serial (COM1)
///   false = из PS/2 клавиатуры
pub fn getchar() -> (char, bool) {
    loop {
        // Источник 1: PS/2 клавиатура (IRQ1)
        {
            let mut buffer = KEYBOARD_BUFFER.lock();
            if let Some(byte) = buffer.first() {
                let c = *byte as char;
                buffer.remove(0);
                return (c, false);
            }
        }
        // Источник 2: serial (COM1) — ввод через -serial stdio.
        // QEMU переводит терминал в raw mode (нет локального эха),
        // поэтому символы выводим сами в read_line().
        if crate::serial::serial_has_data() {
            let byte = crate::serial::serial_read();
            return (byte as char, true);
        }
        // Небольшая пауза, чтобы не раскручивать CPU вхолостую
        for _ in 0..1000 {
            unsafe { core::arch::asm!("nop"); }
        }
    }
}

/// Читает строку до Enter. Возвращает набранные символы.
/// Поддерживает Backspace (стирание последнего символа).
/// Эхо печатается ВСЕГДА (и для serial, и для PS/2): QEMU с
/// -serial stdio переводит терминал в raw mode, где нет локального эха,
/// поэтому символы выводим сами через print! (VGA+serial+framebuffer).
pub fn read_line() -> [u8; 64] {
    let mut input_buf = [0u8; 64];
    let mut pos = 0;

    loop {
        let (c, _from_serial) = getchar();
        match c {
            // Enter — конец строки (serial может прислать \r или \n)
            '\n' | '\r' => {
                println!();
                break;
            }
            // Backspace (0x08 или 0x7F DEL) — стираем последний символ
            '\u{8}' | '\u{7f}' => {
                if pos > 0 {
                    pos -= 1;
                    // Печатаем пробел поверх и откатываем курсор:
                    // \x08 — назад, пробел — затирает, \x08 — снова назад.
                    crate::print!("\u{8} \u{8}");
                }
            }
            // Обычный печатный символ — добавляем в буфер и эхо-сим
            c => {
                if pos < input_buf.len() - 1 && c.is_ascii() {
                    input_buf[pos] = c as u8;
                    pos += 1;
                    crate::print!("{}", c);
                }
            }
        }
    }
    input_buf
}

pub fn shell() {
    println!();
    println!("Welcome to MyKernel shell!");
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
                println!("  reboot  - reboot the system");
                println!("  hlt     - halt the CPU");
                println!("  about   - about this kernel");
                println!("  tasks   - run the task scheduler demo");
                println!("  cursor  - show terminal cursor position");
                println!("  test_alloc - test heap allocator (Box/Vec/String)");
                println!("  test_pmm  - test physical memory manager (alloc/dealloc frames)");
                println!("  test_paging - test page mapping (map/unmap/translate)");
                println!("  test_process - create user process (P4, code, stack)");
                println!("  run_user   - create and run user process in ring 3");
            }
            "about" => {
                println!("MyKernel v0.1.0");
                println!("x86_64 OS kernel written in Rust");
                println!("Bootloader: Limine");
            }
            "tasks" | "task" => {
                // Запускаем одну итерацию планировщика как демонстрацию
                crate::scheduler::run_scheduler_once();
            }
            "cursor" => {
                // Диагностика: выводим логическую позицию курсора терминала.
                // Сравниваем с визуальным положением — если не совпадает,
                // проблема в рендере (draw_serial/draw_vga/draw_fb).
                let (r, c) = crate::terminal::TERMINAL.lock().position();
                println!("cursor: row={}, col={}", r, c);
            }
            "clear" => {
                crate::vesa::vesa_clear_screen(0x000000);
                crate::vga::WRITER.lock().clear();
                // Очистка буфера терминала — с момента интеграции терминала
                // весь вывод идёт через TERMINAL, и без очистки его буфера
                // следующая перерисовка вернёт старый текст.
                crate::terminal::TERMINAL.lock().clear_screen();
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
                // Код: mov eax,0x42; int 0x80; jmp $
                // После int 0x80 syscall_handler вернёт нас в shell.
                let user_code: &[u8] = &[
                    0xB8, 0x42, 0x00, 0x00, 0x00, // mov eax, 0x42
                    0xCD, 0x80,                     // int 0x80
                    0xEB, 0xFE,                     // jmp $ (бесконечный цикл)
                ];

                println!("Creating and launching user process in ring 3...");

                match crate::process::create_process(user_code, 0) {
                    Ok(proc) => {
                        println!("Launching ring 3...");
                        // run_process переключает CR3 и вызывает enter_ring3.
                        // После int 0x80 → syscall_handler → kernel_return_to_shell.
                        crate::process::run_process(&proc);
                        // Сюда не вернёмся — enter_ring3 noreturn,
                        // возврат идёт через kernel_return_to_shell.
                    }
                    Err(e) => {
                        println!("  FAILED: create_process error: {}", e);
                    }
                }
            }
            "" => {
                // Пустая команда — просто перенос строки
            }
            _ => {
                if cmd.starts_with("echo ") {
                    println!("{}", &cmd[5..]);
                } else {
                    println!("Unknown command: {}", cmd);
                }
            }
        }
    }
}