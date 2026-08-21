//! Пейджинг — управление таблицами страниц x86_64 (4-level paging).
//!
//! Реализует map_page / unmap_page / translate для маппинга виртуальных
//! страниц 4K на физические фреймы. Использует HHDM (Higher-Half Direct Map)
//! от Limine для доступа к page tables по их физическому адресу.
//!
//! Иерархия: P4 → P3 → P2 → P1 → физический фрейм 4K
//! Каждая таблица: 512 записей по 8 байт = 4K (одна страница).
//! Виртуальный адрес (48 бит):
//!   биты 47-39 → индекс P4
//!   биты 38-30 → индекс P3
//!   биты 29-21 → индекс P2
//!   биты 20-12 → индекс P1
//!   биты 11-0  → смещение в странице
//!
//! Это фундамент для изоляции адресных пространств пользовательских
//! процессов: каждый процесс получит собственную P4 таблицу.

use x86_64::structures::paging::page_table::{PageTable, PageTableFlags};
use x86_64::{PhysAddr, VirtAddr};
use x86_64::registers::control::Cr3;
use spin::Mutex;
use crate::pmm;
use crate::limine_fb::get_hhdm_offset;
use crate::println;

/// HHDM offset — преобразует физический адрес в виртуальный.
/// Устанавливается один раз при init(). Limine мапит всю физическую
/// память по адресу `phys + hhdm_offset`, что позволяет ядру обращаться
/// к page tables (которые хранятся по физическим адресам).
static mut HHDM_OFFSET: u64 = 0;

/// Физический адрес kernel P4 таблицы (сохраняется при init()).
/// Используется для восстановления CR3 после возврата из user mode.
static mut KERNEL_P4_PHYS: u64 = 0;

/// Spinlock для защиты операций с page tables от конкурентного доступа
/// (прерывания могут вызвать map/unmap в обработчике).
static PAGING_LOCK: Mutex<()> = Mutex::new(());

/// Инициализация пейджинга: запоминаем HHDM offset от Limine.
///
/// Вызывать ОДИН раз в _start_rust, после pmm::init().
pub fn init() {
    let offset = get_hhdm_offset();
    let (p4_frame, _) = Cr3::read();
    let p4_phys = p4_frame.start_address().as_u64();
    unsafe {
        HHDM_OFFSET = offset;
        KERNEL_P4_PHYS = p4_phys;
    }

    println!(
        "Paging initialized: P4 at phys 0x{:016x}, HHDM offset 0x{:016x}",
        p4_phys,
        offset
    );
}

/// Преобразует физический адрес page table в виртуальный указатель через HHDM.
/// `phys` — физический адрес таблицы (выровнен на 4K).
/// Возвращает `*mut PageTable` — можно разыменовать в unsafe-блоке.
#[inline]
unsafe fn phys_to_virt(phys: u64) -> *mut PageTable {
    (phys + HHDM_OFFSET) as *mut PageTable
}

/// Вычисляет индексы P4/P3/P2/P1 из виртуального адреса.
#[inline]
fn virt_to_indices(virt: u64) -> (usize, usize, usize, usize) {
    let p4 = ((virt >> 39) & 0x1FF) as usize;
    let p3 = ((virt >> 30) & 0x1FF) as usize;
    let p2 = ((virt >> 21) & 0x1FF) as usize;
    let p1 = ((virt >> 12) & 0x1FF) as usize;
    (p4, p3, p2, p1)
}

/// Мапит виртуальную страницу 4K на физический фрейм.
///
/// # Аргументы
/// - `virt` — виртуальный адрес (должен быть выровнен на 4K)
/// - `phys` — физический адрес фрейма (должен быть выровнен на 4K)
/// - `flags` — флаги страницы (PRESENT | WRITABLE | USER_ACCESSIBLE | ...)
///
/// # Возвращает
/// - `Ok(())` при успехе
/// - `Err(&'static str)` при ошибке (нет памяти для новой таблицы)
///
/// # Безопасность
/// Создаёт новые P3/P2/P1 таблицы по мере необходимости, выделяя фреймы
/// через PMM. Если страница уже замаплена — перезаписывает запись.
pub fn map_page(virt: u64, phys: u64, flags: PageTableFlags) -> Result<(), &'static str> {
    let _lock = PAGING_LOCK.lock();

    let (p4_idx, p3_idx, p2_idx, p1_idx) = virt_to_indices(virt);

    // Получаем физический адрес активной P4 таблицы из CR3
    let (p4_frame, _) = Cr3::read();
    let p4_phys = p4_frame.start_address().as_u64();

    unsafe {
        let p4: &mut PageTable = &mut *phys_to_virt(p4_phys);

        // --- P3 таблица ---
        let p3_entry = &mut p4[p4_idx];
        let p3_phys;
        if !p3_entry.flags().contains(PageTableFlags::PRESENT) {
            // Выделяем новый фрейм для P3 таблицы
            let new_frame = pmm::alloc_frame().ok_or("PMM: out of memory for P3 table")?;
            p3_phys = new_frame;
            // Очищаем новую таблицу (все записи = 0)
            (*phys_to_virt(new_frame)).zero();
            // P4 → P3: PRESENT | WRITABLE (таблицы должны быть writable,
            // иначе CPU не сможет изменять записи в нижних уровнях)
            p3_entry.set_addr(
                PhysAddr::new(new_frame),
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
            );
        } else {
            p3_phys = p3_entry.addr().as_u64();
        }

        // --- P2 таблица ---
        let p3: &mut PageTable = &mut *phys_to_virt(p3_phys);
        let p2_entry = &mut p3[p3_idx];
        let p2_phys;
        if !p2_entry.flags().contains(PageTableFlags::PRESENT) {
            let new_frame = pmm::alloc_frame().ok_or("PMM: out of memory for P2 table")?;
            p2_phys = new_frame;
            (*phys_to_virt(new_frame)).zero();
            p2_entry.set_addr(
                PhysAddr::new(new_frame),
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
            );
        } else {
            p2_phys = p2_entry.addr().as_u64();
        }

        // --- P1 таблица ---
        let p2: &mut PageTable = &mut *phys_to_virt(p2_phys);
        let p1_entry = &mut p2[p2_idx];
        let p1_phys;
        if !p1_entry.flags().contains(PageTableFlags::PRESENT) {
            let new_frame = pmm::alloc_frame().ok_or("PMM: out of memory for P1 table")?;
            p1_phys = new_frame;
            (*phys_to_virt(new_frame)).zero();
            p1_entry.set_addr(
                PhysAddr::new(new_frame),
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
            );
        } else {
            p1_phys = p1_entry.addr().as_u64();
        }

        // --- Запись P1: мапим виртуальную страницу на физический фрейм ---
        let p1: &mut PageTable = &mut *phys_to_virt(p1_phys);
        p1[p1_idx].set_addr(PhysAddr::new(phys), flags);
    }

    // Инвалидируем TLB для этой страницы — иначе CPU может использовать
    // устаревший кэш и писать не туда.
    x86_64::instructions::tlb::flush(VirtAddr::new(virt));

    Ok(())
}

/// Снимает маппинг виртуальной страницы.
/// Запись P1 очищается (set_unused), но сами P1/P2/P3 таблицы не освобождаются
/// (упрощение — для учебного ядра допустимо; при большом количестве процессов
/// можно добавить reclaim пустых таблиц).
pub fn unmap_page(virt: u64) -> Result<(), &'static str> {
    let _lock = PAGING_LOCK.lock();

    let (p4_idx, p3_idx, p2_idx, p1_idx) = virt_to_indices(virt);

    let (p4_frame, _) = Cr3::read();
    let p4_phys = p4_frame.start_address().as_u64();

    unsafe {
        let p4: &mut PageTable = &mut *phys_to_virt(p4_phys);

        let p3_entry = &p4[p4_idx];
        if !p3_entry.flags().contains(PageTableFlags::PRESENT) {
            return Err("unmap: P3 table not present");
        }
        let p3_phys = p3_entry.addr().as_u64();

        let p3: &mut PageTable = &mut *phys_to_virt(p3_phys);
        let p2_entry = &p3[p3_idx];
        if !p2_entry.flags().contains(PageTableFlags::PRESENT) {
            return Err("unmap: P2 table not present");
        }
        let p2_phys = p2_entry.addr().as_u64();

        let p2: &mut PageTable = &mut *phys_to_virt(p2_phys);
        let p1_entry = &p2[p2_idx];
        if !p1_entry.flags().contains(PageTableFlags::PRESENT) {
            return Err("unmap: P1 table not present");
        }
        let p1_phys = p1_entry.addr().as_u64();

        // Очищаем запись P1 — страница больше не замаплена
        let p1: &mut PageTable = &mut *phys_to_virt(p1_phys);
        p1[p1_idx].set_unused();
    }

    x86_64::instructions::tlb::flush(VirtAddr::new(virt));

    Ok(())
}

/// Переводит виртуальный адрес в физический (translate).
/// Возвращает None, если страница не замаплена (на любом уровне иерархии).
pub fn translate(virt: u64) -> Option<u64> {
    let (p4_frame, _) = Cr3::read();
    let p4_phys = p4_frame.start_address().as_u64();
    translate_in_p4(p4_phys, virt)
}

/// Переводит виртуальный адрес в физический в указанной P4 таблице.
/// Используется для проверки маппингов в неактивной P4 (например, при
/// создании нового процесса до переключения CR3).
pub fn translate_in_p4(p4_phys: u64, virt: u64) -> Option<u64> {
    let (p4_idx, p3_idx, p2_idx, p1_idx) = virt_to_indices(virt);
    let offset = virt & 0xFFF; // смещение внутри страницы

    unsafe {
        let p4: &PageTable = &*phys_to_virt(p4_phys);

        let p3_entry = &p4[p4_idx];
        if !p3_entry.flags().contains(PageTableFlags::PRESENT) {
            return None;
        }
        let p3_phys = p3_entry.addr().as_u64();

        let p3: &PageTable = &*phys_to_virt(p3_phys);
        let p2_entry = &p3[p3_idx];
        if !p2_entry.flags().contains(PageTableFlags::PRESENT) {
            return None;
        }
        let p2_phys = p2_entry.addr().as_u64();

        let p2: &PageTable = &*phys_to_virt(p2_phys);
        let p1_entry = &p2[p2_idx];
        if !p1_entry.flags().contains(PageTableFlags::PRESENT) {
            return None;
        }
        let p1_phys = p1_entry.addr().as_u64();

        let p1: &PageTable = &*phys_to_virt(p1_phys);
        let entry = &p1[p1_idx];
        if !entry.flags().contains(PageTableFlags::PRESENT) {
            return None;
        }

        // Физический адрес фрейма + смещение внутри страницы
        Some(entry.addr().as_u64() | offset)
    }
}

/// Диагностика: проверяет маппинг виртуального адреса в УКАЗАННОЙ P4
/// таблице (не обязательно активной, без переключения CR3).
///
/// Возвращает Some(физический адрес страницы) если страница замаплена,
/// None если хотя бы на одном уровне иерархии нет PRESENT.
/// Используется для отладки перед запуском процесса в ring 3.
pub fn check_mapping_in_p4(p4_phys: u64, virt: u64) -> Option<u64> {
    let (p4_idx, p3_idx, p2_idx, p1_idx) = virt_to_indices(virt);

    unsafe {
        let p4: &PageTable = &*phys_to_virt(p4_phys);
        let p3_entry = &p4[p4_idx];
        if !p3_entry.flags().contains(PageTableFlags::PRESENT) {
            return None;
        }
        let p3: &PageTable = &*phys_to_virt(p3_entry.addr().as_u64());
        let p2_entry = &p3[p3_idx];
        if !p2_entry.flags().contains(PageTableFlags::PRESENT) {
            return None;
        }
        let p2: &PageTable = &*phys_to_virt(p2_entry.addr().as_u64());
        let p1_entry = &p2[p2_idx];
        if !p1_entry.flags().contains(PageTableFlags::PRESENT) {
            return None;
        }
        let p1: &PageTable = &*phys_to_virt(p1_entry.addr().as_u64());
        let p1e = &p1[p1_idx];
        if !p1e.flags().contains(PageTableFlags::PRESENT) {
            return None;
        }
        Some(p1e.addr().as_u64())
    }
}

// ---------------------------------------------------------------------------
// НОВЫЙ КОД (бирюзовый, правило .kodaruls п.6): безопасные системные вызовы.
// ---------------------------------------------------------------------------

/// Возвращает флаги записи P1 для виртуального адреса в активной P4 таблице
/// (та, на которую указывает CR3). None — если хотя бы один уровень
/// иерархии отсутствует (страница не замаплена).
///
/// Используется системными вызовами (syscall_handler_rust) для проверки
/// аргументов из user space: достаточно проверить флаги USER_ACCESSIBLE
/// и (для буферов на запись) WRITABLE на уровне P1 — флаги на верхних
/// уровнях иерархии для user-страниц выставляются одинаковыми в
/// map_page_in_p4 (PRESENT | WRITABLE | USER_ACCESSIBLE).
pub fn flags_at(virt: u64) -> Option<PageTableFlags> {
    let (p4_frame, _) = Cr3::read();
    flags_in_p4(p4_frame.start_address().as_u64(), virt)
}

/// Возвращает флаги записи P1 для виртуального адреса в УКАЗАННОЙ P4
/// таблице (не обязательно активной). None — если страница не замаплена.
pub fn flags_in_p4(p4_phys: u64, virt: u64) -> Option<PageTableFlags> {
    let (p4_idx, p3_idx, p2_idx, p1_idx) = virt_to_indices(virt);

    unsafe {
        let p4: &PageTable = &*phys_to_virt(p4_phys);
        let p3_entry = &p4[p4_idx];
        if !p3_entry.flags().contains(PageTableFlags::PRESENT) {
            return None;
        }
        let p3: &PageTable = &*phys_to_virt(p3_entry.addr().as_u64());
        let p2_entry = &p3[p3_idx];
        if !p2_entry.flags().contains(PageTableFlags::PRESENT) {
            return None;
        }
        let p2: &PageTable = &*phys_to_virt(p2_entry.addr().as_u64());
        let p1_entry = &p2[p2_idx];
        if !p1_entry.flags().contains(PageTableFlags::PRESENT) {
            return None;
        }
        let p1: &PageTable = &*phys_to_virt(p1_entry.addr().as_u64());
        Some(p1[p1_idx].flags())
    }
}

/// Мапит виртуальную страницу 4K в указанной P4 таблице (не обязательно активной).
///
/// Используется при создании нового процесса: страницы мапятся в новой P4
/// до переключения CR3. Флаги таблиц (P4→P3→P2→P1) всегда включают
/// USER_ACCESSIBLE, чтобы пользовательский код мог обращаться к ним.
pub fn map_page_in_p4(
    p4_phys: u64,
    virt: u64,
    phys: u64,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    let _lock = PAGING_LOCK.lock();

    let (p4_idx, p3_idx, p2_idx, p1_idx) = virt_to_indices(virt);

    // Флаги для таблиц: PRESENT | WRITABLE | USER_ACCESSIBLE
    // (user-доступные, чтобы ring 3 мог читать/писать через эти таблицы)
    let table_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;

    unsafe {
        let p4: &mut PageTable = &mut *phys_to_virt(p4_phys);

        // --- P3 таблица ---
        let p3_entry = &mut p4[p4_idx];
        let p3_phys;
        if !p3_entry.flags().contains(PageTableFlags::PRESENT) {
            let new_frame = pmm::alloc_frame().ok_or("PMM: out of memory for P3 table")?;
            p3_phys = new_frame;
            (*phys_to_virt(new_frame)).zero();
            p3_entry.set_addr(PhysAddr::new(new_frame), table_flags);
        } else {
            p3_phys = p3_entry.addr().as_u64();
        }

        // --- P2 таблица ---
        let p3: &mut PageTable = &mut *phys_to_virt(p3_phys);
        let p2_entry = &mut p3[p3_idx];
        let p2_phys;
        if !p2_entry.flags().contains(PageTableFlags::PRESENT) {
            let new_frame = pmm::alloc_frame().ok_or("PMM: out of memory for P2 table")?;
            p2_phys = new_frame;
            (*phys_to_virt(new_frame)).zero();
            p2_entry.set_addr(PhysAddr::new(new_frame), table_flags);
        } else {
            p2_phys = p2_entry.addr().as_u64();
        }

        // --- P1 таблица ---
        let p2: &mut PageTable = &mut *phys_to_virt(p2_phys);
        let p1_entry = &mut p2[p2_idx];
        let p1_phys;
        if !p1_entry.flags().contains(PageTableFlags::PRESENT) {
            let new_frame = pmm::alloc_frame().ok_or("PMM: out of memory for P1 table")?;
            p1_phys = new_frame;
            (*phys_to_virt(new_frame)).zero();
            p1_entry.set_addr(PhysAddr::new(new_frame), table_flags);
        } else {
            p1_phys = p1_entry.addr().as_u64();
        }

        // --- Запись P1 ---
        let p1: &mut PageTable = &mut *phys_to_virt(p1_phys);
        p1[p1_idx].set_addr(PhysAddr::new(phys), flags);
    }

    // TLB flush не нужен — эта P4 не активна (CR3 не указывает на неё).
    // Flush потребуется после переключения CR3.

    Ok(())
}

/// Создаёт новую P4 таблицу для пользовательского процесса.
///
/// Копирует entries 256-511 (higher-half) из текущей (kernel) P4:
/// ядро (0xFFFFFFFF80000000), HHDM, framebuffer. Эти записи указывают
/// на существующие P3 таблицы ядра — они shared.
///
/// Entries 0-255 (lower-half) НЕ копируются — это пользовательское
/// адресное пространство. Это критически важно:
/// 1. User-код/стек мапятся в ЧИСТЫЕ P3/P2/P1 таблицы с флагом
///    USER_ACCESSIBLE. Если скопировать kernel P4[0] (где есть VGA,
///    identity-map и т.д.), то user-маппинги создаются в kernel P3/P2
///    таблицах БЕЗ USER_ACCESSIBLE на верхних уровнях → ring 3 не
///    может пройти по иерархии → #PF при первом же fetch кода.
/// 2. Изоляция: kernel-страницы lower-half (VGA, DMA) не видны user.
///
/// Исключение: VGA text buffer (0xB8000) мапится в user P4 отдельно
/// через map_page_in_p4 с kernel-флагами (U/S=0) — он нужен
/// обработчикам прерываний (println! из #PF/#DF) при активной user CR3,
/// но ring 3 получить к нему доступ не сможет (U/S проверяется на P1).
///
/// Возвращает физический адрес новой P4 или ошибку.
pub fn create_user_p4() -> Result<u64, &'static str> {
    let _lock = PAGING_LOCK.lock();

    // Выделяем фрейм для новой P4
    let new_p4_phys = pmm::alloc_frame().ok_or("PMM: out of memory for user P4")?;

    unsafe {
        // Очищаем новую P4
        (*phys_to_virt(new_p4_phys)).zero();

        // Получаем текущую P4
        let (p4_frame, _) = Cr3::read();
        let kernel_p4_phys = p4_frame.start_address().as_u64();
        let kernel_p4: &PageTable = &*phys_to_virt(kernel_p4_phys);
        let new_p4: &mut PageTable = &mut *phys_to_virt(new_p4_phys);

        // Копируем ТОЛЬКО entries 256-511 (higher-half: ядро + HHDM +
        // framebuffer). Все записи указывают на существующие P3 таблицы
        // ядра — они shared.
        // Entries 0-255 (lower-half) остаются нулевыми — user-код мапится
        // в собственные P3/P2/P1 с USER_ACCESSIBLE (см. doc-комментарий).
        for i in 256..512 {
            new_p4[i] = kernel_p4[i].clone();
        }
    }

    Ok(new_p4_phys)
}

/// Возвращает HHDM offset (публичный доступ для других модулей).
pub fn hhdm_offset() -> u64 {
    unsafe { HHDM_OFFSET }
}

/// Возвращает физический адрес kernel P4 таблицы.
/// Используется syscall_handler для восстановления CR3 после возврата
/// из user mode (ring 3 → ring 0).
pub fn kernel_p4_phys() -> u64 {
    unsafe { KERNEL_P4_PHYS }
}

/// Копирует P4 entry из user P4 в kernel P4.
///
/// Нужно для двухэтапного перехода в ring 3: второй iretq в user_mode_entry
/// читает IRET-фрейм со стека и fetch-ит инструкцию по RIP с kernel CR3
/// (до переключения на user P4). Без этого маппинга instruction fetch
/// по user-адресу → #PF → DOUBLE FAULT. После iretq CPU переключит CR3
/// на user P4, и будут использованы user-таблицы.
///
/// Копируем ТОЛЬКО P4 entry — P3/P2/P1 таблицы остаются shared между
/// kernel и user P4. Это avoids лишних allocations PMM и не ломает
/// user маппинги (раньше функция модифицировала user P3/P2/P1 на месте,
/// что было багом).
pub fn copy_user_mapping_to_kernel(virt: u64, p4_user: u64, _flags: PageTableFlags) -> Result<(), &'static str> {
    let _lock = PAGING_LOCK.lock();

    let (p4_idx, ..) = virt_to_indices(virt);

    // Читаем текущую kernel P4
    let (p4_frame, _) = Cr3::read();
    let kernel_p4_phys = p4_frame.start_address().as_u64();

    unsafe {
        let kernel_p4: &mut PageTable = &mut *phys_to_virt(kernel_p4_phys);
        let user_p4: &PageTable = &*phys_to_virt(p4_user);

        // Проверяем, что user P4 имеет P3 entry для этого адреса
        if !user_p4[p4_idx].flags().contains(PageTableFlags::PRESENT) {
            return Err("copy_user_mapping: user P4 entry not present");
        }

        // Копируем P4 entry из user P4 в kernel P4.
        // P3/P2/P1 таблицы shared — не копируем, не модифицируем.
        kernel_p4[p4_idx].set_addr(
            user_p4[p4_idx].addr(),
            user_p4[p4_idx].flags(),
        );
    }

    x86_64::instructions::tlb::flush(VirtAddr::new(virt));
    Ok(())
}

/// Маппит виртуальную страницу в kernel P4 с флагами PRESENT | USER_ACCESSIBLE.
///
/// Нужно для двухэтапного перехода в ring 3: второй iretq в user_mode_entry
/// читает IRET-фрейм со стека и fetch-ит инструкцию по RIP с kernel CR3
/// (до переключения на user P4). Без этого маппинга instruction fetch
/// по user-адресу → #PF → DOUBLE FAULT. После iretq CPU переключит CR3
/// на user P4, и будут использованы user-таблицы.
///
/// Если user P4 известна — используйте copy_user_mapping_to_kernel (эффективнее).
/// Эта функция создаёт новые P3/P2/P1 таблицы.
#[deprecated(since = "0.1.0", note = "Use copy_user_mapping_to_kernel instead")]
pub fn map_kernel_page(virt: u64, phys: u64, flags: PageTableFlags) -> Result<(), &'static str> {
    let _lock = PAGING_LOCK.lock();
    let (p4_idx, p3_idx, p2_idx, p1_idx) = virt_to_indices(virt);

    let (p4_frame, _) = Cr3::read();
    let p4_phys = p4_frame.start_address().as_u64();

    unsafe {
        let p4: &mut PageTable = &mut *phys_to_virt(p4_phys);

        // --- P3 ---
        let p3_entry = &mut p4[p4_idx];
        let p3_phys;
        if !p3_entry.flags().contains(PageTableFlags::PRESENT) {
            let new_frame = pmm::alloc_frame().ok_or("PMM: OOM for P3 table")?;
            p3_phys = new_frame;
            (*phys_to_virt(new_frame)).zero();
            p3_entry.set_addr(
                PhysAddr::new(new_frame),
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE,
            );
        } else {
            p3_phys = p3_entry.addr().as_u64();
        }

        // --- P2 ---
        let p3: &mut PageTable = &mut *phys_to_virt(p3_phys);
        let p2_entry = &mut p3[p3_idx];
        let p2_phys;
        if !p2_entry.flags().contains(PageTableFlags::PRESENT) {
            let new_frame = pmm::alloc_frame().ok_or("PMM: OOM for P2 table")?;
            p2_phys = new_frame;
            (*phys_to_virt(new_frame)).zero();
            p2_entry.set_addr(
                PhysAddr::new(new_frame),
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE,
            );
        } else {
            p2_phys = p2_entry.addr().as_u64();
        }

        // --- P1 ---
        let p2: &mut PageTable = &mut *phys_to_virt(p2_phys);
        let p1_entry = &mut p2[p2_idx];
        let p1_phys;
        if !p1_entry.flags().contains(PageTableFlags::PRESENT) {
            let new_frame = pmm::alloc_frame().ok_or("PMM: OOM for P1 table")?;
            p1_phys = new_frame;
            (*phys_to_virt(new_frame)).zero();
            p1_entry.set_addr(
                PhysAddr::new(new_frame),
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE,
            );
        } else {
            p1_phys = p1_entry.addr().as_u64();
        }

        // --- Запись P1 ---
        let p1: &mut PageTable = &mut *phys_to_virt(p1_phys);
        p1[p1_idx].set_addr(PhysAddr::new(phys), flags);
    }

    x86_64::instructions::tlb::flush(VirtAddr::new(virt));
    Ok(())
}
