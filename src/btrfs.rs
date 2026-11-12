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

use btrfs_no_std::{SuperBlock, Header, Key, Item, Chunk, KeyPointer};

/// Адрес суперблока (64 КБ от начала диска).
const SUPERBLOCK_ADDR: u64 = 0x10000;

/// Magic btrfs: "_BHRfS_M"
const BTRFS_MAGIC: u64 = 0x4D5F53665248425F;

/// Типы ключей btrfs.
const DIR_ITEM_KEY: u8 = 84;
const EXTENT_DATA_KEY: u8 = 108;
const ROOT_ITEM_KEY: u8 = 132;
const CHUNK_ITEM_KEY: u8 = 228;

/// Object IDs корневых деревьев.
const FS_TREE_OBJECTID: u64 = 5;
// Корневой каталог FS tree = 256 (BTRFS_FIRST_FREE_OBJECTID).
// НЕ 6 (BTRFS_ROOT_TREE_DIR_OBJECTID=6 — это каталог root tree).
const ROOT_DIR_OBJECTID: u64 = 256;

/// Размер узла B-дерева (из суперблока, обычно 16384).
static mut NODE_SIZE: u32 = 16384;

/// Сохранённый суперблок (для logical_to_physical и поиска root tree).
/// Код выделен бирюзовым цветом (правило .kodaruls п.6).
static mut SUPERBLOCK: Option<SuperBlock> = None;

/// Кэш chunk mappings из chunk tree (правило .kodaruls п.6).
/// Каждый entry: (logical_start, chunk_length, physical_start).
const MAX_CHUNKS: usize = 64;
static mut CHUNK_MAP: [(u64, u64, u64); MAX_CHUNKS] = [(0, 0, 0); MAX_CHUNKS];
static mut CHUNK_MAP_COUNT: usize = 0;

/// Кэш адреса FS tree root (вычисляется один раз, правило .kodaruls п.6).
static mut FS_ROOT_ADDR: u64 = 0;

/// Типы файловых записей в DIR_ITEM (btrfs_dir_item.type).
/// Код выделен бирюзовым цветом (правило .kodaruls п.6).
const DIR_TYPE_DIRECTORY: u8 = 2;

/// Размер DirItem в leaf B-дерева btrfs (30 байт).
const DIR_ITEM_SIZE: usize = 30;

/// Размер заголовка FileExtentItem (37 байт) — данные inline следуют сразу после.
const FILE_EXTENT_HEADER_SIZE: usize = 37;

/// Типы EXTENT_DATA (btrfs_file_extent_item.type).
const EXTENT_TYPE_INLINE: u8 = 0;
const EXTENT_TYPE_REGULAR: u8 = 1;

/// Возвращает логический адрес корня FS tree (подтом objectid=5).
/// Кэширует результат для повторных вызовов.
fn get_fs_root_addr() -> Option<u64> {
    unsafe {
        if FS_ROOT_ADDR != 0 {
            return Some(FS_ROOT_ADDR);
        }
    }

    let sb = unsafe { SUPERBLOCK? };
    let root_addr = sb.root.get();

    // Ищем ROOT_ITEM_KEY (132) с objectid=FS_TREE_OBJECTID (5), offset=0
    let (_key, data_off, data_sz, leaf_buf) =
        find_item(root_addr, FS_TREE_OBJECTID, ROOT_ITEM_KEY, 0)?;

    if data_sz < core::mem::size_of::<btrfs_no_std::RootItem>() {
        crate::println!("btrfs: ROOT_ITEM too small ({})", data_sz);
        return None;
    }

    let root_item = unsafe {
        &*(leaf_buf.as_ptr().add(data_off) as *const btrfs_no_std::RootItem)
    };
    let fs_root = root_item.bytenr.get();
    unsafe { FS_ROOT_ADDR = fs_root; }
    crate::println!("btrfs: FS tree root at 0x{:x}", fs_root);
    Some(fs_root)
}

/// Читает u64 LE из среза по смещению (правило .kodaruls п.6).
fn read_u64_le(buf: &[u8], offset: usize) -> Option<u64> {
    if offset + 8 > buf.len() { return None; }
    Some(u64::from_le_bytes(buf[offset..offset+8].try_into().ok()?))
}

/// Читает u16 LE из среза по смещению (правило .kodaruls п.6).
fn read_u16_le(buf: &[u8], offset: usize) -> Option<u16> {
    if offset + 2 > buf.len() { return None; }
    Some(u16::from_le_bytes(buf[offset..offset+2].try_into().ok()?))
}

/// Читает u8 из среза по смещению (правило .kodaruls п.6).
fn read_u8(buf: &[u8], offset: usize) -> Option<u8> {
    if offset >= buf.len() { return None; }
    Some(buf[offset])
}

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

    // Сохраняем суперблок глобально для logical_to_physical (правило .kodaruls п.6).
    unsafe { SUPERBLOCK = Some(sb); }

    crate::println!("btrfs: superblock OK");
    crate::println!("  root=0x{:x}, chunk_root=0x{:x}", sb.root.get(), sb.chunk_root.get());
    crate::println!("  nodesize={}, sectorsize={}", sb.nodesize.get(), sb.sectorsize.get());
    crate::println!("  total_bytes={}", sb.total_bytes.get());

    Some(sb)
}

/// Читает узел B-дерева (node/leaf) по ЛОГИЧЕСКОМУ адресу.
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

/// Читает узел B-дерева по ФИЗИЧЕСКОМУ адресу (без chunk mapping).
///
/// Нужен для чтения chunk tree: его root (chunk_root) лежит в metadata
/// chunk, который не замаплен в system chunk array. Читаем напрямую
/// по физическому адресу = логическому (identity для первых адресов).
/// Код выделен бирюзовым цветом (правило .kodaruls п.6).
fn read_node_identity(phys_addr: u64) -> Option<alloc::vec::Vec<u8>> {
    let node_size = unsafe { NODE_SIZE } as usize;
    let mut buf = alloc::vec![0u8; node_size];
    if !read_disk(phys_addr, &mut buf) {
        return None;
    }
    Some(buf)
}

/// Заполняет CHUNK_MAP из chunk tree (sb.chunk_root).
///
/// Chunk tree содержит CHUNK_ITEM (type=128): Key.objectid = logical_start,
/// Key.offset = chunk_length, данные = Chunk (stripe.offset = physical_start).
/// Читает chunk tree (корень в system chunk array, доступен по identity или
/// через system mapping), обходит B-tree и собирает все chunk mappings.
///
/// Код выделен бирюзовым цветом (правило .kodaruls п.6).
fn init_chunk_map() {
    let sb = match unsafe { SUPERBLOCK } { Some(s) => s, None => return };
    let chunk_root = sb.chunk_root.get();

    // Диагностика: выводим system chunk array (правило .kodaruls п.6).
    let array = &sb.sys_chunk_array;
    let array_size = sb.sys_chunk_array_size.get() as usize;
    let key_size = core::mem::size_of::<Key>();
    let chunk_size = core::mem::size_of::<Chunk>();
    let mut off = 0usize;
    while off + key_size + chunk_size <= array_size {
        let key = unsafe { &*(array.as_ptr().add(off) as *const Key) };
        let chunk = unsafe { &*(array.as_ptr().add(off + key_size) as *const Chunk) };
        let logical = key.objectid.get();
        let length = key.offset.get();
        let phys = chunk.stripe.offset.get();
        crate::println!("btrfs: sys chunk logical=0x{:x} len=0x{:x} phys=0x{:x}",
            logical, length, phys);
        let num_stripes = chunk.num_stripes.get() as usize;
        off += key_size + chunk_size + (num_stripes.saturating_sub(1)) * 32;
    }
    crate::println!("btrfs: chunk_root=0x{:x}", chunk_root);

    // Счётчик найденных chunk'ов.
    let mut count = 0usize;

    // Обходим chunk tree: спускаемся к leaf'ам, собираем CHUNK_ITEM.
    // Используем стек адресов (BFS/DFS по KeyPointer).
    // Читаем по identity (физический = логический): chunk_root и его
    // child-узлы лежат в metadata chunk, который не замаплен в system
    // chunk array. Для одиночного диска identity корректен для первых
    // metadata-узлов (правило .kodaruls п.6).
    let mut stack = alloc::vec::Vec::new();
    stack.push(chunk_root);

    while let Some(node_addr) = stack.pop() {
        // Читаем по физическому адресу (identity), без logical_to_physical.
        let buf = match read_node_identity(node_addr) { Some(b) => b, None => continue };
        let header = match read_header(&buf) { Some(h) => h, None => continue };
        let level = header.level;
        let num_items = header.num_items.get() as usize;
        let header_size = core::mem::size_of::<Header>();
        let item_size = core::mem::size_of::<Item>();
        let kp_size = core::mem::size_of::<KeyPointer>();

        // Диагностика узла (правило .kodaruls п.6).
        crate::println!("btrfs: chunk node 0x{:x} level={} items={}",
            node_addr, level, num_items);

        if level == 0 {
            // Leaf: собираем CHUNK_ITEM
            for i in 0..num_items {
                let item_offset = header_size + i * item_size;
                if item_offset + item_size > buf.len() { break; }
                let item = unsafe { &*(buf.as_ptr().add(item_offset) as *const Item) };

                // Диагностика ВСЕХ items (правило .kodaruls п.6).
                // Ручное чтение полей Key для проверки смещений.
                let obj_manual = read_u64_le(&buf, item_offset).unwrap_or(0);
                let type_manual = read_u8(&buf, item_offset + 8).unwrap_or(0);
                let off_manual = read_u64_le(&buf, item_offset + 9).unwrap_or(0);
                crate::println!("btrfs:   item[{}] obj=0x{:x} type={} off=0x{:x} (manual: obj=0x{:x} type={} off=0x{:x})",
                    i, item.key.objectid.get(), item.key.r#type, item.key.offset.get(),
                    obj_manual, type_manual, off_manual);

                if type_manual != CHUNK_ITEM_KEY { continue; }

                // В CHUNK_ITEM logical_start = key.offset (а не objectid,
                // который всегда = FIRST_CHUNK_TREE = 3). chunk_length
                // берётся из данных Chunk.length.
                let logical_start = off_manual; // item.key.offset.get() = логический адрес
                let data_start = item.offset.get() as usize + header_size;

                if data_start + core::mem::size_of::<Chunk>() > buf.len() { continue; }
                let chunk = unsafe { &*(buf.as_ptr().add(data_start) as *const Chunk) };
                let chunk_length = chunk.length.get();
                let physical_start = chunk.stripe.offset.get();

                // Диагностика chunk (правило .kodaruls п.6).
                crate::println!("btrfs:   CHUNK logical=0x{:x} len=0x{:x} phys=0x{:x}",
                    logical_start, chunk_length, physical_start);

                if count < MAX_CHUNKS {
                    unsafe {
                        CHUNK_MAP[count] = (logical_start, chunk_length, physical_start);
                        count += 1;
                    }
                }
            }
        } else {
            // Internal node: добавляем child адреса в стек
            for i in 0..num_items {
                let kp_offset = header_size + i * kp_size;
                if kp_offset + kp_size > buf.len() { break; }
                let kp = unsafe { &*(buf.as_ptr().add(kp_offset) as *const KeyPointer) };
                let child = kp.block_pointer.get();
                // Диагностика child (правило .kodaruls п.6).
                crate::println!("btrfs:   child 0x{:x}", child);
                stack.push(child);
            }
        }
    }

    unsafe {
        CHUNK_MAP_COUNT = count;
    }
    crate::println!("btrfs: chunk map loaded: {} chunks", count);
}

/// Преобразует логический адрес btrfs в физический адрес на диске.
///
/// BTRFS использует виртуальные (логические) адреса. Chunk mapping
/// мапит logical → physical. Ищем в:
///   1. CHUNK_MAP (собран из chunk tree при init_chunk_map).
///   2. system chunk array в суперблоке.
/// Каждая запись: logical_start, chunk_length, physical_start.
///
/// Код выделен бирюзовым цветом (правило .kodaruls п.6).
fn logical_to_physical(logical: u64) -> Option<u64> {
    // 1. Ищем в кэше из chunk tree
    unsafe {
        for i in 0..CHUNK_MAP_COUNT {
            let (start, len, phys) = CHUNK_MAP[i];
            if logical >= start && logical < start + len {
                return Some(phys + (logical - start));
            }
        }
    }

    // 2. Ищем в system chunk array суперблок
    let sb = unsafe { SUPERBLOCK? };
    let array = &sb.sys_chunk_array;
    let array_size = sb.sys_chunk_array_size.get() as usize;

    let key_size = core::mem::size_of::<Key>();       // 17
    let chunk_size = core::mem::size_of::<Chunk>();   // 80

    let mut offset = 0usize;
    while offset + key_size + chunk_size <= array_size {
        // Читаем Key (17 байт)
        let key = unsafe { &*(array.as_ptr().add(offset) as *const Key) };
        let logical_start = key.objectid.get();
        let chunk_length = key.offset.get();

        // Читаем Chunk (80 байт) сразу после Key
        let chunk = unsafe {
            &*(array.as_ptr().add(offset + key_size) as *const Chunk)
        };
        let physical_start = chunk.stripe.offset.get();

        // Проверяем, попадает ли logical в этот chunk
        if logical >= logical_start && logical < logical_start + chunk_length {
            return Some(physical_start + (logical - logical_start));
        }

        // Переходим к следующей записи (Key + Chunk + доп. Stripe)
        let num_stripes = chunk.num_stripes.get() as usize;
        let stripe_size = core::mem::size_of::<btrfs_no_std::Stripe>(); // 32
        offset += key_size + chunk_size + (num_stripes.saturating_sub(1)) * stripe_size;
    }

    // Fallback: если не найдено, используем identity с диагностикой.
    crate::println!("btrfs: chunk mapping not found for 0x{:x}, using identity", logical);
    Some(logical)
}

/// Читает Header из начала буфера узла.
fn read_header(buf: &[u8]) -> Option<&Header> {
    if buf.len() < core::mem::size_of::<Header>() {
        return None;
    }
    Some(unsafe { &*(buf.as_ptr() as *const Header) })
}

/// Ищет элемент в B-дереве по ключу (objectid, type, offset).
///
/// Начинает с корня, обходит внутренние узлы через KeyPointer,
/// в листе ищет Item с matching key (включая offset).
/// Возвращает (item_key, data_offset, data_size, leaf_buf).
///
/// Код выделен бирюзовым цветом (правило .kodaruls п.6) — добавлен
/// параметр target_offset для точного поиска DIR_ITEM (offset = name hash)
/// и EXTENT_DATA (offset = file offset).
fn find_item(root_addr: u64, target_objectid: u64, target_type: u8, target_offset: u64) -> Option<(Key, usize, usize, alloc::vec::Vec<u8>)> {
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
                let off = key.offset.get();

                // Ищем точное совпадение (objectid, type, offset)
                if obj == target_objectid && typ == target_type && off == target_offset {
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

            // Ищем подходящий KeyPointer: первый, чей key >= target.
            // Сравниваем по (objectid, type, offset) — btrfs хранит ключи
            // в лексикографическом порядке (objectid, type, offset).
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
                let off = kp.key.offset.get();

                // Сравниваем тройку (objectid, type, offset) с целевой
                if obj > target_objectid
                    || (obj == target_objectid && typ > target_type)
                    || (obj == target_objectid && typ == target_type && off >= target_offset)
                {
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
            // Загружаем chunk map из chunk tree — без него logical→physical
            // для root/FS tree и данных не работает (правило .kodaruls п.6).
            init_chunk_map();
            crate::println!("btrfs: mounted successfully");
            true
        }
        None => {
            crate::println!("btrfs: mount failed");
            false
        }
    }
}

// ============================================================================
// Функции чтения файлов и каталогов с btrfs-диска.
// Код выделен бирюзовым цветом (правило .kodaruls п.6).
// ============================================================================

/// Находит leaf, содержащий items с заданным (objectid, type).
///
/// Обходит B-tree от корня, спускается к leaf.
/// Возвращает (leaf_buf, header_size, item_size) для дальнейшего перебора.
fn find_leaf(root_addr: u64, target_objectid: u64, target_type: u8)
    -> Option<(alloc::vec::Vec<u8>, usize, usize)>
{
    let mut addr = root_addr;

    for _ in 0..20 {
        let buf = read_node(addr)?;
        let header = read_header(&buf)?;
        let level = header.level;
        let num_items = header.num_items.get() as usize;
        let header_size = core::mem::size_of::<Header>();
        let item_size = core::mem::size_of::<Item>();
        let kp_size = core::mem::size_of::<KeyPointer>();

        if level == 0 {
            return Some((buf, header_size, item_size));
        }

        // Internal node: ищем KeyPointer
        let mut next_addr = None;
        for i in 0..num_items {
            let kp_offset = header_size + i * kp_size;
            if kp_offset + kp_size > buf.len() { break; }
            let kp = unsafe { &*(buf.as_ptr().add(kp_offset) as *const KeyPointer) };
            let obj = kp.key.objectid.get();
            let typ = kp.key.r#type;

            if obj > target_objectid
                || (obj == target_objectid && typ >= target_type)
            {
                next_addr = Some(kp.block_pointer.get());
                break;
            }
        }

        if next_addr.is_none() && num_items > 0 {
            let kp_offset = header_size + (num_items - 1) * kp_size;
            if kp_offset + kp_size <= buf.len() {
                let kp = unsafe { &*(buf.as_ptr().add(kp_offset) as *const KeyPointer) };
                next_addr = Some(kp.block_pointer.get());
            }
        }

        match next_addr {
            Some(a) => addr = a,
            None => return None,
        }
    }
    None
}

/// Ищет файл/подкаталог по имени в указанном каталоге.
///
/// Перебирает все DIR_ITEM (type=84) с objectid=parent_dir в FS tree.
/// Возвращает inode objectid найденного файла или None.
fn lookup_in_dir(parent_dir: u64, name: &str) -> Option<u64> {
    let fs_root = get_fs_root_addr()?;
    let (leaf_buf, header_size, item_size) = find_leaf(fs_root, parent_dir, DIR_ITEM_KEY)?;

    let name_bytes = name.as_bytes();
    let num_items = read_header(&leaf_buf)?.num_items.get() as usize;

    for i in 0..num_items {
        let item_offset = header_size + i * item_size;
        if item_offset + item_size > leaf_buf.len() { break; }

        let item = unsafe { &*(leaf_buf.as_ptr().add(item_offset) as *const Item) };
        let obj = item.key.objectid.get();
        let typ = item.key.r#type;

        if obj != parent_dir || typ != DIR_ITEM_KEY { continue; }

        // Данные item: один или несколько DirItem (для коллизий хеша)
        let data_start = item.offset.get() as usize + header_size;
        let data_end = data_start + item.size.get() as usize;
        let mut pos = data_start;

        while pos + DIR_ITEM_SIZE <= data_end {
            // DirItem: location (17 байт) + transid (8) + data_len (2) + name_size (2)
            let inode = read_u64_le(&leaf_buf, pos)?;
            let name_size = read_u16_le(&leaf_buf, pos + 27)? as usize;
            let data_len = read_u16_le(&leaf_buf, pos + 25)? as usize;

            // Имя следует сразу после DirItem
            let name_start = pos + DIR_ITEM_SIZE;
            if name_start + name_size > leaf_buf.len() { break; }

            let item_name = &leaf_buf[name_start..name_start + name_size];
            if item_name == name_bytes {
                return Some(inode);
            }

            // Переходим к следующему DirItem (структура + имя + данные)
            pos = name_start + name_size + data_len;
        }
    }

    None
}

/// Разрешает путь (например "/foo/bar.txt") в inode objectid.
///
/// Обходит компоненты пути от корневого каталога (objectid=6).
fn resolve_path(path: &str) -> Option<u64> {
    let mut current_dir = ROOT_DIR_OBJECTID;
    let trimmed = path.trim_start_matches('/');

    if trimmed.is_empty() {
        return Some(current_dir); // корень
    }

    for component in trimmed.split('/') {
        if component.is_empty() { continue; }
        current_dir = lookup_in_dir(current_dir, component)?;
    }

    Some(current_dir)
}

/// Читает данные файла по inode objectid.
///
/// Ищет EXTENT_DATA (type=108) для данного inode, поддерживает
/// inline (данные в leaf) и regular (данные на диске).
/// Возвращает Vec<u8> с содержимым файла.
pub fn read_file_by_inode(inode: u64) -> Option<alloc::vec::Vec<u8>> {
    let fs_root = get_fs_root_addr()?;
    let (leaf_buf, header_size, item_size) = find_leaf(fs_root, inode, EXTENT_DATA_KEY)?;

    let num_items = read_header(&leaf_buf)?.num_items.get() as usize;
    let mut result = alloc::vec::Vec::new();

    for i in 0..num_items {
        let item_offset = header_size + i * item_size;
        if item_offset + item_size > leaf_buf.len() { break; }

        let item = unsafe { &*(leaf_buf.as_ptr().add(item_offset) as *const Item) };
        let obj = item.key.objectid.get();
        let typ = item.key.r#type;

        if obj != inode || typ != EXTENT_DATA_KEY { continue; }

        let data_start = item.offset.get() as usize; // offset уже включает header_size (из find_item)

        if data_start + FILE_EXTENT_HEADER_SIZE > leaf_buf.len() {
            break;
        }

        let extent_type = read_u8(&leaf_buf, data_start)?;
        let disk_bytenr = read_u64_le(&leaf_buf, data_start + 1)?;
        let disk_num_bytes = read_u64_le(&leaf_buf, data_start + 9)?;
        let num_bytes = read_u64_le(&leaf_buf, data_start + 17)?;

        // Диагностика extent (правило .kodaruls п.6)
        crate::println!("btrfs: extent obj=0x{:x} type={} item_size={} disk_bytenr=0x{:x} disk_num_bytes={} num_bytes={}",
            obj, extent_type, item.size.get(), disk_bytenr, disk_num_bytes, num_bytes);

        // Диагностика: выводим первые 40 байт extent_struct
        let dump_len = (data_start + 40).min(leaf_buf.len());
        if dump_len > data_start {
            crate::println!("btrfs:   extent_struct bytes:");
            for i in data_start..dump_len {
                crate::print!(" {:02x}", leaf_buf[i]);
            }
            crate::println!();
        }

        match extent_type {
            EXTENT_TYPE_INLINE => {
                // Inline: данные файла следуют сразу после extent_struct (37 байт)
                // num_bytes — размер данных файла (для inline)
                // item.size — размер Item в leaf B-дерева (включает extent_struct + данные)
                let inline_start = data_start + FILE_EXTENT_HEADER_SIZE;
                let inline_len = (item.size.get() as usize).saturating_sub(FILE_EXTENT_HEADER_SIZE);

                crate::println!("btrfs:   inline: start=0x{:x} len={} (item_size=53 - 37)",
                    inline_start, inline_len);

                if inline_len > 0 && inline_start + inline_len <= leaf_buf.len() {
                    result.extend_from_slice(&leaf_buf[inline_start..inline_start + inline_len]);
                }
            }
            EXTENT_TYPE_REGULAR => {
                // Regular: данные на диске по логическому адресу
                // disk_bytenr — физический адрес на диске (через chunk mapping)
                // disk_num_bytes — размер на диске
                // num_bytes — размер в памяти (RAM)
                // num_bytes — это размер данных файла, который нужно вернуть!

                if disk_bytenr == 0 || num_bytes == 0 { continue; }

                // Читаем с диска (через chunk mapping)
                let mut buf = alloc::vec![0u8; disk_num_bytes as usize];
                let phys = logical_to_physical(disk_bytenr)?;
                if !read_disk(phys, &mut buf) {
                    crate::println!("btrfs: failed to read extent at 0x{:x}", disk_bytenr);
                    continue;
                }
                // Берём num_bytes байт (размер файла), а не disk_num_bytes
                let take = num_bytes.min(buf.len() as u64) as usize;
                result.extend_from_slice(&buf[..take]);
            }
            _ => {
                crate::println!("btrfs: unknown extent type {}", extent_type);
            }
        }
    }

    if result.is_empty() { None } else { Some(result) }
}

/// Читает файл по пути (например "/test.txt").
///
/// Разрешает путь в inode, затем читает данные.
pub fn read_file(path: &str) -> Option<alloc::vec::Vec<u8>> {
    let inode = resolve_path(path)?;
    read_file_by_inode(inode)
}

/// Список файлов в каталоге по пути.
///
/// Возвращает Vec<(имя, inode, тип)>.
pub struct DirEntry {
    pub name: alloc::string::String,
    pub inode: u64,
    pub is_dir: bool,
}

pub fn list_dir(path: &str) -> Option<alloc::vec::Vec<DirEntry>> {
    let parent_dir = resolve_path(path)?;
    let fs_root = get_fs_root_addr()?;
    let (leaf_buf, header_size, item_size) = find_leaf(fs_root, parent_dir, DIR_ITEM_KEY)?;

    let mut entries = alloc::vec::Vec::new();
    let num_items = read_header(&leaf_buf)?.num_items.get() as usize;

    for i in 0..num_items {
        let item_offset = header_size + i * item_size;
        if item_offset + item_size > leaf_buf.len() { break; }

        let item = unsafe { &*(leaf_buf.as_ptr().add(item_offset) as *const Item) };
        let obj = item.key.objectid.get();
        let typ = item.key.r#type;

        if obj != parent_dir || typ != DIR_ITEM_KEY { continue; }

        let data_start = item.offset.get() as usize + header_size;
        let data_end = data_start + item.size.get() as usize;
        let mut pos = data_start;

        while pos + DIR_ITEM_SIZE <= data_end {
            let inode = read_u64_le(&leaf_buf, pos)?;
            let name_size = read_u16_le(&leaf_buf, pos + 27)? as usize;
            let data_len = read_u16_le(&leaf_buf, pos + 25)? as usize;
            // Тип файла: поле type в DirItem (последний байт фиксированной
            // части, на offset DIR_ITEM_SIZE-1 = 29). НЕ location.type (pos+8).
            let file_type = read_u8(&leaf_buf, pos + DIR_ITEM_SIZE - 1)?;
            let is_dir = file_type == DIR_TYPE_DIRECTORY;

            let name_start = pos + DIR_ITEM_SIZE;
            if name_start + name_size > leaf_buf.len() { break; }

            let name_bytes = &leaf_buf[name_start..name_start + name_size];
            let name = alloc::string::String::from_utf8_lossy(name_bytes).into_owned();
            entries.push(DirEntry { name, inode, is_dir });

            pos = name_start + name_size + data_len;
        }
    }

    Some(entries)
}
