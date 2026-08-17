#!/bin/bash

cd /home/makgeer/my_kernel

# Cargo установлен через rustup в ~/.cargo/bin, но в Flatpak-VSCode
# PATH не содержит этой папки. Явно добавляем её в PATH.
export PATH="$HOME/.cargo/bin:$PATH"

echo "=== Building kernel ==="
# Режим сборки: по умолчанию с framebuffer (fb).
# Для Bochs term (ncurses, нет 32bpp) — без framebuffer:
#   ./make_iso.sh no-fb
# Текстовый VGA 80x25 + serial, term-дисплей работает.
if [ "$1" = "no-fb" ]; then
    echo "Mode: no-fb (текстовый VGA, для Bochs term)"
    cargo build --release --no-default-features
else
    echo "Mode: fb (по умолчанию, графический framebuffer)"
    cargo build --release
fi

if [ $? -ne 0 ]; then
    echo "Build failed!"
    exit 1
fi

echo "=== Copying kernel ==="
cp target/x86_64-unknown-none/release/malinka iso/boot/malinka.bin

echo "=== Copying Limine files ==="
cp limine-binary/limine-bios.sys iso/
cp limine-binary/limine-bios-cd.bin iso/limine-cd.bin

echo "=== Creating ISO ==="
xorriso -as mkisofs -b limine-cd.bin \
    -no-emul-boot -boot-load-size 4 -boot-info-table \
    iso -o boot.iso

echo "=== Done ==="
echo "ISO created: boot.iso"
echo "Run manually with your QEMU command"
# Проверка
echo "=== ISO Structure ==="
ls -la iso/

# Создание ISO
XORRISO_BIN="/run/host/usr/local/bin/xorriso"
if [ ! -f "$XORRISO_BIN" ]; then
    echo "ERROR: xorriso not found at $XORRISO_BIN"
    exit 1
fi

$XORRISO_BIN -as mkisofs \
    -volid "RUSTKERNEL" \
    -joliet -rock \
    -isohybrid-mbr limine-binary/limine-bios-cd.bin \
    -c boot.catalog \
    -b limine-bios-cd.bin -no-emul-boot -boot-load-size 4 -boot-info-table \
    -graft-points \
    /limine.conf=iso/limine.conf \
    /kernel.elf=iso/kernel.elf \
    /limine-bios-cd.bin=iso/limine-bios-cd.bin \
    /limine-bios.sys=iso/limine-bios.sys \
    -o boot.iso

echo ""
echo "=== Installing Limine ==="
# Limine требует специальной установки
dd if=limine-binary/limine-bios-cd.bin of=boot.iso bs=2048 count=1 conv=notrunc 2>/dev/null || true

echo ""
echo "=== Final ISO ==="
ls -lh boot.iso
file boot.iso
isoinfo -R -f boot.iso | head -20
