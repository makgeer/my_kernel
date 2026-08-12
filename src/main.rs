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

use core::panic::PanicInfo;
use crate::scheduler::{add_task, run_scheduler};
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
    
    // Запускаем планировщик
    run_scheduler();
    
    loop {}
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("PANIC: {}", info.message());
    loop {
        unsafe { core::arch::asm!("hlt"); }
    }
}
