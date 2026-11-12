//! Физический менеджер памяти (Physical Memory Manager).
//!
//! Bitmap-аллокатор физических страниц 4K. Отслеживает свободные/занятые
//! физические фреймы на основе карты памяти, полученной от Limine через
//! MemmapRequest.
//!
//! Это фундамент для пейджинга: новые page tables выделяются через
//! alloc_frame(), а освобождаются через dealloc_frame().
//!
//! Лимит: статический bitmap 128 KB → поддерживает до 4 GB физической памяти.
//! При необходимости можно увеличить BITMAP_SIZE или перейти на динамический
//! bitmap, размещаемый в первом usable-регионе.

use limine::request::MemmapRequest;
use limine::memmap;
use spin::Mutex;
use crate::println;

/// Запрос карты памяти у Limine. Размещается в секции .requests —
/// Limine сканирует её и заполняет ответ до передачи управления ядру.
#[used]
#[link_section = ".requests"]
pub static MEMMAP_REQUEST: MemmapRequest = MemmapRequest::new();

/// Размер физической страницы (4 KiB).
pub const PAGE_SIZE: usize = 4096;

/// Размер bitmap в байтах. 128 KB = 1M бит = 1M фреймов × 4K = 4 GB максимум.
pub const BITMAP_SIZE: usize = 128 * 1024;

/// Bitmap: 1 бит на физический фрейм.
/// 1 = занят, 0 = свободен.
/// Инициализируется всеми единицами (всё занято), затем usable-регионы
/// из карты Limine помечаются нулями (свободны).
static mut BITMAP: [u8; BITMAP_SIZE] = [0xFF; BITMAP_SIZE];

/// Spinlock для защиты bitmap от конкурентного доступа (прерывания).
static BITMAP_LOCK: Mutex<()> = Mutex::new(());

/// Общее количество фреймов, известных PMM (по карте Limine).
/// Фреймы с индексом >= TOTAL_FRAMES считаются несуществующими.
static mut TOTAL_FRAMES: usize = 0;

/// Количество свободных фреймов (для статистики).
static mut FREE_FRAMES: usize = 0;

/// Преобразует номер фрейма в (индекс байта, битовая маска).
#[inline]
fn frame_to_bit(frame: usize) -> (usize, u8) {
    (frame >> 3, 1 << (frame & 7))
}

/// Проверяет, занят ли фрейм. Не потокобезопасно — вызывать под локом.
#[inline]
unsafe fn is_frame_used(frame: usize) -> bool {
    let (byte_idx, bit) = frame_to_bit(frame);
    if byte_idx >= BITMAP_SIZE {
        return true; // За пределами bitmap — считаем занятым
    }
    // Используем raw pointer, чтобы избежать static_mut_refs warning
    // Используем addr_of! чтобы избежать static_mut_refs warning
    let ptr = core::ptr::addr_of!(BITMAP) as *const u8;
    *ptr.add(byte_idx) & bit != 0
}

/// Устанавливает состояние фрейма (true = занят, false = свободен).
/// Не потокобезопасно — вызывать под локом.
#[inline]
unsafe fn set_frame(frame: usize, used: bool) {
    let (byte_idx, bit) = frame_to_bit(frame);
    if byte_idx >= BITMAP_SIZE {
        return; // За пределами bitmap — игнорируем
    }
    // Используем addr_of_mut! чтобы избежать static_mut_refs warning
    let ptr = core::ptr::addr_of_mut!(BITMAP) as *mut u8;
    if used {
        *ptr.add(byte_idx) |= bit;
    } else {
        *ptr.add(byte_idx) &= !bit;
    }
}

/// Инициализация PMM: разбирает карту памяти от Limine, строит bitmap.
///
/// Вызывать ОДИН раз в _start_rust, до первого alloc_frame().
pub fn init() {
    let _lock = BITMAP_LOCK.lock();

    // По умолчанию все фреймы помечены как занятые (0xFF в определении).
    // Теперь размечаем свободные регионы из карты Limine.

    if let Some(response) = MEMMAP_REQUEST.response() {
        let entries = response.entries();

        // Первый проход: находим максимальный физический адрес среди usable-регионов
        let mut max_addr: u64 = 0;
        for entry in entries {
            if entry.type_ == memmap::MEMMAP_USABLE {
                let end = entry.base + entry.length;
                if end > max_addr {
                    max_addr = end;
                }
            }
        }

        let total_frames = if max_addr > 0 {
            (max_addr as usize).div_ceil(PAGE_SIZE)
        } else {
            0
        };

        // Ограничиваем размером bitmap
        let total_frames = total_frames.min(BITMAP_SIZE * 8);

        unsafe {
            TOTAL_FRAMES = total_frames;
        }

        // Второй проход: помечаем usable-регионы как свободные
        let mut free_count: usize = 0;
        for entry in entries {
            if entry.type_ == memmap::MEMMAP_USABLE {
                let start_frame = (entry.base as usize) / PAGE_SIZE;
                // Округляем длину вниз до целых фреймов
                let frame_count = (entry.length as usize) / PAGE_SIZE;

                for i in 0..frame_count {
                    let frame = start_frame + i;
                    if frame < total_frames {
                        unsafe {
                            if is_frame_used(frame) {
                                set_frame(frame, false);
                                free_count += 1;
                            }
                        }
                    }
                }
            }
        }

        unsafe {
            FREE_FRAMES = free_count;
        }

        let total_mb = total_frames * PAGE_SIZE / (1024 * 1024);
        let free_mb = free_count * PAGE_SIZE / (1024 * 1024);
        println!(
            "PMM initialized: {} frames total ({} MB), {} free ({} MB)",
            total_frames, total_mb, free_count, free_mb
        );
    } else {
        println!("PMM: No memory map response from Limine!");
    }
}

/// Выделяет один физический фрейм 4K.
/// Возвращает физический адрес (без HHDM offset) или None, если память закончилась.
///
/// Линейный поиск по bitmap — простой, но достаточный для учебного ядра.
/// При высокой нагрузке можно заменить на buddy allocator или free list.
pub fn alloc_frame() -> Option<u64> {
    let _lock = BITMAP_LOCK.lock();

    let total = unsafe { TOTAL_FRAMES };
    for frame in 0..total {
        unsafe {
            if !is_frame_used(frame) {
                set_frame(frame, true);
                FREE_FRAMES -= 1;
                return Some((frame * PAGE_SIZE) as u64);
            }
        }
    }
    None
}

/// Освобождает физический фрейм.
/// addr должен быть выровнен на 4K и ранее получен через alloc_frame().
pub fn dealloc_frame(addr: u64) {
    let _lock = BITMAP_LOCK.lock();

    let frame = (addr as usize) / PAGE_SIZE;
    unsafe {
        if frame < TOTAL_FRAMES {
            if is_frame_used(frame) {
                set_frame(frame, false);
                FREE_FRAMES += 1;
            }
        }
    }
}

/// Возвращает количество свободных фреймов (для диагностики).
pub fn free_count() -> usize {
    unsafe { FREE_FRAMES }
}
