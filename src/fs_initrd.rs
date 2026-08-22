//! Модуль initrd — встроенный образ файловой системы.
//!
//! Вместо сложного tar-парсера используем прямое статическое дерево
//! VFS: каждый .elf файл из user_programs включается через include_bytes!
//! и регистрируется как узел FsNode в каталоге /bin.
//!
//! Это даёт «пространство пользователя»: shell может загружать программы
//! по имени (run hello.elf) вместо жёстко зашитых include_bytes в tests.rs.
//!
//! Позже (Фаза 2) заменится на настоящий tar/initrd, читаемый с диска.

/// Все user-программы, доступные через VFS в /bin.
static HELLO: &[u8] = include_bytes!("../user_programs/hello.elf");
static LOOP_A: &[u8] = include_bytes!("../user_programs/loop_a.elf");
static LOOP_B: &[u8] = include_bytes!("../user_programs/loop_b.elf");
static SYSCALL_TEST: &[u8] = include_bytes!("../user_programs/syscall_test.elf");
static IPC_SENDER: &[u8] = include_bytes!("../user_programs/ipc_sender.elf");
static IPC_RECEIVER: &[u8] = include_bytes!("../user_programs/ipc_receiver.elf");

use crate::fs::FsNode;

/// Статические узлы VFS.
static HELLO_NODE: FsNode = FsNode::file("hello.elf", HELLO);
static LOOP_A_NODE: FsNode = FsNode::file("loop_a.elf", LOOP_A);
static LOOP_B_NODE: FsNode = FsNode::file("loop_b.elf", LOOP_B);
static SYSCALL_TEST_NODE: FsNode = FsNode::file("syscall_test.elf", SYSCALL_TEST);
static IPC_SENDER_NODE: FsNode = FsNode::file("ipc_sender.elf", IPC_SENDER);
static IPC_RECEIVER_NODE: FsNode = FsNode::file("ipc_receiver.elf", IPC_RECEIVER);

/// Узлы каталога /bin.
static BIN_NODES: &[&FsNode] = &[
    &HELLO_NODE,
    &LOOP_A_NODE,
    &LOOP_B_NODE,
    &SYSCALL_TEST_NODE,
    &IPC_SENDER_NODE,
    &IPC_RECEIVER_NODE,
];

/// Каталог /bin.
static BIN_NODE: FsNode = FsNode::dir("bin", BIN_NODES);

/// Корневой каталог VFS: /bin с программами.
static ROOT_CHILDREN: &[&FsNode] = &[&BIN_NODE];

/// Монтирует initrd в VFS.
///
/// Вызывается один раз при старте ядра (до shell).
/// Заполняет корень VFS каталогом /bin с программами.
pub fn mount() {
    crate::fs::mount_root(ROOT_CHILDREN);
    crate::println!("initrd: mounted /bin ({} files)", BIN_NODES.len());
}

