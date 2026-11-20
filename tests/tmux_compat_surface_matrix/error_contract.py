#!/usr/bin/env python3
"""Compare error channels and queue recovery with a freshly identified tmux 3.7b."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import select
import subprocess
import time


CASES = {
    "success": ["display-message", "-p", "L075_OK"],
    "literal-queued": ["display-message", "-p", "-t", "errors:0.0", "-:1: literal output"],
    "runtime-error": ["wait-for", "-U", "contract-unlocked"],
    "target-error": ["kill-session", "-t", "=missing"],
    "option-error": ["set-option", "-g", "status", "invalid"],
    "parse-error": ["nonexistent-command-xyz"],
    "sequence": ["set-option", "-g", "@before", "yes", ";", "wait-for", "-U",
                 "contract-unlocked", ";", "set-option", "-g", "@after", "bad"],
}


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def capture(argv, env, cwd, stdin=None):
    started = time.time_ns()
    child = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.PIPE,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    pid = child.pid
    try:
        stdout, stderr = child.communicate(stdin, timeout=20)
    except subprocess.TimeoutExpired:
        child.kill()
        stdout, stderr = child.communicate()
        raise RuntimeError(f"command timed out: {argv!r}: {stdout!r}: {stderr!r}")
    return {"argv": list(map(str, argv)), "pid": pid, "started_ns": started,
            "ended_ns": time.time_ns(), "exit": child.returncode,
            "stdout": base64.b64encode(stdout).decode(),
            "stderr": base64.b64encode(stderr).decode()}


def surface(row):
    return {key: row[key] for key in ("exit", "stdout", "stderr")}


class Peer:
    def __init__(self, binary, folder, env):
        self.binary = str(binary)
        self.folder = folder
        self.env = dict(env)
        self.socket = folder / "s"
        self.rows = []

    def argv(self, args):
        return [self.binary, "-S", str(self.socket), *args]

    def run(self, args, stdin=None):
        row = capture(self.argv(args), self.env, self.folder, stdin)
        self.rows.append(row)
        return row

    def value(self, option):
        return self.run(["show-options", "-gqv", option])

    def control(self):
        commands = (b"display-message -p CONTROL_BEFORE\n"
                    b"wait-for -U contract-unlocked\n"
                    b"display-message -p CONTROL_AFTER\ndetach-client\n")
        row = self.run(["-C", "attach-session", "-t", "errors"], commands)
        blocks, body = [], None
        for line in base64.b64decode(row["stdout"]).decode().splitlines():
            if line.startswith("%begin "):
                if body is not None:
                    raise RuntimeError("control scene not reached: nested block")
                body = []
            elif line.startswith(("%end ", "%error ")) and body is not None:
                blocks.append({"terminal": line.split()[0], "body": body})
                body = None
            elif body is not None:
                body.append(line)
        if body is not None:
            raise RuntimeError("control scene not reached: unclosed block")
        return {"exit": row["exit"], "stderr": row["stderr"], "blocks": blocks}

    def binding(self):
        import fcntl
        import pty
        import struct
        import termios

        binding = ("set-option -g @bind-before yes ; wait-for -U contract-unlocked ; "
                   "set-option -g @bind-after bad")
        configured = self.run(["bind-key", "-T", "root", "F12", binding])
        if configured["exit"] != 0:
            raise RuntimeError(f"binding setup failed: {configured}")
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
        attached = subprocess.Popen(self.argv(["attach-session", "-t", "errors"]),
                                    cwd=self.folder, env=self.env, stdin=slave,
                                    stdout=slave, stderr=slave)
        os.close(slave)
        raw = bytearray()
        try:
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                clients = self.run(["list-clients", "-F", "#{client_name}"])
                if base64.b64decode(clients["stdout"]).strip():
                    break
                time.sleep(.02)
            else:
                raise RuntimeError("binding client did not attach")
            os.write(master, b"\x1b[24~")
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                ready, _, _ = select.select([master], [], [], .05)
                if ready:
                    raw.extend(os.read(master, 65536))
                if b"channel contract-unlocked not locked" in raw:
                    break
            before = self.value("@bind-before")
            after = self.value("@bind-after")
            self.run(["detach-client", "-a"])
            attached.wait(timeout=5)
            return {"before": surface(before), "after": surface(after),
                    "error_visible": b"channel contract-unlocked not locked" in raw,
                    "client_exit": attached.returncode,
                    "terminal_sha256": hashlib.sha256(raw).hexdigest()}
        finally:
            if attached.poll() is None:
                attached.kill()
                attached.wait()
            os.close(master)
            (self.folder / "binding-terminal.bin").write_bytes(raw)

    def matrix(self, source):
        result = {}
        try:
            created = self.run(["-f", "/dev/null", "new-session", "-d", "-s", "errors"])
            if created["exit"] != 0:
                raise RuntimeError(f"session setup failed: {created}")
            for name, args in CASES.items():
                result[name] = surface(self.run(args))
            result["before"] = surface(self.value("@before"))
            result["after"] = surface(self.value("@after"))
            result["source"] = surface(self.run(["source-file", str(source)]))
            for name in ("@src-before", "@src-skipped", "@src-after"):
                result[name] = surface(self.value(name))
            result["control"] = self.control()
            result["binding"] = self.binding()
            result["recovery"] = surface(self.run(["display-message", "-p", "RECOVERED"]))
            return result
        finally:
            self.run(["kill-server"])
            (self.folder / "commands.json").write_text(json.dumps(self.rows, indent=2) + "\n")


def require_reached(name, matrix):
    binding = matrix["binding"]
    witness = {"exit": 0, "stdout": base64.b64encode(b"yes\n").decode(), "stderr": ""}
    if (binding["before"] != witness or not binding["error_visible"]
            or binding["client_exit"] != 0):
        raise RuntimeError(f"{name}: binding scene not reached: {binding}")
    control = matrix["control"]
    sequence = [
        {"terminal": "%end", "body": ["CONTROL_BEFORE"]},
        {"terminal": "%error", "body": ["channel contract-unlocked not locked"]},
        {"terminal": "%end", "body": ["CONTROL_AFTER"]},
    ]
    blocks = control["blocks"]
    if (control["exit"] != 0 or control["stderr"]
            or not any(blocks[i:i + 3] == sequence for i in range(len(blocks) - 2))):
        raise RuntimeError(f"{name}: control scene not reached: {control}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tmux", required=True, type=Path)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--tiny", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    for binary in (args.tmux, args.binary, args.tiny):
        if binary is not None and (not binary.is_absolute() or not binary.is_file()):
            parser.error("every binary must be an existing absolute path")
    args.output.mkdir(parents=True, exist_ok=False)
    env = {k: v for k, v in os.environ.items()
           if k not in {"RMUX", "TMUX", "RMUX_PANE", "TMUX_PANE"}}
    env.update(TERM="xterm-256color", NO_COLOR="1", RMUX_DISABLE_TMUX_FALLBACK="1")
    home = args.output / "home"
    home.mkdir()
    env.update(HOME=str(home), XDG_CONFIG_HOME=str(home / "xdg"))
    env["RMUX_FULL_BINARY_PATH"] = str(args.binary)
    version = capture([str(args.tmux), "-V"], env, args.output)
    if base64.b64decode(version["stdout"]).strip() != b"tmux 3.7b" or version["exit"] != 0:
        raise RuntimeError(f"tmux 3.7b required: {version}")
    source = args.output / "source.conf"
    source.write_text("set-option -g @src-before yes ; wait-for -U contract-unlocked ; "
                      "set-option -g @src-skipped bad\nset-option -g @src-after yes\n")
    report = {"oracle": {"path": str(args.tmux), "sha256": sha(args.tmux),
                         "version": version}, "results": {}, "differences": []}
    (args.output / "oracle-identity.json").write_text(json.dumps(report["oracle"], indent=2) + "\n")
    for name, binary in (("tmux", args.tmux), ("full", args.binary), ("tiny", args.tiny)):
        if binary is None:
            continue
        folder = args.output / name
        folder.mkdir()
        matrix = Peer(binary, folder, env).matrix(source)
        report["results"][name] = {"path": str(binary), "sha256": sha(binary), "matrix": matrix}
        try:
            require_reached(name, matrix)
        except RuntimeError as error:
            report["reachability_error"] = str(error)
            (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
            raise
        if name != "tmux":
            oracle = report["results"]["tmux"]["matrix"]
            for case, expected in oracle.items():
                actual = matrix[case]
                if case == "binding":
                    expected = {k: v for k, v in expected.items() if k != "terminal_sha256"}
                    actual = {k: v for k, v in actual.items() if k != "terminal_sha256"}
                if actual != expected:
                    report["differences"].append({"binary": name, "case": case,
                                                  "oracle": expected, "actual": actual})
        (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"cases": len(report["results"]["tmux"]["matrix"]),
                      "differences": len(report["differences"])}))
    return bool(report["differences"])


if __name__ == "__main__":
    raise SystemExit(main())
