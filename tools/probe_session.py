#!/usr/bin/env python3
"""probe_session.py — сценарий команд через COM1 (Bochs nogui, без X11).

Запускает Bochs с bochsrc.bxrc (display_library: nogui), подключается
к socket-server COM1 (127.0.0.1:8888), выполняет шаги сценария по
порядку и сохраняет весь serial-вывод в файл. Экран не затрагивается:
это соответствует правилам 9 и 10 (.kodaruls).

Шаги (--send, повторяемый, порядок сохраняется):
  cmd:<текст>   отправить строку + Enter (команда shell)
  text:<текст>  отправить текст без Enter (ввод для драйвера)
  hex:<байты>   отправить байты, например hex:41 или hex:03
  pause:<сек>   пауза

Пример (воспроизведение #PF в seriald):
  python3 tools/probe_session.py \
      --send cmd:seriald --send pause:2 \
      --send hex:41 --send pause:3 --timeout 60

Результат: session_serial.log (весь вывод), session_report.txt (сводка).
"""

import os
import socket
import subprocess
import sys
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BOCHS = os.path.join(ROOT, "bochs-bin", "bin", "bochs")
CONF = "bochsrc.bxrc"
HOST = "127.0.0.1"
PORT = 8888
SERIAL_LOG = os.path.join(ROOT, "session_serial.log")
BOCHS_LOG = os.path.join(ROOT, "session_bochs.out")
REPORT = os.path.join(ROOT, "session_report.txt")

PROMPT = "Welcome to Malinka shell!"

# Маркеры, которые стоит отметить в сводке (не влияют на сценарий).
MARKERS = ("[PF]", "DOUBLE FAULT", "KERNEL PANIC", "[DF]", "seriald:")


def log(msg):
    print(msg, flush=True)


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
            time.sleep(0.2)
        return False

    def dump(self):
        with self.lock:
            data = bytes(self.buf)
        with open(SERIAL_LOG, "wb") as f:
            f.write(data)
        return len(data)


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


def parse_steps(argv):
    """Разбирает --send в список (тип, значение) с сохранением порядка."""
    steps = []
    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "--send" and i + 1 < len(argv):
            spec = argv[i + 1]
            if ":" not in spec:
                sys.exit(f"--send: ожидается тип:значение, получено {spec!r}")
            kind, value = spec.split(":", 1)
            if kind not in ("cmd", "text", "hex", "pause"):
                sys.exit(f"--send: неизвестный тип {kind!r}")
            steps.append((kind, value))
            i += 2
            continue
        i += 1
    return steps


def main():
    if "--send" not in sys.argv:
        log(__doc__)
        sys.exit(1)

    timeout = 120
    if "--timeout" in sys.argv:
        timeout = int(sys.argv[sys.argv.index("--timeout") + 1])
    settle = 1.5
    if "--settle" in sys.argv:
        settle = float(sys.argv[sys.argv.index("--settle") + 1])

    steps = parse_steps(sys.argv)
    proc = start_bochs()
    sock = connect_serial(time.time() + 60)
    reader = SerialReader(sock)
    reader.start()

    report = []
    try:
        ok = reader.wait_for(PROMPT, timeout)
        report.append(f"bochs pid: {proc.pid}")
        report.append(f"shell prompt: {'ДА' if ok else 'НЕТ'}")
        if not ok:
            report.append("--- последние строки serial ---")
            report.extend(reader.text().splitlines()[-25:])
        else:
            for kind, value in steps:
                if kind == "pause":
                    time.sleep(float(value))
                    continue
                if kind == "cmd":
                    payload = (value + "\n").encode()
                elif kind == "text":
                    payload = value.encode()
                else:  # hex
                    payload = bytes.fromhex(value.replace(" ", ""))
                try:
                    sock.sendall(payload)
                except OSError as e:
                    report.append(f"шаг {kind}:{value!r}: ошибка отправки: {e}")
                    break
                report.append(f"отправлено {kind}:{value!r}")
                time.sleep(settle)
            # Дать ядру доработать сценарий и собрать хвост лога.
            time.sleep(2)
            got = reader.text()
            found = [m for m in MARKERS if m in got]
            report.append("маркеры в логе: " + (", ".join(found) if found else "нет"))
            report.append("--- последние строки serial ---")
            report.extend(got.splitlines()[-30:])
    finally:
        reader.stop = True
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
        with open(REPORT, "w", encoding="utf-8") as f:
            f.write("\n".join(report) + "\n")
        log("\n".join(report))
        log(f"serial сохранён: {SERIAL_LOG} ({size} байт)")
        log(f"отчёт: {REPORT}")


if __name__ == "__main__":
    main()
