//! ELF64-загрузчик — парсинг ELF и загрузка PT_LOAD сегментов
//! в адресное пространство пользовательского процесса.
//!
//! Загрузчик принимает сырые байты ELF-файла (например, встроенные через
//! `include_bytes!`), проверяет заголовок, мапит сегменты в P4 процесса
//! через PMM + paging, копирует данные и возвращает точку входа (entry point).
//!
//! Поддерживается только статический ELF64 для x86_64 (ET_EXEC):
//! - Little-endian
//! - Machine = EM_X86_64 (0x3E)
//! - Program header table (PT_LOAD сегменты)
//! - Сегменты с p_memsz > p_filesz заполняются нулями (bss)

use x86_64::structures::paging::page_table::PageTableFlags;
use crate::pmm;
use crate::paging;
use crate::println;

// ── Константы ELF ────────────────────────────────────────────────

/// Магическое число ELF: 0x7F 'E' 'L' 'F'
const ELFMAG: [u8; 4] = [0x7F, b'E', b'L', b'F'];

/// ELFCLASS64 — 64-битный ELF
const ELFCLASS64: u8 = 2;

/// ELFDATA2LSB — little-endian
const ELFDATA2LSB: u8 = 1;

/// ET_EXEC — исполняемый файл (статический, без relocation)
const ET_EXEC: u16 = 2;

/// EM_X86_64 — целевая архитектура x86_64
const EM_X86_64: u16 = 0x3E;

/// PT_LOAD — сегмент, загружаемый в память
const PT_LOAD: u32 = 1;

/// PF_X — сегмент исполняемый
const PF_X: u32 = 1;

/// PF_W — сегмент доступен для записи
const PF_W: u32 = 2;

/// PF_R — сегмент доступен для чтения
const PF_R: u32 = 4;

/// Размер страницы 4K
const PAGE_SIZE: u64 = 4096;

// ── Структуры ELF64 (repr(C) для прямого чтения из сырых байт) ────

/// Заголовок ELF64 (64 байта).
/// Поля соответствуют системному ABI ELF64 на x86_64.
#[repr(C)]
struct Elf64Ehdr {
    e_ident: [u8; 16],   // 0:  магическое число + класс + endian + версия
    e_type: u16,         // 16: тип файла (ET_EXEC, ET_DYN, ...)
    e_machine: u16,      // 18: архитектура (EM_X86_64)
    e_version: u32,      // 20: версия ELF
    e_entry: u64,        // 24: точка входа (виртуальный адрес)
    e_phoff: u64,        // 32: смещение таблицы program headers в файле
    e_shoff: u64,        // 40: смещение таблицы section headers
    e_flags: u32,        // 48: флаги (зависят от архитектуры)
    e_ehsize: u16,       // 52: размер этого заголовка
    e_phentsize: u16,    // 54: размер одной записи program header
    e_phnum: u16,        // 56: количество program headers
    e_shentsize: u16,    // 58: размер одной записи section header
    e_shnum: u16,        // 60: количество section headers
    e_shstrndx: u16,     // 62: индекс секции со строками имён секций
}

/// Program header ELF64 (56 байт).
/// Описывает один сегмент: где в файле, куда в памяти, размер, права.
#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64Phdr {
    p_type: u32,   // 0:  тип сегмента (PT_LOAD, PT_NULL, ...)
    p_flags: u32,  // 4:  права доступа (PF_R | PF_W | PF_X)
    p_offset: u64, // 8:  смещение данных сегмента в файле
    p_vaddr: u64,  // 16: виртуальный адрес сегмента в памяти
    p_paddr: u64,  // 24: физический адрес (не используется в paging)
    p_filesz: u64, // 32: размер данных сегмента в файле
    p_memsz: u64,  // 40: размер сегмента в памяти (filesz <= memsz, остаток — bss)
    p_align: u64,  // 48: выравнивание сегмента в файле и памяти
}

// ── Публичный API ────────────────────────────────────────────────

/// Результат загрузки ELF: точка входа процесса.
pub struct ElfLoadResult {
    /// Точка входа (e_entry из ELF header) — виртуальный адрес в user space.
    pub entry_point: u64,
}

/// Парсит и загружает ELF64 в адресное пространство процесса (P4).
///
/// # Аргументы
/// - `elf_bytes` — сырые байты ELF-файла (например, из `include_bytes!`)
/// - `p4_phys` — физический адрес P4 таблицы процесса (созданной через
///   `paging::create_user_p4()`)
///
/// # Что делает
/// 1. Проверяет ELF-магику, класс (64-бит), endian (LE), машину (x86_64),
///    тип (ET_EXEC).
/// 2. Читает таблицу program headers.
/// 3. Для каждого PT_LOAD сегмента:
///    - Вычисляет диапазон страниц [vaddr_start, vaddr_end)
///    - Выделяет физические фреймы через PMM
///    - Мапит страницы в P4 процесса с правами из p_flags
///    - Копирует данные сегмента (p_filesz) через HHDM
///    - Заполняет остаток (p_memsz - p_filesz) нулями (bss)
/// 4. Возвращает точку входа (e_entry).
///
/// # Возвращает
/// `ElfLoadResult` с entry_point или сообщение об ошибке.
///
/// # Безопасность
/// Чтение структур через сырые указатели — UB если входные байты не
/// являются валидным ELF64. Все проверки выполняются до чтения.
pub fn load_elf(elf_bytes: &[u8], p4_phys: u64) -> Result<ElfLoadResult, &'static str> {
    // ── 1. Проверка минимального размера ──
    if elf_bytes.len() < core::mem::size_of::<Elf64Ehdr>() {
        return Err("ELF: too small for header");
    }

    // ── 2. Чтение заголовка (через сырой указатель — repr(C) структура) ──
    let ehdr: &Elf64Ehdr = unsafe {
        &*(elf_bytes.as_ptr() as *const Elf64Ehdr)
    };

    // ── 3. Проверка магического числа ──
    if ehdr.e_ident[0..4] != ELFMAG {
        return Err("ELF: bad magic");
    }

    // ── 4. Проверка класса (64-бит) ──
    if ehdr.e_ident[4] != ELFCLASS64 {
        return Err("ELF: not 64-bit");
    }

    // ── 5. Проверка endian (little-endian) ──
    if ehdr.e_ident[5] != ELFDATA2LSB {
        return Err("ELF: not little-endian");
    }

    // ── 6. Проверка архитектуры (x86_64) ──
    if ehdr.e_machine != EM_X86_64 {
        return Err("ELF: not x86_64");
    }

    // ── 7. Проверка типа (ET_EXEC — статический исполняемый) ──
    if ehdr.e_type != ET_EXEC {
        return Err("ELF: not ET_EXEC (dynamic/unsupported)");
    }

    println!(
        "ELF: entry=0x{:016x}, phoff={}, phnum={}, phentsize={}",
        ehdr.e_entry, ehdr.e_phoff, ehdr.e_phnum, ehdr.e_phentsize
    );

    // ── 8. Проверка таблицы program headers ──
    let phdr_size = core::mem::size_of::<Elf64Phdr>() as u16;
    if ehdr.e_phentsize < phdr_size {
        return Err("ELF: phentsize too small");
    }
    if ehdr.e_phnum == 0 {
        return Err("ELF: no program headers");
    }

    let phoff = ehdr.e_phoff as usize;
    let phnum = ehdr.e_phnum as usize;
    let phentsize = ehdr.e_phentsize as usize;

    // Проверка, что вся таблица program headers помещается в файл
    let phdr_end = phoff.checked_add(phnum.checked_mul(phentsize).ok_or("ELF: phnum overflow")?)
        .ok_or("ELF: phoff overflow")?;
    if phdr_end > elf_bytes.len() {
        return Err("ELF: program header table out of bounds");
    }

    // ── 9. Загрузка каждого PT_LOAD сегмента ──
    let mut loaded_segments = 0u32;

    for i in 0..phnum {
        let phdr_offset = phoff + i * phentsize;
        let phdr: &Elf64Phdr = unsafe {
            &*(elf_bytes.as_ptr().add(phdr_offset) as *const Elf64Phdr)
        };

        if phdr.p_type != PT_LOAD {
            // Пропускаем не-LOAD сегменты (PT_NULL, PT_GNU_STACK, ...)
            continue;
        }

        println!(
            "ELF: PT_LOAD vaddr=0x{:016x} filesz={} memsz={} flags=0x{:x} offset={}",
            phdr.p_vaddr, phdr.p_filesz, phdr.p_memsz, phdr.p_flags, phdr.p_offset
        );

        load_segment(elf_bytes, phdr, p4_phys)?;
        loaded_segments += 1;
    }

    if loaded_segments == 0 {
        return Err("ELF: no PT_LOAD segments found");
    }

    println!("ELF: loaded {} segment(s), entry=0x{:016x}", loaded_segments, ehdr.e_entry);

    Ok(ElfLoadResult {
        entry_point: ehdr.e_entry,
    })
}

/// Загружает один PT_LOAD сегмент в адресное пространство процесса.
///
/// Алгоритм:
/// 1. Вычисляем диапазон страниц: от floor(vaddr / PAGE) до ceil((vaddr+memsz) / PAGE) - 1
/// 2. Для каждой страницы:
///    - Выделяем физический фрейм (pmm::alloc_frame)
///    - Мапим virt → phys в P4 процесса (paging::map_page_in_p4)
///    - Через HHDM копируем данные файла, попадающие на эту страницу
///    - Остаток страницы — нули (bss / padding)
///
/// Флаги страницы:
/// - PF_X → исполняемая (НЕ добавляем NO_EXECUTE)
/// - PF_W → WRITABLE
/// - Все user страницы: PRESENT | USER_ACCESSIBLE
/// - Если не PF_X → добавляем NO_EXECUTE (защита от выполнения данных)
fn load_segment(
    elf_bytes: &[u8],
    phdr: &Elf64Phdr,
    p4_phys: u64,
) -> Result<(), &'static str> {
    // Вычисляем флаги страницы из p_flags
    let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if phdr.p_flags & PF_W != 0 {
        flags |= PageTableFlags::WRITABLE;
    }
    if phdr.p_flags & PF_X == 0 {
        // Неисполняемый сегмент — устанавливаем NX бит.
        // Для исполняемых сегментов NX не ставим (код должен выполняться).
        flags |= PageTableFlags::NO_EXECUTE;
    }

    // Диапазон виртуальных адресов сегмента
    let vaddr_start = phdr.p_vaddr;
    let vaddr_end = phdr.p_vaddr + phdr.p_memsz;

    // Диапазон страниц (выровненный на 4K)
    let page_start = vaddr_start & !(PAGE_SIZE - 1); // floor
    let page_end = (vaddr_end + PAGE_SIZE - 1) & !(PAGE_SIZE - 1); // ceil

    // Смещение данных сегмента в файле
    let file_offset = phdr.p_offset;
    let file_size = phdr.p_filesz;

    let hhdm = paging::hhdm_offset();

    let mut page_virt = page_start;
    while page_virt < page_end {
        // Выделяем физический фрейм для этой страницы
        let phys = pmm::alloc_frame().ok_or("ELF: OOM for segment page")?;

        // Мапим страницу в P4 процесса
        paging::map_page_in_p4(p4_phys, page_virt, phys, flags)?;

        // Получаем доступ к физическому фрейму через HHDM
        let frame_ptr = unsafe {
            (phys + hhdm) as *mut u8
        };

        // Сначала заполняем всю страницу нулями (bss + padding)
        unsafe {
            core::ptr::write_bytes(frame_ptr, 0, PAGE_SIZE as usize);
        }

        // Вычисляем, какая часть данных файла попадает на эту страницу
        // Виртуальный адрес начала данных на этой странице:
        //   data_virt_start = max(page_virt, vaddr_start)
        // Смещение данных в файле:
        //   file_data_offset = file_offset + (data_virt_start - vaddr_start)
        let data_virt_start = if page_virt > vaddr_start {
            page_virt
        } else {
            vaddr_start
        };

        // Сколько байт данных файла приходится на эту страницу
        let data_virt_end = if page_virt + PAGE_SIZE < vaddr_end {
            page_virt + PAGE_SIZE
        } else {
            vaddr_end
        };

        // Но данных файла может быть меньше, чем memsz (bss)
        let data_end_in_mem = vaddr_start + file_size;
        let copy_virt_end = if data_virt_end < data_end_in_mem {
            data_virt_end
        } else {
            data_end_in_mem
        };

        // Если на этой странице есть данные файла — копируем
        if copy_virt_end > data_virt_start {
            // Смещение в файле для этой порции данных
            let file_data_offset = file_offset + (data_virt_start - vaddr_start);
            // Сколько байт копировать
            let copy_len = (copy_virt_end - data_virt_start) as usize;
            // Смещение внутри страницы
            let page_offset = (data_virt_start - page_virt) as usize;

            // Проверка границ файла
            let file_copy_end = file_data_offset.checked_add(copy_len as u64)
                .ok_or("ELF: file offset overflow")?;
            if file_copy_end as usize > elf_bytes.len() {
                return Err("ELF: segment data out of file bounds");
            }

            unsafe {
                let src = elf_bytes.as_ptr().add(file_data_offset as usize);
                let dst = frame_ptr.add(page_offset);
                core::ptr::copy_nonoverlapping(src, dst, copy_len);
            }
        }

        page_virt += PAGE_SIZE;
    }

    Ok(())
}
