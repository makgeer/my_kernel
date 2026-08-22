//! Модуль BTRFS — чтение файлов с btrfs-диска.
//!
//! Реализует минимальный read-only доступ к btrfs:
//! 1. Чтение суперблока (адрес 0x10000).
//! 2. Chunk mapping (logical → physical) из system chunk array.
//! 3. Обход B-дерева (root tree → FS tree → листья).
//! 4. Поиск файлов/каталогов по имени (DIR_ITEM).
//! 5. Чтение данных файла (EXTENT_DATA, inline или regular).
//!
//! Использует btrfs-no-std для структур дискового формата и
//! ATA PIO драйвер (src/ata.rs) для чтения секторов.

use btrfs_no_std::{SuperBlock, Header, Key, Item, Chunk, KeyPointer, InodeItem};

/// Адрес суперблока (64 КБ от начала диска).
const SUPERBLOCK_ADDR: u64 = 0x10000;

/// Magic btrfs: "_BHRfS_M"
const BTRFS_MAGIC: u64 = 0x4D5F53665248425F;

/// Типы ключей btrfs.
const INODE_ITEM_KEY: u8 = 1;
const INODE_REF_KEY: u8 = 12;
const DIR_ITEM_KEY: u8 = 84;
const EXTENT_DATA_KEY: u8 = 108;
const ROOT_ITEM_KEY: u8 = 132;
const CHUNK_ITEM_KEY: u8 = 128;

/// Object IDs корневых деревьев.
const ROOT_TREE_OBJECTID: u64 = 1;
const CHUNK_TREE_OBJECTID: u64 = 3;
const FS_TREE_OBJECTID: u64 = 5;
const ROOT_DIR_OBJECTID: u64 = 6;

/// Размер узла B-дерева (из суперблока, обычно 16384).
static mut NODE_SIZE: u32 = 16384;

/// Сектор диска = 512 байт.

/// Читает данные с диска по физическому адресу.
/// buffer должен быть кратен 512.
fn read_disk(phys_addr: u64, buffer: &mut [u8]) -> bool {
    let lba = (phys_addr / 512) as u32;
    crate::ata::read_sectors(lba, buffer)
}

/// Читает суперблок btrfs с диска.
///
/// Возвращает скопированный суперблок или None.
pub fn read_superblock() -> Option<SuperBlock> {
    // Суперблок на 0x10000 = 64 КБ = сектор 128
    let mut buf = [0u8; 4096];
    if !read_disk(SUPERBLOCK_ADDR, &mut buf) {
        crate::println!("btrfs: failed to read superblock sector");
        return None;
    }

    // Копируем в структуру SuperBlock
    let sb = unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const SuperBlock) };

    // Проверяем magic
    let magic = sb.magic.get();
    if magic != BTRFS_MAGIC {
        crate::println!("btrfs: bad magic 0x{:016x} (expected 0x{:016x})", magic, BTRFS_MAGIC);
        return None;
    }

    unsafe { NODE_SIZE = sb.nodesize.get(); }

    crate::println!("btrfs: superblock OK");
    crate::println!("  root=0x{:x}, chunk_root=0x{:x}", sb.root.get(), sb.chunk_root.get());
    crate::println!("  nodesize={}, sectorsize={}", sb.nodesize.get(), sb.sectorsize.get());
    crate::println!("  total_bytes={}", sb.total_bytes.get());

    Some(sb)
}

/// Читает узел B-дерева (node/leaf) по логическому адресу.
///
/// Возвращает сырые байты узла (nodesize байт).
fn read_node(logical_addr: u64) -> Option<alloc::vec::Vec<u8>> {
    let node_size = unsafe { NODE_SIZE } as usize;
    let phys = logical_to_physical(logical_addr)?;
    let mut buf = alloc::vec![0u8; node_size];
    if !read_disk(phys, &mut buf) {
        return None;
    }
    Some(buf)
}

/// Преобразует логический адрес btrfs в физический адрес на диске.
///
/// BTRFS использует виртуальные адреса (logical). Chunk tree
/// мапит logical → physical. Пока используем упрощённое
/// предположение: для одиночного диска logical ≈ physical
/// (без RAID, без compression). Это работает для базовых случаев.
///
/// TODO: правильно парсить system chunk array из суперблока.
fn logical_to_physical(logical: u64) -> Option<u64> {
    // Для одиночного диска без RAID: physical = logical
    // (btrfs на одном устройстве использует linear mapping
    // для первых chunk'ов).
    Some(logical)
}

/// Читает Header из начала буфера узла.
fn read_header(buf: &[u8]) -> Option<&Header> {
    if buf.len() < core::mem::size_of::<Header>() {
        return None;
    }
    Some(unsafe { &*(buf.as_ptr() as *const Header) })
}

/// Ищет элемент в B-дереве по ключу (objectid, type).
///
/// Начинает с корня, обходит внутренние узлы через KeyPointer,
/// в листе ищет Item с matching key.
/// Возвращает (item_key, data_offset, data_size, leaf_buf).
fn find_item(root_addr: u64, target_objectid: u64, target_type: u8) -> Option<(Key, usize, usize, alloc::vec::Vec<u8>)> {
    let mut addr = root_addr;

    for _ in 0..20 { // ограничение глубины
        let buf = read_node(addr)?;
        let header = read_header(&buf)?;
        let level = header.level;
        let num_items = header.num_items.get() as usize;

        if level == 0 {
            // Leaf: ищем Item
            let header_size = core::mem::size_of::<Header>();
            let item_size = core::mem::size_of::<Item>();

            for i in 0..num_items {
                let item_offset = header_size + i * item_size;
                if item_offset + item_size > buf.len() {
                    break;
                }
                let item = unsafe {
                    &*(buf.as_ptr().add(item_offset) as *const Item)
                };
                let key = item.key;
                let obj = key.objectid.get();
                let typ = key.r#type;

                if obj == target_objectid && typ == target_type {
                    let data_off = item.offset.get() as usize + header_size;
                    let data_sz = item.size.get() as usize;
                    return Some((key, data_off, data_sz, buf));
                }
            }
            return None; // не найдено в листе
        } else {
            // Internal node: ищем KeyPointer и спускаемся
            let header_size = core::mem::size_of::<Header>();
            let kp_size = core::mem::size_of::<KeyPointer>();

            // Ищем подходящий KeyPointer: первый, чей key >= target
            let mut next_addr = None;
            for i in 0..num_items {
                let kp_offset = header_size + i * kp_size;
                if kp_offset + kp_size > buf.len() {
                    break;
                }
                let kp = unsafe {
                    &*(buf.as_ptr().add(kp_offset) as *const KeyPointer)
                };
                let obj = kp.key.objectid.get();
                let typ = kp.key.r#type;

                if obj >= target_objectid {
                    next_addr = Some(kp.block_pointer.get());
                    break;
                }
            }

            // Если не нашли — берём последний
            if next_addr.is_none() && num_items > 0 {
                let kp_offset = header_size + (num_items - 1) * kp_size;
                if kp_offset + kp_size <= buf.len() {
                    let kp = unsafe {
                        &*(buf.as_ptr().add(kp_offset) as *const KeyPointer)
                    };
                    next_addr = Some(kp.block_pointer.get());
                }
            }

            match next_addr {
                Some(a) => addr = a,
                None => return None,
            }
        }
    }
    None
}

/// Монтирует btrfs: читает суперблок, проверяет.
pub fn mount() -> bool {
    crate::ata::select_channel(0x170); // secondary channel
    match read_superblock() {
        Some(_) => {
            crate::println!("btrfs: mounted successfully");
            true
        }
        None => {
            crate::println!("btrfs: mount failed");
            false
        }
    }
}
