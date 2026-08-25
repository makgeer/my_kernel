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

/// Создаёт файл в каталоге.
pub fn create_file(parent_path: &str, name: &str, data: &[u8]) -> bool {
    let parent = match lookup(parent_path) {
        Some(p) => p,
        None => return false,
    };
    if parent.node_type != FsNodeType::Directory {
        return false;
    }
    if parent.children.lock().iter().any(|c| c.name == name) {
        return false;
    }
    parent.add_child(Box::new(FsNode::file(name, data)));
    true
}

/// Создаёт каталог.
pub fn create_dir(parent_path: &str, name: &str) -> bool {
    let parent = match lookup(parent_path) {
        Some(p) => p,
        None => return false,
    };
    if parent.node_type != FsNodeType::Directory {
        return false;
    }
    if parent.children.lock().iter().any(|c| c.name == name) {
        return false;
    }
    parent.add_child(Box::new(FsNode::dir(name)));
    true
}

/// Удаляет файл/каталог.
pub fn remove(path: &str) -> bool {
    let (parent_path, name) = match path.rfind('/') {
        Some(pos) => (&path[..pos], &path[pos + 1..]),
        None => return false,
    };
    if name.is_empty() { return false; }
    
    let parent = match lookup(parent_path) {
        Some(p) => p,
        None => return false,
    };
    if parent.node_type != FsNodeType::Directory {
        return false;
    }
    
    let mut children = parent.children.lock();
    let idx = children.iter().position(|c| c.name == name);
    match idx {
        Some(i) => { children.remove(i); true }
        None => false,
    }
}

/// Копирует файл из src в dst.
pub fn copy(src_path: &str, dst_path: &str) -> bool {
    let src_data = match read_file(src_path) {
        Some(data) => data,
        None => return false,
    };
    let (dst_dir, dst_name) = match dst_path.rfind('/') {
        Some(pos) => (&dst_path[..pos], &dst_path[pos + 1..]),
        None => (".", dst_path),
    };
    create_file(dst_dir, dst_name, &src_data)
}

/// Перемещает/переименовывает файл/каталог.
pub fn rename(src_path: &str, dst_path: &str) -> bool {
    let src_data = read_file(src_path);
    let src_node = lookup(src_path);
    let is_dir = src_node.map(|n| n.node_type == FsNodeType::Directory);
    
    let (dst_dir, dst_name) = match dst_path.rfind('/') {
        Some(pos) => (&dst_path[..pos], &dst_path[pos + 1..]),
        None => (".", dst_path),
    };
    
    let removed = remove(src_path);
    if !removed { return false; }
    
    match is_dir {
        Some(true) => create_file(dst_dir, dst_name, &[]),
        _ => match src_data {
            Some(data) => create_file(dst_dir, dst_name, &data),
            None => false,
        },
    }
}

/// Записывает данные в файл (создаёт, если не существует).
pub fn write_file(path: &str, data: &[u8]) -> bool {
    let (parent_path, name) = match path.rfind('/') {
        Some(pos) => (&path[..pos], &path[pos + 1..]),
        None => return false,
    };
    if name.is_empty() { return false; }
    
    let parent = match lookup(parent_path) {
        Some(p) => p,
        None => return false,
    };
    if parent.node_type != FsNodeType::Directory {
        return false;
    }
    
    let mut children = parent.children.lock();
    if let Some(existing) = children.iter_mut().find(|c| c.name == name) {
        if existing.node_type == FsNodeType::File {
            existing.data = data.to_vec().into_boxed_slice();
            return true;
        }
        return false;
    }
    drop(children);
    create_file(parent_path, name, data)
}

/// Монтирует корневой узел (заполняет ROOT).
pub fn mount_root(children: Vec<Box<FsNode>>) {
    init_root();
    let mut root = ROOT.lock();
    if let Some(node) = root.as_mut() {
        node.children.lock().extend(children);
    }
}
