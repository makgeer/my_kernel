//! Виртуальная файловая система (VFS).
//!
//! Абстракция над конкретными ФС (initrd, позже — FAT32/ext2).
//! Предоставляет единый интерфейс: поиск файла по пути, чтение,
//! список содержимого каталога.
//!
//! Узлы ФС (FsNode) — это «inode»: имя, тип, размер и данные.
//! Для initrd данные — это просто байты в памяти (встроенный образ).
//! Позже для дисковых ФС данные будут читаться с блочного устройства.

/// Тип узла ФС.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsNodeType {
    /// Обычный файл (можно читать).
    File,
    /// Каталог (можно перечислять содержимое).
    Directory,
}

/// Узел ФС (inode).
///
/// Для initrd: файл содержит ссылку на свои данные в памяти.
/// Размер — длина данных.
#[derive(Clone, Copy)]
pub struct FsNode {
    /// Имя файла/каталога (без пути).
    pub name: &'static str,
    /// Тип узла.
    pub node_type: FsNodeType,
    /// Данные файла (для initrd — байты в памяти). Для каталога — пустой.
    pub data: &'static [u8],
    /// Список дочерних узлов (для каталога).
    pub children: &'static [&'static FsNode],
}

impl FsNode {
    /// Создаёт файл.
    pub const fn file(name: &'static str, data: &'static [u8]) -> Self {
        Self {
            name,
            node_type: FsNodeType::File,
            data,
            children: &[],
        }
    }

    /// Создаёт каталог.
    pub const fn dir(name: &'static str, children: &'static [&'static FsNode]) -> Self {
        Self {
            name,
            node_type: FsNodeType::Directory,
            data: &[],
            children,
        }
    }

    /// Возвращает размер файла.
    pub fn size(&self) -> usize {
        self.data.len()
    }
}

/// Корневой каталог VFS.
///
/// Включает все смонтированные ФС. Сейчас — только initrd.
/// Позже добавится корень диска (FAT32/ext2).
static ROOT: FsNode = FsNode::dir("/", &[]);

/// Ищет узел по пути (например "/bin/hello" или "hello").
///
/// Путь разбивается на компоненты по '/'. Пустой путь или "/" → корень.
/// Возвращает None если узел не найден.
pub fn lookup(path: &str) -> Option<&'static FsNode> {
    // Пустой путь → корень
    if path.is_empty() || path == "/" {
        return Some(&ROOT);
    }

    // Ищем от корня, проходя по компонентам пути
    let mut node = &ROOT;
    for comp in path.split('/').filter(|c| !c.is_empty()) {
        let found = node.children.iter().find(|c| c.name == comp);
        match found {
            Some(child) => node = child,
            None => return None,
        }
    }
    Some(node)
}

/// Возвращает содержимое каталога (список дочерних узлов).
pub fn read_dir(path: &str) -> Option<&'static [&'static FsNode]> {
    let node = lookup(path)?;
    if node.node_type == FsNodeType::Directory {
        Some(node.children)
    } else {
        None
    }
}

/// Читает файл по пути. Возвращает данные файла или None.
pub fn read_file(path: &str) -> Option<&'static [u8]> {
    let node = lookup(path)?;
    if node.node_type == FsNodeType::File {
        Some(node.data)
    } else {
        None
    }
}

/// Монтирует корневой узел (заполняет ROOT).
///
/// Пока ROOT — статический пустой каталог. Для initrd мы заменим
/// его содержимое при монтировании. Используем unsafe static mut,
/// т.к. монтирование происходит один раз при старте ядра.
pub fn mount_root(children: &'static [&'static FsNode]) {
    unsafe {
        let root_ptr = &ROOT as *const FsNode as *mut FsNode;
        (*root_ptr).children = children;
    }
}
