//! Виртуальная файловая система (VFS).
//!
//! Абстракция над конкретными ФС (initrd, позже — FAT32/ext2).
//! Предоставляет единый интерфейс: поиск файла по пути, чтение,
//! список содержимого каталога, создание/удаление файлов и каталогов.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

/// Тип узла ФС.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsNodeType {
    /// Обычный файл (можно читать/писать).
    File,
    /// Каталог (можно перечислять содержимое).
    Directory,
}

/// Узел ФС (inode).
pub struct FsNode {
    /// Имя файла/каталога (без пути).
    pub name: String,
    /// Тип узла.
    pub node_type: FsNodeType,
    /// Данные файла (для initrd — байты в памяти). Для каталога — пустой.
    pub data: Box<[u8]>,
    /// Список дочерних узлов (для каталога).
    pub children: Mutex<Vec<Box<FsNode>>>,
}

impl Clone for FsNode {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            node_type: self.node_type,
            data: self.data.clone(),
            children: Mutex::new(self.children.lock().clone()),
        }
    }
}

impl FsNode {
    /// Создаёт файл.
    pub fn file(name: &str, data: &[u8]) -> Self {
        Self {
            name: name.into(),
            node_type: FsNodeType::File,
            data: data.into(),
            children: Mutex::new(Vec::new()),
        }
    }

    /// Создаёт каталог.
    pub fn dir(name: &str) -> Self {
        Self {
            name: name.into(),
            node_type: FsNodeType::Directory,
            data: Box::new([]),
            children: Mutex::new(Vec::new()),
        }
    }

    /// Добавляет дочерний узел в каталог.
    pub fn add_child(&self, child: Box<FsNode>) {
        self.children.lock().push(child);
    }

    /// Возвращает размер файла.
    pub fn size(&self) -> usize {
        self.data.len()
    }
}

/// Корневой каталог VFS.
static ROOT: Mutex<Option<FsNode>> = Mutex::new(None);

/// Инициализирует корневой каталог VFS.
fn init_root() {
    let mut root = ROOT.lock();
    if root.is_none() {
        *root = Some(FsNode::dir("/"));
    }
}

/// Ищет узел по пути (например "/bin/hello" или "hello").
pub fn lookup(path: &str) -> Option<FsNode> {
    init_root();
    
    if path.is_empty() || path == "/" {
        return Some(ROOT.lock().as_ref()?.clone());
    }

    // Начинаем с корневого узла
    let mut node = ROOT.lock().as_ref()?.clone();
    
    for comp in path.split('/').filter(|c| !c.is_empty()) {
        if node.node_type != FsNodeType::Directory {
            return None;
        }
        // Блокируем children, ищем, сразу разблокируем
        let found = {
            let children = node.children.lock();
            children.iter().find(|c| c.name == comp).cloned()
        };
        match found {
            Some(child) => node = *child,
            None => return None,
        }
    }
    
    Some(node)
}

/// Возвращает содержимое каталога (список дочерних узлов).
pub fn read_dir(path: &str) -> Option<Vec<Box<FsNode>>> {
    let node = lookup(path)?;
    if node.node_type == FsNodeType::Directory {
        Some(node.children.lock().clone())
    } else {
        None
    }
}

/// Читает файл по пути. Возвращает данные файла или None.
pub fn read_file(path: &str) -> Option<Box<[u8]>> {
    let node = lookup(path)?;
    if node.node_type == FsNodeType::File {
        Some(node.data)
    } else {
        None
    }
}

/// Монтирует корневой узел (заполняет ROOT).
pub fn mount_root(children: Vec<Box<FsNode>>) {
    init_root();
    let mut root = ROOT.lock();
    if let Some(node) = root.as_mut() {
        node.children.lock().extend(children);
    }
}
