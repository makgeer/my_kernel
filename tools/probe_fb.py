#!/usr/bin/env python3
"""probe_fb.py — проверка VBE-framebuffer Bochs БЕЗ X11 и без экрана.

Как работает:
  1. Запускает Bochs с bochsrc.bxrc (display_library: nogui) — окно
     не создаётся, реальный экран не затрагивается.
  2. Подключается к COM1 (socket-server, localhost:8888), ждёт
     приглашение shell.
  3. Находит в памяти процесса Bochs буфер VBE (окно 0xE0000000,
     1024x768x32) и декодирует экран 80x25 по шрифту из src/video.rs.

Результат пишется в probe_report.txt, serial-вывод — в probe_serial.log.

Запуск: python3 tools/probe_fb.py [--timeout СЕК]
"""

import os
import re
import socket
import struct
import subprocess
import sys
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BOCHS = os.path.join(ROOT, "bochs-bin", "bin", "bochs")
CONF = "bochsrc.bxrc"
HOST = "127.0.0.1"
PORT = 8888
SERIAL_LOG = os.path.join(ROOT, "probe_serial.log")
BOCHS_LOG = os.path.join(ROOT, "probe_bochs.out")
REPORT = os.path.join(ROOT, "probe_report.txt")

# Параметры гостевого режима (из bochs-gui.log: VBE 1024x768x32).
FB_W = 1024
FB_H = 768
FB_PITCH = 4096          # байт на строку
FB_PHYS = 0xE0000000     # окно VBE в Bochs
FB_SIZE = 16 * 1024 * 1024  # VBE Bochs Display Extension Enabled (16 MB)

# Цвет текста терминала по умолчанию: PALETTE[7] = 0x00AAAAAA.
FG_DEFAULT = 0x00AAAAAA
# Паттерн пикселя в памяти (little-endian BGRX).
FG_PATTERN = struct.pack("<I", FG_DEFAULT)

TERM_COLS = 80
TERM_ROWS = 25
CELL_W = 9
CELL_H = 16


def log(msg):
    print(msg, flush=True)


def start_bochs():
    if not os.path.exists(BOCHS):
        sys.exit(f"нет бинарника Bochs: {BOCHS}")
    out = open(BOCHS_LOG, "wb")
    proc = subprocess.Popen(
        [BOCHS, "-q", "-unlock", "-f", CONF],
        cwd=ROOT,
        stdout=out,
        stderr=subprocess.STDOUT,
        stdin=subprocess.DEVNULL,
    )
    log(f"bochs запущен: pid={proc.pid}, конфиг={CONF} (nogui)")
    return proc


def connect_serial(deadline):
    """Подключается к socket-server COM1. Возвращает сокет."""
    last_err = None
    while time.time() < deadline:
        try:
            s = socket.create_connection((HOST, PORT), timeout=2)
            s.settimeout(1)
            log(f"COM1: подключено к {HOST}:{PORT}")
            return s
        except OSError as e:
            last_err = e
            time.sleep(0.5)
    sys.exit(f"не удалось подключиться к COM1 {HOST}:{PORT}: {last_err}")


class SerialReader(threading.Thread):
    """Читает COM1 в фоне и копит данные в buffer."""

    def __init__(self, sock):
        super().__init__(daemon=True)
        self.sock = sock
        self.buf = bytearray()
        self.lock = threading.Lock()
        self.stop = False

    def run(self):
        while not self.stop:
            try:
                data = self.sock.recv(4096)
                if not data:
                    break
                with self.lock:
                    self.buf.extend(data)
            except socket.timeout:
                continue
            except OSError:
                break

    def text(self):
        with self.lock:
            return self.buf.decode("utf-8", "replace")

    def wait_for(self, marker, timeout):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if marker in self.text():
                return True
            time.sleep(0.25)
        return False

    def dump(self):
        with self.lock:
            data = bytes(self.buf)
        with open(SERIAL_LOG, "wb") as f:
            f.write(data)
        return len(data)


def read_maps(pid):
    """Список (start, end, perms, path) из /proc/<pid>/maps."""
    regions = []
    with open(f"/proc/{pid}/maps", "r") as f:
        for line in f:
            parts = line.split()
            if len(parts) < 2:
                continue
            rng, perms = parts[0], parts[1]
            if "r" not in perms:
                continue
            start_s, end_s = rng.split("-")
            start, end = int(start_s, 16), int(end_s, 16)
            path = parts[5] if len(parts) > 5 else ""
            regions.append((start, end, perms, path))
    return regions


def read_region(pid, start, size):
    """Читает size байт из памяти процесса по адресу start."""
    with open(f"/proc/{pid}/mem", "rb", buffering=0) as mem:
        mem.seek(start)
        return mem.read(size)


def scan_pattern(pid, start, size, pattern, limit=50000):
    """Сканирует регион чанками и возвращает смещения pattern."""
    offs = []
    chunk = 8 * 1024 * 1024
    overlap = len(pattern) - 1
    pos = 0
    while pos < size and len(offs) < limit:
        n = min(chunk, size - pos)
        try:
            data = read_region(pid, start + pos, n)
        except OSError:
            break
        idx = data.find(pattern)
        while idx != -1 and len(offs) < limit:
            offs.append(pos + idx)
            idx = data.find(pattern, idx + 4)
        pos += n - overlap
        if n <= overlap:
            break
    return offs


def cluster_offsets(offs, gap=2 * 1024 * 1024):
    """Группирует смещения в кластеры; возвращает крупнейший."""
    if not offs:
        return []
    offs = sorted(offs)
    best = []
    cur = [offs[0]]
    for off in offs[1:]:
        if off - cur[-1] <= gap:
            cur.append(off)
        else:
            if len(cur) > len(best):
                best = cur
            cur = [off]
    if len(cur) > len(best):
        best = cur
    return best


def find_fb_buffer(pid):
    """Ищет буфер VBE в памяти процесса Bochs.

    Возвращает (region_start, delta, data, size) или None.
    delta — смещение начала framebuffer внутри региона.
    """
    regions = read_maps(pid)
    candidates = []
    for start, end, perms, path in regions:
        size = end - start
        if size < 4 * 1024 * 1024:
            continue
        if path and not path.startswith("["):
            continue
        candidates.append((start, end))

    log(f"сканирую регионы: {len(candidates)}")

    for start, end in candidates:
        size = end - start
        offs = scan_pattern(pid, start, size, FG_PATTERN)
        if len(offs) < 100:
            continue
        cluster = cluster_offsets(offs)
        log(f"  регион 0x{start:x} ({size / 1048576:.1f} MB): "
            f"пикселей={len(offs)}, крупнейший кластер={len(cluster)}")
        if len(cluster) < 100:
            continue

        # Начало буфера не может быть раньше base_min: первый пиксель
        # текста лежит в последней ячейке области 80x25.
        first = cluster[0]
        base_min = max(
            0,
            first - 4 * ((TERM_ROWS * CELL_H - 1) * FB_W
                         + (TERM_COLS * CELL_W - 1)),
        )
        read_len = min(size - base_min,
                       3 * 1024 * 1024 + 4 * TERM_ROWS * CELL_H * FB_W)
        data = read_region(pid, start + base_min, read_len)

        delta = locate_fb_base(cluster, data, base_min)
        if delta is None:
            continue
        local = delta - base_min
        log(f"  framebuffer найден: регион 0x{start:x} + 0x{delta:x}")
        return start, local, data

    return None


def dump_regions(pid):
    """Печатает крупные регионы памяти Bochs и статистику пикселей."""
    regions = read_maps(pid)
    rows = []
    for start, end, perms, path in regions:
        size = end - start
        if size < 1024 * 1024:
            continue
        rows.append((size, start, end, perms, path))
    rows.sort(reverse=True)
    log(f"регионов >= 1 МБ: {len(rows)}")
    for size, start, end, perms, path in rows:
        line = (f"0x{start:012x}-0x{end:012x} "
                f"{size / 1048576:8.1f} MB {perms} {path}")
        try:
            data = read_region(pid, start, min(size, 16 * 1024 * 1024))
        except OSError as e:
            log(line + f"  [read err: {e}]")
            continue
        fg = data.count(FG_PATTERN)
        white = data.count(struct.pack("<I", 0x00FFFFFF))
        line += f"  fg7={fg} white={white}"
        log(line)


def candidate_deltas(offs):
    """Возможные смещения начала буфера.

    Отсев: пиксели текста лежат внутри области 80x25 ячеек и не
    попадают в разделительную колонку между ячейками (x % 9 == 8).
    """
    probe = offs[:300]
    first = probe[0]
    cands = set()
    for y in range(0, TERM_ROWS * CELL_H):
        for x in range(0, TERM_COLS * CELL_W):
            delta = first - 4 * (y * FB_W + x)
            if delta < 0 or delta % 4:
                continue
            ok = True
            for off in probe[:80]:
                idx = (off - delta) // 4
                if idx < 0:
                    ok = False
                    break
                px = idx % FB_W
                if px >= TERM_COLS * CELL_W:
                    ok = False
                    break
                if idx // FB_W >= TERM_ROWS * CELL_H:
                    ok = False
                    break
                if px % CELL_W == CELL_W - 1:
                    ok = False
                    break
            if ok:
                cands.add(delta)
    return sorted(cands)


def cell_bits(data, base, r, c):
    """Возвращает 16 байт глифа ячейки (r, c) как hex-строку."""
    x0, y0 = c * CELL_W, r * CELL_H
    out = bytearray()
    for row in range(CELL_H):
        b = 0
        for col in range(8):
            off = base + (y0 + row) * FB_PITCH + (x0 + col) * 4
            if struct.unpack_from("<I", data, off)[0] == FG_DEFAULT:
                b |= 1 << (7 - col)
        out.append(b)
    return out.hex()


def score_delta(data, base, rows=3):
    """Оценка смещения: распознанные глифы + разнообразие символов.

    Ложное смещение часто даёт одинаковую горизонтальную черту во всех
    ячейках, поэтому разнообразие важно не меньше количества.
    """
    lines = decode_screen(data, base, rows)
    text = "".join(lines)
    recognized = sum(1 for ch in text if ch != "?")
    distinct = len(set(text))
    return recognized + distinct * 5


def locate_fb_base(offs, data, origin):
    """Выбирает смещение начала буфера по лучшему распознаванию глифов.

    offs — глобальные смещения в регионе, origin — смещение data[0].
    Возвращает глобальное смещение начала framebuffer.
    """
    cands = candidate_deltas(offs)
    log(f"  кандидатов начала буфера: {len(cands)}")
    # При большом числе кандидатов оцениваем по одной строке.
    rows = 3 if len(cands) <= 400 else 1
    best = None
    best_score = -1
    for delta in cands:
        local = delta - origin
        if local < 0 or local + FB_PITCH * FB_H > len(data):
            continue
        score = score_delta(data, local, rows)
        if score > best_score:
            best_score = score
            best = delta
    if best is not None:
        log(f"  лучшее смещение 0x{best:x}, распознано глифов: {best_score}")
    return best


def load_font():
    """Читает FONT_DATA (8x16, 95 символов) из src/video.rs."""
    path = os.path.join(ROOT, "src", "video.rs")
    with open(path, "r", encoding="utf-8") as f:
        src = f.read()
    m = re.search(r"const FONT_DATA.*?=\s*\[(.*?)\n\];", src, re.S)
    if not m:
        return []
    glyphs = []
    for row in re.findall(r"&\[([^\]]+)\]", m.group(1)):
        vals = [int(v.strip(), 0) for v in row.split(",") if v.strip()]
        if len(vals) == 16:
            glyphs.append(bytes(vals))
    return glyphs


_FONT_INDEX = None


def font_index():
    """Кэш: байты глифа -> символ (шрифт читается один раз)."""
    global _FONT_INDEX
    if _FONT_INDEX is None:
        _FONT_INDEX = {g: chr(32 + i) for i, g in enumerate(load_font())}
    return _FONT_INDEX


def decode_screen(data, base, rows=TERM_ROWS):
    """Декодирует экран 80x25 (или первые rows строк) из framebuffer."""
    index = font_index()

    def pixel(x, y):
        off = base + y * FB_PITCH + x * 4
        return struct.unpack_from("<I", data, off)[0]

    lines = []
    for r in range(rows):
        chars = []
        for c in range(TERM_COLS):
            x0, y0 = c * CELL_W, r * CELL_H
            bits = bytearray()
            for row in range(CELL_H):
                b = 0
                for col in range(8):
                    b = (b << 1) | (1 if pixel(x0 + col, y0 + row) == FG_DEFAULT else 0)
                bits.append(b)
            ch = index.get(bytes(bits), "?")
            if ch == "?":
                # Инвертированная ячейка (курсор): фон цвета текста.
                inv = bytes(0xFF ^ v for v in bits)
                ch = index.get(inv, "?")
            chars.append(ch)
        lines.append("".join(chars).rstrip())
    return lines


def main():
    timeout = 300
    if "--timeout" in sys.argv:
        timeout = int(sys.argv[sys.argv.index("--timeout") + 1])

    proc = start_bochs()
    sock = connect_serial(time.time() + 60)
    reader = SerialReader(sock)
    reader.start()

    try:
        ok = reader.wait_for("Welcome to Malinka shell!", timeout)
        got = reader.text()
        report = []
        report.append(f"bochs pid: {proc.pid}")
        report.append(f"shell prompt: {'ДА' if ok else 'НЕТ'}")

        if "Framebuffer found:" in got:
            line = [l for l in got.splitlines() if "Framebuffer found:" in l][0]
            report.append(line.strip())
        else:
            report.append("Framebuffer found: НЕТ (ISO собран без fb?)")

        if ok:
            time.sleep(2)  # дать терминалу дорисовать экран
            if "--serial-only" in sys.argv:
                # Только serial-лог: без сканирования памяти и декодера.
                found = None
                report.append("режим --serial-only: память не сканируется")
            elif "--dump-maps" in sys.argv:
                dump_regions(proc.pid)
                found = None
                report.append("режим --dump-maps: регионы выведены в stdout")
            else:
                found = find_fb_buffer(proc.pid)
            if found is None:
                report.append("VBE-буфер в памяти Bochs: НЕ НАЙДЕН")
                report.append("=> ядро не пишет в framebuffer (адрес/маппинг)")
            else:
                start, delta, data = found
                report.append(f"VBE-буфер: регион 0x{start:x}, delta 0x{delta:x}")
                lines = decode_screen(data, delta)
                report.append("--- hex глифов (r,c) ---")
                for (r, c) in [(0, 0), (0, 1), (0, 4), (1, 0), (2, 0)]:
                    report.append(f"({r},{c}): {cell_bits(data, delta, r, c)}")
                report.append("--- экран, декодированный из видеопамяти ---")
                for i, line in enumerate(lines):
                    if line:
                        report.append(f"{i:02d}| {line}")
                report.append("--- конец экрана ---")
                if any("Malinka" in l or "KERNEL" in l for l in lines):
                    report.append("=> текст на экране ЕСТЬ: ядро пишет в fb корректно")
                else:
                    report.append("=> буфер найден, но текст не распознан")
        else:
            report.append("--- последние строки serial ---")
            report.extend(got.splitlines()[-20:])

        with open(REPORT, "w", encoding="utf-8") as f:
            f.write("\n".join(report) + "\n")
        log("\n".join(report))
    finally:
        reader.stop = True
        try:
            sock.sendall(b"exit\n")
            time.sleep(1)
        except OSError:
            pass
        try:
            sock.close()
        except OSError:
            pass
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
        size = reader.dump()
        log(f"serial сохранён: {SERIAL_LOG} ({size} байт)")
        log(f"отчёт: {REPORT}")


if __name__ == "__main__":
    main()
