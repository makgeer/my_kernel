//! Модуль initrd — встроенный образ файловой системы.
//!
//! Каждый .elf файл из user_programs включается через include_bytes!
//! и регистрируется как узел FsNode в каталоге /bin.

use alloc::boxed::Box;
use alloc::vec;
use crate::fs::FsNode;

/// Все user-программы, доступные через VFS в /bin.
static INIT: &[u8] = include_bytes!("../user_programs/init.elf");
static HELLO: &[u8] = include_bytes!("../user_programs/hello.elf");
static LOOP_A: &[u8] = include_bytes!("../user_programs/loop_a.elf");
static LOOP_B: &[u8] = include_bytes!("../user_programs/loop_b.elf");
static SYSCALL_TEST: &[u8] = include_bytes!("../user_programs/syscall_test.elf");
static IPC_SENDER: &[u8] = include_bytes!("../user_programs/ipc_sender.elf");
static IPC_RECEIVER: &[u8] = include_bytes!("../user_programs/ipc_receiver.elf");
static ARROW_TEST: &[u8] = include_bytes!("../user_programs/arrow_test.elf");
static SYSINFO: &[u8] = include_bytes!("../user_programs/sysinfo.elf");
static KEYBOARDD: &[u8] = include_bytes!("../user_programs/keyboardd.elf");
static SERIALD: &[u8] = include_bytes!("../user_programs/seriald.elf");

/// Создаёт каталог /bin с программами.
fn create_bin_dir() -> FsNode {
    let bin = FsNode::dir("bin");
    bin.add_child(Box::new(FsNode::file("init.elf", INIT)));
    bin.add_child(Box::new(FsNode::file("hello.elf", HELLO)));
    bin.add_child(Box::new(FsNode::file("loop_a.elf", LOOP_A)));
    bin.add_child(Box::new(FsNode::file("loop_b.elf", LOOP_B)));
    bin.add_child(Box::new(FsNode::file("syscall_test.elf", SYSCALL_TEST)));
    bin.add_child(Box::new(FsNode::file("ipc_sender.elf", IPC_SENDER)));
    bin.add_child(Box::new(FsNode::file("ipc_receiver.elf", IPC_RECEIVER)));
    bin.add_child(Box::new(FsNode::file("arrow_test.elf", ARROW_TEST)));
    bin.add_child(Box::new(FsNode::file("sysinfo.elf", SYSINFO)));
    bin.add_child(Box::new(FsNode::file("keyboardd.elf", KEYBOARDD)));
    bin.add_child(Box::new(FsNode::file("seriald.elf", SERIALD)));
    bin
}

/// Монтирует initrd в VFS.
pub fn mount() {
    let bin = create_bin_dir();
    crate::fs::mount_root(vec![Box::new(bin)]);
    crate::println!("initrd: mounted /bin ({} files)", 11);
}

