#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]

extern crate alloc;

mod video;
mod interrupts;
mod scheduler;
mod tasks;
mod limine_fb;
mod serial;
mod gdt;
mod shell;
mod terminal;
mod allocator;
mod pmm;
mod paging;
mod process;
mod process_manager;
mod elf;
mod script;
mod brush;
mod debug;
mod tests;
mod fs;
mod fs_initrd;
mod ata;
mod btrfs;

use core::panic::PanicInfo;
use crate::scheduler::{add_task};
use crate::tasks::{task_hello, task_counter};
#[cfg(feature = "fb")]
use crate::limine_fb::FRAMEBUFFER_REQUEST;

#[no_mangle]
pub extern "C" fn _start_rust() -> ! {
    // Инициализируем serial (COM1) до первого вывода — чтобы весь лог
    // ядра попадал в последовательный порт (bochs_serial.log)
    serial::init();

    // Инициализируем кучу (8 МБ) — до первой аллокации.
    // Нужно для загрузки ELF, структур процессов, Box/Vec/String.
    allocator::init_heap();

    // Инициализируем физический менеджер памяти (bitmap-аллокатор фреймов 4K).
    // Нужно для создания page tables пользовательских процессов.
    pmm::init();

    // Инициализируем пейджинг — запоминаем HHDM offset для доступа к page tables.
    // map_page/unmap_page используют HHDM для модификации таблиц страниц.
    paging::init();

    // Монтируем initrd (встроенный образ ФС) в VFS.
    // После этого shell может загружать user-программы из /bin.
    fs_initrd::mount();

    // Инициализируем ATA PIO драйвер — проверяем наличие дисков.
    // Primary (0x1F0) = CD-ROM (boot), Secondary (0x170) = btrfs-диск.
    // btrfs::mount() читает суперблок с secondary-канала.
    // Код выделен бирюзовым цветом (правило .kodaruls п.6).
    let _ata_ok = ata::init();
    let _btrfs_ok = btrfs::mount();

    println!("=== KERNEL START ===");
    
    // Запрашиваем framebuffer от Limine.
    // cfg(feature = "fb"): запрос включён только при сборке с фичей fb.
    // При --no-default-features (режим Bochs term) framebuffer не
    // запрашивается — Limine не переключается в графический режим,
    // и ядро работает в текстовом VGA 80x25.
    #[cfg(feature = "fb")]
    {
        let fb_response = FRAMEBUFFER_REQUEST.response();
        if let Some(response) = fb_response {
            let framebuffers = response.framebuffers();
            if !framebuffers.is_empty() {
                let fb = framebuffers[0];
                let width = fb.width;
                let height = fb.height;
                println!("Framebuffer found: {}x{}", width, height);
                // Передаём framebuffer в vesa-подсистему (переиспользуем шрифт
                // и функции рисования) и очищаем экран чёрным.
                // fb.address() — ФИЗИЧЕСКИЙ адрес; Limine не отображает его
                // в higher-half сам. Преобразуем через HHDM offset:
                //   виртуальный = физический + hhdm_offset
                // Без этого запись по физ. адресу → page fault → triple fault.
                // fb.address() из Limine УЖЕ возвращает виртуальный (higher-half)
                // адрес framebuffer: Limine сам применяет HHDM offset
                // (физический 0xfd000000 → виртуальный 0xffff8000fd000000).
                // Повторно добавлять offset НЕЛЬЗЯ — получится переполнение
                // (0xffff8000fd000000 + 0xffff800000000000 = 0xffff0000fd000000)
                // и запись по несуществующему адресу → page fault → triple fault.
                let fb_virt = fb.address() as *mut u32;
                video::set_framebuffer(
                    fb_virt,
                    fb.pitch as u32,
                    width as u32,
                    height as u32,
                );
                // --- ДИАГНОСТИКА АДРЕСАЦИИ FRAMEBUFFER ---
                // 1. Какой адрес выдал Limine: физический или виртуальный.
                //    Признак HHDM: старшие биты равны 0xffff8000_0000_0000.
                // 2. HHDM offset, чтобы вручную проверить преобразование.
                // 3. Физический адрес: virt - hhdm (если адрес виртуальный).
                //    Он должен совпасть с окном VBE, которое эмулирует Bochs
                //    (bochs.log: "Register memory access handlers:
                //     0x0000e0000000 - 0x0000e0ffffff").
                let hhdm = crate::limine_fb::get_hhdm_offset();
                let fb_addr = fb.address() as u64;
                let in_hhdm = (fb_addr & 0xFFFF_8000_0000_0000) == 0xFFFF_8000_0000_0000;
                let fb_phys = if in_hhdm {
                    fb_addr.wrapping_sub(hhdm)
                } else {
                    fb_addr
                };
                println!(
                    "FB limine: addr=0x{:016x} hhdm=0x{:016x}",
                    fb_addr, hhdm
                );
                println!(
                    "FB classify: hhdm_range={} phys=0x{:016x}",
                    in_hhdm, fb_phys
                );
                println!(
                    "FB mode: {}x{} pitch={} bpp={} model={} masks={}/{}/{}",
                    width,
                    height,
                    fb.pitch,
                    fb.bpp,
                    fb.memory_model,
                    fb.red_mask_size,
                    fb.green_mask_size,
                    fb.blue_mask_size
                );
                let (vaddr, vw, vh, vpitch) = video::fb_debug_state();
                println!(
                    "FB video:  addr=0x{:016x} {}x{} pitch={}",
                    vaddr, vw, vh, vpitch
                );
                video::clear_screen(0x000000);
                // --- READBACK В ЧЕТЫРЁХ УГЛАХ ---
                // Чтение до записи проверяет, что весь LFB замаплен:
                // если последняя страница не замаплена, будет #PF с CR2.
                unsafe {
                    let pitch_px = (fb.pitch / 4) as usize;
                    let w = width as usize;
                    let h = height as usize;
                    let corners = [
                        ("TL", 0usize),
                        ("TR", w - 1),
                        ("BL", (h - 1) * pitch_px),
                        ("BR", (h - 1) * pitch_px + w - 1),
                    ];
                    for (name, off) in corners.iter() {
                        let v = core::ptr::read_volatile(fb_virt.add(*off));
                        println!(
                            "FB read {}: off=0x{:08x} val=0x{:08x}",
                            name,
                            off * 4,
                            v
                        );
                    }
                    let vals = [0x00112233u32, 0x00445566, 0x00778899, 0x00AABBCC];
                    for (i, (_, off)) in corners.iter().enumerate() {
                        core::ptr::write_volatile(fb_virt.add(*off), vals[i]);
                    }
                    for (i, (name, off)) in corners.iter().enumerate() {
                        let v = core::ptr::read_volatile(fb_virt.add(*off));
                        println!(
                            "FB rb {}: wrote=0x{:08x} read=0x{:08x} {}",
                            name,
                            vals[i],
                            v,
                            if v == vals[i] { "OK" } else { "MISMATCH" }
                        );
                    }
                }
                println!("Framebuffer output enabled");
            } else {
                println!("No framebuffer available");
            }
        } else {
            println!("No Limine framebuffer response");
        }
    }
    #[cfg(not(feature = "fb"))]
    {
        // Режим без framebuffer (Bochs term): выводим в текстовый VGA.
        // Вся работа ведётся через terminal (VGA 0xB8000 + serial COM1).
        println!("Framebuffer disabled (no-fb mode, text VGA only)");
    }
    
    // Инициализируем GDT
    gdt::init_gdt();
    
    interrupts::init_idt();
    println!("Interrupts enabled");
    
    // Добавляем задачи
    add_task("Hello", task_hello);
    add_task("Counter", task_counter);
    
    println!("=== KERNEL READY ===");
    #[cfg(feature = "fb")]
    {
        // Считаем пиксели цвета текста по всему экрану: если терминал
        // действительно дорисовывает кадр, их будут десятки тысяч.
        let (fb_w, fb_h) = (1024usize, 768usize);
        let mut text_px = 0usize;
        for i in 0..(fb_w * fb_h) {
            if video::fb_read_px(i) == 0x00AAAAAA {
                text_px += 1;
            }
        }
        let (flushes, draws) = crate::terminal::render_counters();
        println!(
            "FB check: text_px={} flush={} draw_fb={}",
            text_px, flushes, draws
        );
    }
    println!("Commands: help, echo, clear, reboot, hlt");
    
    // Запускаем init.elf — первый пользовательский процесс (Ring 3).
    // Init проверяет ФС и завершается, передавая управление shell.
    println!("Loading /bin/init.elf...");
    match crate::process::load_program_from_fs("/bin/init.elf") {
        Ok(proc) => {
            println!("Init loaded: entry=0x{:016x}", proc.entry_point);
            crate::process::run_process(&proc);
            // Сюда не вернёмся — run_process noreturn,
            // возврат идёт через kernel_return_to_shell.
        }
        Err(e) => {
            println!("Failed to load init.elf: {}", e);
        }
    }
    
    // Запускаем shell (читает клавиатуру через IRQ1).
    // run_scheduler() вызывается из команды "tasks" — по одной итерации,
    // чтобы shell оставался отзывчивым. Бесконечный цикл планировщика
    // убран: задачи и shell живут в одном потоке кооперативно.
    shell::shell();
    
    loop {}
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("PANIC: {}", info.message());
    // Вместо зависания — восстанавливаемся в shell через kernel_panic.
    // kernel_panic восстановит CR3, переключит стек и вызовет shell().
    crate::gdt::kernel_panic("Rust panic — recovering to kernel shell");
}
