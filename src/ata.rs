//! ATA PIO драйвер — чтение/запись секторов с диска.
//!
//! Работает с первичным ATA-каналом (Primary, порты 0x1F0-0x1F7).
//! Режим PIO (Programmed I/O): CPU сам читает данные через порты.
//!
//! Сектор = 512 байт. LBA28 (28-битная адресация, до 128 ГБ).
//!
//! Используется BTRFS-модулем (src/btrfs.rs) для чтения блочных
//! данных с диска.

use x86_64::instructions::port::{Port, PortRead, PortWrite};

/// База портов первичного ATA-канала.
const ATA_PRIMARY_BASE: u16 = 0x1F0;
/// База портов вторичного ATA-канала (для btrfs-диска).
const ATA_SECONDARY_BASE: u16 = 0x170;

/// Текущий выбранный канал ATA.
static mut CURRENT_BASE: u16 = ATA_PRIMARY_BASE;

/// Выбирает канал ATA (primary/secondary).
pub fn select_channel(base: u16) {
    unsafe { CURRENT_BASE = base; }
}

/// Регистры ATA (смещения от базы).
const REG_DATA: u16 = 0x000;          // данные
const REG_SECTOR_COUNT: u16 = 0x002;  // количество секторов
const REG_LBA_LO: u16 = 0x003;        // LBA 0-7
const REG_LBA_MID: u16 = 0x004;       // LBA 8-15
const REG_LBA_HI: u16 = 0x005;        // LBA 16-23
const REG_DRIVE: u16 = 0x006;         // drive + LBA 24-27
const REG_COMMAND: u16 = 0x007;       // команда
const REG_STATUS: u16 = 0x007;        // статус

/// Команды ATA.
const CMD_READ_SECTORS: u8 = 0x20;
const CMD_WRITE_SECTORS: u8 = 0x30;
const CMD_IDENTIFY: u8 = 0xEC;

/// Биты статуса.
const STATUS_BSY: u8 = 0x80;
const STATUS_DRQ: u8 = 0x08;
const STATUS_ERR: u8 = 0x01;
const STATUS_DF: u8 = 0x20;

/// Возвращает базу текущего канала.
fn base() -> u16 {
    unsafe { CURRENT_BASE }
}

/// Ждёт, пока контроллер не будет готов (не Busy).
fn wait_not_busy() -> bool {
    let mut port = Port::<u8>::new(base() + REG_STATUS);
    for _ in 0..100_000 {
        let status = unsafe { port.read() };
        if status & STATUS_BSY == 0 {
            return true;
        }
        for _ in 0..100 {
            unsafe { core::arch::asm!("nop"); }
        }
    }
    false
}

/// Ждёт готовности к передаче данных (DRQ).
fn wait_drq() -> bool {
    let mut port = Port::<u8>::new(base() + REG_STATUS);
    for _ in 0..100_000 {
        let status = unsafe { port.read() };
        if status & STATUS_BSY == 0 {
            return status & STATUS_DRQ != 0;
        }
        for _ in 0..100 {
            unsafe { core::arch::asm!("nop"); }
        }
    }
    false
}

/// Читает один сектор (512 байт) по LBA28.
///
/// Возвращает true при успехе.
pub fn read_sector(lba: u32, buffer: &mut [u8; 512]) -> bool {
    if !wait_not_busy() {
        return false;
    }

    let b = base();
    let mut drive = Port::<u8>::new(b + REG_DRIVE);
    let mut count = Port::<u8>::new(b + REG_SECTOR_COUNT);
    let mut lo = Port::<u8>::new(b + REG_LBA_LO);
    let mut mid = Port::<u8>::new(b + REG_LBA_MID);
    let mut hi = Port::<u8>::new(b + REG_LBA_HI);
    let mut cmd = Port::<u8>::new(b + REG_COMMAND);

    unsafe {
        drive.write(0xE0 | ((lba >> 24) & 0x0F) as u8);
        count.write(1);
        lo.write((lba & 0xFF) as u8);
        mid.write(((lba >> 8) & 0xFF) as u8);
        hi.write(((lba >> 16) & 0xFF) as u8);
        cmd.write(CMD_READ_SECTORS);
    }

    if !wait_drq() {
        return false;
    }

    let mut data = Port::<u16>::new(b + REG_DATA);
    for i in 0..256 {
        let word = unsafe { data.read() };
        buffer[i * 2] = (word & 0xFF) as u8;
        buffer[i * 2 + 1] = (word >> 8) as u8;
    }

    let mut status = Port::<u8>::new(b + REG_STATUS);
    let st = unsafe { status.read() };
    st & (STATUS_ERR | STATUS_DF) == 0
}

/// Читает несколько секторов подряд.
pub fn read_sectors(lba: u32, buffer: &mut [u8]) -> bool {
    let n = buffer.len() / 512;
    for i in 0..n {
        let mut sector = [0u8; 512];
        if !read_sector(lba + i as u32, &mut sector) {
            return false;
        }
        buffer[i * 512..(i + 1) * 512].copy_from_slice(&sector);
    }
    true
}

/// Записывает один сектор (512 байт) по LBA28.
pub fn write_sector(lba: u32, buffer: &[u8; 512]) -> bool {
    if !wait_not_busy() {
        return false;
    }

    let b = base();
    let mut drive = Port::<u8>::new(b + REG_DRIVE);
    let mut count = Port::<u8>::new(b + REG_SECTOR_COUNT);
    let mut lo = Port::<u8>::new(b + REG_LBA_LO);
    let mut mid = Port::<u8>::new(b + REG_LBA_MID);
    let mut hi = Port::<u8>::new(b + REG_LBA_HI);
    let mut cmd = Port::<u8>::new(b + REG_COMMAND);

    unsafe {
        drive.write(0xE0 | ((lba >> 24) & 0x0F) as u8);
        count.write(1);
        lo.write((lba & 0xFF) as u8);
        mid.write(((lba >> 8) & 0xFF) as u8);
        hi.write(((lba >> 16) & 0xFF) as u8);
        cmd.write(CMD_WRITE_SECTORS);
    }

    if !wait_drq() {
        return false;
    }

    let mut data = Port::<u16>::new(b + REG_DATA);
    for i in 0..256 {
        let word = (buffer[i * 2] as u16) | ((buffer[i * 2 + 1] as u16) << 8);
        unsafe { data.write(word); }
    }

    wait_not_busy()
}

/// IDENTIFY DEVICE — проверка наличия диска на текущем канале.
pub fn identify() -> bool {
    if !wait_not_busy() {
        return false;
    }

    let b = base();
    let mut drive = Port::<u8>::new(b + REG_DRIVE);
    unsafe { drive.write(0xA0); } // drive 0 master

    for _ in 0..1000 { unsafe { core::arch::asm!("nop"); } }

    let mut status = Port::<u8>::new(b + REG_STATUS);
    let st = unsafe { status.read() };
    if st == 0 {
        return false;
    }

    let mut cmd = Port::<u8>::new(b + REG_COMMAND);
    unsafe { cmd.write(CMD_IDENTIFY); }

    if !wait_drq() {
        return false;
    }

    let mut data = Port::<u16>::new(b + REG_DATA);
    for _ in 0..256 {
        unsafe { data.read(); }
    }
    true
}

/// Инициализирует ATA: проверяет наличие диска.
pub fn init() -> bool {
    // Проверяем primary (CD-ROM) и secondary (btrfs-диск)
    let primary = {
        select_channel(ATA_PRIMARY_BASE);
        identify()
    };
    let secondary = {
        select_channel(ATA_SECONDARY_BASE);
        identify()
    };
    if primary {
        crate::println!("ATA: primary channel drive 0 present (CD-ROM)");
    }
    if secondary {
        crate::println!("ATA: secondary channel drive 0 present (btrfs disk)");
        // По умолчанию работаем с secondary (btrfs)
        select_channel(ATA_SECONDARY_BASE);
    } else {
        crate::println!("ATA: no btrfs disk on secondary channel");
        select_channel(ATA_PRIMARY_BASE);
    }
    primary || secondary
}
