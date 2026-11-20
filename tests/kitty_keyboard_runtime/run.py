#!/usr/bin/env python3
"""Public CLI/attach regression using native PTYs and pinned expected bytes.

No terminal capability is inferred from injected acquired frames. Run with an
absolute rmux binary and a fresh durable audit output directory. No tmux parity
claim: Kitty is measured against the committed specification population.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import shlex
import struct
import subprocess
import sys
import termios
import threading
import time
import uuid


def wait(predicate, label, timeout=6):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = predicate()
        if value:
            return value
        time.sleep(0.01)
    raise AssertionError(f"timeout: {label}")


class Client:
    def __init__(self, run):
        self.fd, slave = pty.openpty()
        self.tty = os.ttyname(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        self.p = subprocess.Popen(run.prefix + ["attach-session", "-t", "kitty"], env=run.env,
                                  stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        self.raw, self.stop = bytearray(), False
        self.thread = threading.Thread(target=self.drain, daemon=True)
        self.thread.start()
        run.clients.append(self)
        wait(lambda: self.tty in run.q("list-clients", "-F", "#{client_name}"), "attached client")

    def drain(self):
        while not self.stop and len(self.raw) < 8 * 1024 * 1024:
            try:
                if select.select([self.fd], [], [], 0.05)[0]:
                    data = os.read(self.fd, 65536)
                    if not data:
                        break
                    self.raw.extend(data)
            except OSError:
                break

    def close(self):
        if self.p.poll() is None:
            self.p.terminate()
            try:
                self.p.wait(timeout=1)
            except subprocess.TimeoutExpired:
                self.p.kill()
                self.p.wait(timeout=1)
        self.stop = True
        self.thread.join(timeout=1)
        os.close(self.fd)


class Run:
    def __init__(self, binary, output):
        self.binary, self.output = binary.resolve(), output.resolve()
        self.output.mkdir(parents=True, exist_ok=False)
        self.label = "kitty" + uuid.uuid4().hex[:10]
        self.prefix = [str(self.binary), "-L", self.label, "-f", "/dev/null"]
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(("TMUX", "RMUX"))}
        self.env.update(TERM="xterm-256color", RMUX_DISABLE_TMUX_FALLBACK="1", LC_ALL="C.UTF-8")
        self.clients, self.steps = [], {"a": 0, "b": 0}
        self.receipt = dict(binary=str(self.binary), binary_sha256=hashlib.sha256(self.binary.read_bytes()).hexdigest(),
                            commands=[], exchanges=[], label=self.label, passed=False)

    def q(self, *args, check=True):
        p = subprocess.run(self.prefix + list(args), env=self.env, capture_output=True, timeout=8)
        self.receipt["commands"].append(dict(argv=list(args), code=p.returncode, stdout=p.stdout.decode(errors="replace"), stderr=p.stderr.decode(errors="replace")))
        if check:
            assert p.returncode == 0, self.receipt["commands"][-1]
        return p.stdout.decode().strip()

    def child(self, name):
        directory = self.output / name
        directory.mkdir()
        return shlex.join([sys.executable, str(Path(__file__).with_name("child.py").resolve()), str(directory)])

    def exchange(self, name, output=b"", expected=b"", client=None, acquired=b""):
        step = self.steps[name]
        root = self.output / name
        tmp = root / "request.tmp"
        tmp.write_text(json.dumps(dict(output=output.hex(), expected=expected.hex())))
        tmp.rename(root / f"{step}.request.json")
        wait(lambda: (root / f"{step}.ready").exists(), "child ready")
        if acquired:
            os.write(client.fd, acquired)
        wait(lambda: (root / f"{step}.result.json").exists(), "child result")
        result = json.loads((root / f"{step}.result.json").read_text())
        self.receipt["exchanges"].append(dict(pane=name, step=step, acquired=acquired.hex(), **result))
        assert result["passed"], result
        self.steps[name] += 1

    def execute(self):
        self.q("new-session", "-d", "-s", "kitty", "-x", "80", "-y", "24", self.child("a"))
        self.receipt["server"] = self.q("display-message", "-p", "#{pid}|#{socket_path}|#{version}")
        self.q("set-option", "-t", "kitty", "status", "off")
        self.q("split-window", "-h", "-t", "kitty:0.0", self.child("b"))
        for name in self.steps:
            wait(lambda: (self.output / name / "pid").exists(), "native child")
        self.exchange("a", b"\x1b[?u\x1b[=10u\x1b[?u", b"\x1b[?0u\x1b[?10u")
        self.exchange("b", b"\x1b[=8u\x1b[?u", b"\x1b[?8u")
        self.q("select-pane", "-t", "kitty:0.0")
        client = Client(self)
        self.exchange("a", expected=b"\x1b[97u", client=client, acquired=b"\x1b[97u")
        self.q("select-pane", "-t", "kitty:0.1")
        self.exchange("a", expected=b"\x1b[97;1:2u\x1b[97;1:3u", client=client, acquired=b"\x1b[97;1:2u\x1b[97;1:3u")
        self.exchange("b")
        self.exchange("b", expected=b"\x1b[98u", client=client, acquired=b"\x1b[98u")
        self.exchange("b", b"\x1b[>31u\x1b[?u", b"\x1b[?31u")
        self.exchange("b", expected=b"\x1b[97:65:113;2;65u", client=client, acquired=b"\x1b[97:65:113;2;65u")
        self.exchange("b", b"\x1b[>4;2m\x1b[<99u\x1b[?u", b"\x1b[?0u")
        self.exchange("b", expected=b"\x1b[27;2;13~", client=client, acquired=b"\x1b[13;2u")
        self.exchange("b", b"\x1b[>4;0m")
        self.exchange("b", expected=b"\n", client=client, acquired=b"\x1b[13;2u")
        self.q("detach-client", "-t", client.tty)
        assert client.p.wait(timeout=3) == 0
        self.q("select-pane", "-t", "kitty:0.0")
        again = Client(self)
        self.exchange("a", b"\x1b[?u", b"\x1b[?10u")
        self.exchange("a", client=again, acquired=b"\x1b[97;1:3u")
        self.exchange("a", expected=b"\x1b[101u\x1b[101;1:3u", client=again, acquired=b"\x1b[101u\x1b[101;1:3u")
        self.exchange("a", b"\x1b[?1049h\x1b[?u\x1b[=31u\x1b[?1049l\x1b[?u", b"\x1b[?0u\x1b[?10u")
        self.q("set-option", "-s", "escape-time", "50")
        self.q("bind-key", "-n", "u", "send-keys", "-l", "BOUND")
        # The empty exchange observes 200 ms of silence: the actual attach
        # scheduler must expire this quarantined frame within that interval.
        self.exchange("a", client=again, acquired=b"\x1b[97;x")
        self.exchange("a", expected=b"BOUND", client=again, acquired=b"u")
        self.q("unbind-key", "-n", "u")
        self.q("detach-client", "-t", again.tty)
        assert again.p.wait(timeout=3) == 0
        self.receipt["passed"] = True

    def close(self):
        self.q("kill-server", check=False)
        for index, client in enumerate(self.clients):
            client.close()
            (self.output / f"client-{index}.bin").write_bytes(client.raw)
            assert not re.search(rb"\x1b\[[>=][0-9;]*u", client.raw), "pane negotiation escaped to outer client"
        child_pids = [int(p.read_text()) for p in self.output.glob("*/pid")]
        def absent(pid):
            try:
                os.kill(pid, 0)
                return False
            except ProcessLookupError:
                return True
        wait(lambda: all(absent(pid) for pid in child_pids), "owned pane processes exited")
        socket = self.receipt.get("server", "").split("|")
        if len(socket) == 3:
            wait(lambda: not Path(socket[1]).exists(), "owned socket removed")
        self.receipt["cleanup"] = dict(clients_exited=all(c.p.poll() is not None for c in self.clients), child_pids=child_pids, children_exited=True)
        (self.output / "RESULT.json").write_text(json.dumps(self.receipt, indent=2))


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    args = p.parse_args()
    run = Run(args.binary, args.output)
    try:
        run.execute()
    finally:
        run.close()
    print(json.dumps(dict(passed=run.receipt["passed"], exchanges=len(run.receipt["exchanges"]))))


if __name__ == "__main__":
    main()
