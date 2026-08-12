// Limine Framebuffer Protocol
// Получаем framebuffer напрямую из Limine Bootloader

use limine::request::FramebufferRequest;

// Временно отключены: без этих атрибутов Limine не видит запрос framebuffer
// и остаётся в текстовом режиме, где VGA-буфер 0xB8000 отображается корректно.
// Код не удалён — атрибуты будут возвращены после реализации вывода на framebuffer.
// #[used]
// #[link_section = ".requests"]
pub static FRAMEBUFFER_REQUEST: FramebufferRequest = FramebufferRequest::new();

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