#!/usr/bin/env python3
"""Bounded native new-window -S/-k reference matrix; no third-party packages.

Run once with the frozen tmux 3.7b and once with the candidate rmux. Each route
owns its socket and config and records commands, outputs and binary identity.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import select
from types import SimpleNamespace
import subprocess
import sys
import time


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


class Control:
    def __init__(self, binary, socket, env):
        self.process = subprocess.Popen([str(binary), "-S", str(socket), "-f", "/dev/null", "-C", "attach-session", "-t", "s"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.buffer = b""
        self.transcript = []
        self.response()

    def line(self):
        deadline = time.monotonic() + 10
        while b"\n" not in self.buffer:
            remaining = deadline - time.monotonic()
            assert remaining > 0 and select.select([self.process.stdout], [], [], remaining)[0], "control timeout"
            data = os.read(self.process.stdout.fileno(), 65536)
            assert data, "control EOF"
            self.buffer += data
        line, self.buffer = self.buffer.split(b"\n", 1)
        line = line.decode()
        self.transcript.append(line)
        return line

    def response(self, command=False):
        while True:
            begin = self.line()
            if not begin.startswith("%begin "):
                continue
            selected = not command or begin.split()[-1] == "1"
            output = []
            while True:
                line = self.line()
                if line.startswith(("%end ", "%error ")):
                    if not selected:
                        break
                    failed = line.startswith("%error ")
                    text = "".join(item + "\n" for item in output)
                    return SimpleNamespace(returncode=int(failed), stdout="" if failed else text, stderr=text if failed else "")
                output.append(line)

    def invoke(self, argv):
        self.process.stdin.write((shlex.join(argv) + "\n").encode())
        self.process.stdin.flush()
        return self.response(command=True)

    def close(self):
        self.process.stdin.close()
        self.process.wait(timeout=10)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--server", type=Path, help="Exact server executable for an installed tiny layout")
    args = parser.parse_args()
    if sys.platform != "linux":
        parser.error("this native runner verifies daemon lifetime through Linux /proc")
    binary = args.binary.resolve()
    root = args.out.resolve()
    root.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, TERM="xterm-256color", LC_ALL="C", RMUX_DISABLE_TMUX_FALLBACK="1")
    for key in ["TMUX", "TMUX_PANE", "RMUX", "RMUX_PANE"]:
        env.pop(key, None)
    receipt = {"binary": str(binary), "binary_sha256": digest(binary), "routes": {}}
    try:
        for route in ["direct", "queue", "source", "control"]:
            # Short dedicated pathname stays below sockaddr_un.sun_path limits.
            socket = root.parent / ("nws-" + str(os.getpid()) + "-" + route)
            assert not socket.exists()
            control = None
            records = []
            result = {"commands": records, "checks": 0, "socket": str(socket)}
            receipt["routes"][route] = result

            def run(*argv):
                command = [str(binary), "-S", str(socket), "-f", "/dev/null", *argv]
                p = subprocess.run(command, env=env, text=True, capture_output=True, timeout=10)
                records.append({"argv": argv, "rc": p.returncode, "out": p.stdout, "err": p.stderr})
                return p

            def check(condition, detail):
                assert condition, detail
                result["checks"] += 1

            def ok(*argv):
                p = run(*argv)
                check(p.returncode == 0 and not p.stderr, records[-1])
                return p.stdout

            def invoke(argv):
                if route == "control":
                    p = control.invoke(argv)
                    records.append({"argv": argv, "rc": p.returncode, "out": p.stdout, "err": p.stderr})
                    return p
                if route == "direct":
                    return run(*argv)
                if route == "queue":
                    return run(*argv, ";", "display-message", "-p", "DONE")
                config = root / "commands.conf"
                config.write_text(shlex.join(argv) + "\n")
                return run("source-file", str(config))

            def invoke_ok(argv, output=""):
                p = invoke(argv)
                expected = output + ("DONE\n" if route == "queue" else "")
                check((p.returncode, p.stdout, p.stderr) == (0, expected, ""), records[-1])

            def state():
                return ok("list-windows", "-t", "s", "-F", "#{window_index}|#{window_id}|#{window_name}")

            try:
                ok("new-session", "-d", "-s", "s", "-n", "zero", "sleep 60")
                pid = int(ok("display-message", "-p", "#{pid}").strip())
                executable = Path("/proc") / str(pid) / "exe"
                result["server_pid"] = pid
                if executable.exists():
                    result["server_executable"] = str(executable.resolve())
                    result["server_sha256"] = digest(executable)
                    allowed = [args.server.resolve()] if args.server else [binary, binary.with_name("rmux-daemon")]
                    check(executable.resolve() in allowed, "wrong daemon executable")
                ok("new-window", "-d", "-t", "s:1", "-n", "found", "sleep 60")
                if route == "control":
                    control = Control(binary, socket, env)
                hooks = ["after-new-window", "after-select-window", "session-window-changed"]
                for hook in hooks:
                    ok("set-hook", "-g", hook, "set-option -g @" + hook + " yes")
                ok("select-window", "-t", "s:1")
                check(ok("show-options", "-gqv", "@after-select-window") == "yes\n", "ordinary selection lost its after hook")
                for flags in [[], ["-d"], ["-k"], ["-dk"]]:
                    ok("select-window", "-t", "s:0")
                    for hook in hooks:
                        ok("set-option", "-gu", "@" + hook)
                    before = state()
                    invoke_ok(["new-window", "-S", *flags, "-t", "s:", "-n", "found", "-P", "-F", "PRINT", "ignored-command"])
                    check(state() == before, "reuse changed the window identities")
                    snapshot = ok("display-message", "-p", "-t", "s", "#{window_index}|#{@after-new-window}|#{@after-select-window}|#{@session-window-changed}")
                    check(snapshot == ("0|||\n" if any("d" in f for f in flags) else "1|||yes\n"), snapshot)
                # Explicit indices stay creation/replacement, even with -S.
                before = state()
                p = invoke(["new-window", "-Sd", "-t", "s:1", "-n", "found"])
                check(p.returncode == 1 and "index 1 in use" in p.stderr, records[-1])
                check(state() == before, "failed creation altered windows")
                invoke_ok(["new-window", "-Sdk", "-t", "s:1", "-n", "replaced", "-P", "-F", "#{window_index}:#{window_name}", "sleep 60"], "1:replaced\n")
                check(state() != before and len(state().splitlines()) == 2, "replacement not committed")
                invoke_ok(["new-window", "-Sd", "-t", "s:", "-n", "missing", "-P", "-F", "#{window_index}:#{window_name}", "sleep 60"], "2:missing\n")
                ok("new-window", "-d", "-t", "s:3", "-n", "missing", "sleep 60")
                before = state()
                p = invoke(["new-window", "-S", "-t", "s:", "-n", "missing"])
                check(p.returncode == 1 and "multiple windows named missing" in p.stderr, records[-1])
                check(state() == before, "ambiguous reuse changed windows")
                result["passed"] = True
            finally:
                if control:
                    control.close()
                    result["control_transcript"] = control.transcript
                run("kill-server")
                deadline = time.monotonic() + 5
                proc_stat = Path("/proc") / str(result.get("server_pid", 0)) / "stat"
                def server_live():
                    try:
                        return proc_stat.read_text().rsplit(")", 1)[1].split()[0] != "Z"
                    except FileNotFoundError:
                        return False
                while server_live() and time.monotonic() < deadline:
                    time.sleep(0.02)
                check(not server_live(), "owned daemon did not exit")
                # tmux leaves its socket pathname after normal server exit.
                socket.unlink(missing_ok=True)
                check(not socket.exists(), "owned socket did not disappear")
                result["cleaned"] = True
                check(digest(result["server_executable"]) == result["server_sha256"], "daemon binary changed during matrix")
        assert digest(binary) == receipt["binary_sha256"], "binary changed during matrix"
        receipt["passed"] = True
    finally:
        (root / "result.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps({"passed": receipt["passed"], "checks": sum(r["checks"] for r in receipt["routes"].values())}))


if __name__ == "__main__":
    main()
