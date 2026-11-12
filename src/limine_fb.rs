// Limine Framebuffer Protocol
// Получаем framebuffer напрямую из Limine Bootloader

use limine::request::HhdmRequest;
#[cfg(feature = "fb")]
use limine::request::FramebufferRequest;

// Возвращены атрибуты #[used] и #[link_section = ".requests"]:
// Limine сканирует секцию .requests и предоставляет ядру framebuffer.
// (Изначально отключались для текстового режима варианта A — VGA-буфер.
//  Теперь реализуется вывод на framebuffer — атрибуты снова нужны.)
//
// cfg(feature = "fb"): запрос framebuffer создаётся ТОЛЬКО при сборке
// с фичей fb (по умолчанию). Сборка --no-default-features отключает
// запрос — Limine не выдаст графический режим, ядро работает в текстовом
// VGA 80x25. Это нужно для Bochs term-дисплея (ncurses), который не
// поддерживает 32bpp graphics mode.
#[cfg(feature = "fb")]
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

/// Получаем framebuffer от Limine.
/// Доступна только при сборке с фичей fb (см. FRAMEBUFFER_REQUEST выше).
#[cfg(feature = "fb")]
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