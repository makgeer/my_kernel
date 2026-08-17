#!/bin/bash

echo "====================================="
echo "  Rust Kernel Debug Build & Run"
echo "====================================="

# 1. Очистка
echo "[1/6] Cleaning old builds..."
cargo clean

# 2. Сборка
echo "[2/6] Building kernel (release)..."
cargo build --release

if [ $? -ne 0 ]; then
    echo "[ERROR] Build failed!"
    exit 1
fi
echo "[OK] Build successful"

# 3. Подготовка ISO
echo "[3/6] Preparing ISO structure..."
mkdir -p iso/boot/grub

# Копируем ядро
cp target/x86_64-unknown-none/release/malinka iso/boot/malinka.bin
echo "[OK] Kernel copied"

# Создаём grub.cfg
cat > iso/boot/grub/grub.cfg << EOF
set timeout=0
set default=0

menuentry "My Rust Kernel" {
    multiboot2 /boot/malinka.bin
    boot
}
EOF
echo "[OK] GRUB config created"

# 4. Создание ISO
echo "[4/6] Creating ISO image..."
grub-mkrescue -o boot.iso iso 2>&1 | grep -v "xorriso"

if [ $? -ne 0 ]; then
    echo "[ERROR] ISO creation failed!"
    exit 1
fi
echo "[OK] ISO created: boot.iso"

# 5. Запуск Bochs (устаревший скрипт: использует GRUB, а не Limine)
#    Скрипт оставлен для истории. Актуальная сборка: make_iso.sh + bochsrc.bxrc.
echo "[5/6] Starting Bochs..."
echo "====================================="
echo "  KERNEL LOG OUTPUT:"
echo "====================================="
echo ""

./bochs-bin/bin/bochs -q -f bochsrc.bxrc

# 6. После завершения Bochs
echo ""
echo "====================================="
echo "[6/6] Bochs exited"
echo "====================================="
