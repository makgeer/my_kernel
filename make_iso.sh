#!/bin/bash

# Переходим в каталог скрипта, а не в жёстко заданный путь:
# проект может быть склонирован в любое место.
cd "$(dirname "$0")"

# Cargo установлен через rustup в ~/.cargo/bin, но в Flatpak-VSCode
# PATH не содержит этой папки. Явно добавляем её в PATH.
export PATH="$HOME/.cargo/bin:$PATH"

# Cargo можно переопределить (например, CARGO="cargo +nightly"),
# если nightly не выбран toolchain по умолчанию.
CARGO="${CARGO:-cargo}"

echo "=== Building kernel ==="
# Режим сборки: по умолчанию с framebuffer (fb).
# Для Bochs term (ncurses, нет 32bpp) — без framebuffer:
#   ./make_iso.sh no-fb
# Текстовый VGA 80x25 + serial, term-дисплей работает.
if [ "$1" = "no-fb" ]; then
    echo "Mode: no-fb (текстовый VGA, для Bochs term)"
    $CARGO build --release --no-default-features
else
    echo "Mode: fb (по умолчанию, графический framebuffer)"
    $CARGO build --release
fi

if [ $? -ne 0 ]; then
    echo "Build failed!"
    exit 1
fi

echo "=== Copying kernel ==="
# Важно: limine.conf указывает KERNEL_PATH: boot():/kernel.elf,
# поэтому копируем ядро именно в iso/kernel.elf (не в iso/boot/malinka.bin).
cp target/x86_64-unknown-none/release/malinka iso/kernel.elf

echo "=== Copying Limine files ==="
cp limine-binary/limine-bios.sys iso/
cp limine-binary/limine-bios-cd.bin iso/limine-cd.bin

echo "=== Creating ISO ==="
# xorriso предпочтителен, но его может не быть в системе —
# genisoimage/mkisofs даёт совместимый ISO9660 + El Torito.
#
# КРИТИЧНО: обязательны -R (Rock Ridge) и -J (Joliet). Без них имена
# файлов в ISO9660 обрезаются/переводятся в ВЕРХНИЙ регистр
# (kernel.elf → KERNEL.ELF;1), и Limine не находит ни limine.conf,
# ни kernel.elf — загрузка останавливается молча.
if command -v xorriso >/dev/null 2>&1; then
    xorriso -as mkisofs -R -J -b limine-cd.bin \
        -no-emul-boot -boot-load-size 4 -boot-info-table \
        iso -o boot.iso
elif command -v genisoimage >/dev/null 2>&1; then
    # ВАЖНО: genisoimage требует -o ДО пути к каталогу.
    genisoimage -R -J -b limine-cd.bin \
        -no-emul-boot -boot-load-size 4 -boot-info-table \
        -o boot.iso iso
elif command -v mkisofs >/dev/null 2>&1; then
    mkisofs -R -J -b limine-cd.bin \
        -no-emul-boot -boot-load-size 4 -boot-info-table \
        -o boot.iso iso
else
    echo "ERROR: нужен xorriso, genisoimage или mkisofs"
    exit 1
fi

if [ $? -ne 0 ] || [ ! -f boot.iso ]; then
    echo "ERROR: не удалось создать boot.iso"
    exit 1
fi

echo "=== Done ==="
echo "ISO created: boot.iso"
echo "Run manually with Bochs: ./bochs-bin/bin/bochs -q -f bochsrc.bxrc"
# Проверка
echo "=== ISO Structure ==="
ls -la iso/
ls -lh boot.iso
