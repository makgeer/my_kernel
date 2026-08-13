// Limine Framebuffer Protocol
// Получаем framebuffer напрямую из Limine Bootloader

use limine::request::{FramebufferRequest, HhdmRequest};

// Возвращены атрибуты #[used] и #[link_section = ".requests"]:
// Limine сканирует секцию .requests и предоставляет ядру framebuffer.
// (Изначально отключались для текстового режима варианта A — VGA-буфер.
//  Теперь реализуется вывод на framebuffer — атрибуты снова нужны.)
#[used]
#[link_section = ".requests"]
pub static FRAMEBUFFER_REQUEST: FramebufferRequest = FramebufferRequest::new();

// HHDM (Higher Half Direct Map): Limine отображает всю физическую память
// в higher-half со смещением offset. Это нужно, чтобы преобразовать
// ФИЗИЧЕСКИЙ адрес framebuffer (fb.address()) в ВИРТУАЛЬНЫЙ, по которому
// ядро может писать: virt = phys + hhdm_offset.
#[used]
#[link_section = ".requests"]
pub static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

/// Смещение direct map: физ. адрес + offset = вирт. адрес
pub fn get_hhdm_offset() -> u64 {
    HHDM_REQUEST.response().map(|r| r.offset).unwrap_or(0)
}

/// Получаем framebuffer от Limine
#[allow(dead_code)]
pub fn get_framebuffer() -> Option<(*mut u32, u64, u64, u64)> {
    let response = FRAMEBUFFER_REQUEST.response()?;
    let framebuffers = response.framebuffers();
    if framebuffers.is_empty() {
        return None;
    }
    
    let fb = framebuffers[0];
    // address() возвращает *mut (), приводим к *mut u32
    let address = fb.address() as *mut u32;
    let width = fb.width as u64;
    let height = fb.height as u64;
    let pitch = fb.pitch as u64;
    
    Some((address, width, height, pitch))
}