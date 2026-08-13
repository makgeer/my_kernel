#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

mod vga;
mod interrupts;
mod scheduler;
mod tasks;
mod limine_fb;
mod serial;
mod gdt;
mod vesa;
mod shell;
mod terminal;

use core::panic::PanicInfo;
use crate::scheduler::{add_task};
use crate::tasks::{task_hello, task_counter};
use crate::limine_fb::FRAMEBUFFER_REQUEST;

#[no_mangle]
pub extern "C" fn _start_rust() -> ! {
    // Инициализируем serial (COM1) до первого вывода — чтобы весь лог
    // ядра попадал в последовательный порт (qemu.log)
    serial::init();

    println!("=== KERNEL START ===");
    
    // Запрашиваем framebuffer от Limine
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
            vesa::vesa_set_framebuffer(
                fb_virt,
                fb.pitch as u32,
                width as u32,
                height as u32,
            );
            vesa::vesa_clear_screen(0x000000);
            println!("Framebuffer output enabled");
        } else {
            println!("No framebuffer available");
        }
    } else {
        println!("No Limine framebuffer response");
    }
    
    // Инициализируем GDT
    gdt::init_gdt();
    
    interrupts::init_idt();
    println!("Interrupts enabled");
    
    // Добавляем задачи
    add_task("Hello", task_hello);
    add_task("Counter", task_counter);
    
    println!("=== KERNEL READY ===");
    println!("Commands: help, echo, clear, reboot, hlt");
    
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
    loop {
        unsafe { core::arch::asm!("hlt"); }
    }
}
