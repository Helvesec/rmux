"""Owned, bounded macOS process/PTY support; no product imports or mutations."""
import ctypes
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import termios
import threading
import time
import uuid


def stamp():
    return {"wall": time.time(), "monotonic": time.monotonic()}


def sha(path):
    return hashlib.file_digest(open(path, "rb"), "sha256").hexdigest()


def save(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + "\n")


class Skip(Exception):
    pass


class Failure(Exception):
    pass


LIBPROC = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
LIBPROC.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
LIBPROC.proc_pidpath.restype = ctypes.c_int


def identity(pid):
    buf = ctypes.create_string_buffer(4096)
    if LIBPROC.proc_pidpath(pid, buf, len(buf)) <= 0:
        return None
    p = subprocess.run(["/bin/ps", "-p", str(pid), "-o", "lstart=", "-o", "command="],
                       capture_output=True, text=True, timeout=1)
    if p.returncode or not p.stdout.strip():
        return None
    return {"pid": pid, "path": os.path.realpath(os.fsdecode(buf.value)),
            "ps_lstart_argv": p.stdout.strip()}


OSC = re.compile(rb"\x1b\](?:0|2);(.*?)(?:\x07|\x1b\\)", re.S)


def first_title(raw):
    match = OSC.search(raw)
    if match:
        return {"payload": match[1].decode("utf-8", "replace"),
                "start": match.start(), "end": match.end()}
    return None


class Drain:
    def __init__(self, fd):
        self.fd = fd
        self.raw = bytearray()
        self.first = None
        self.events = []
        self.error = None
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self):
        try:
            while not self.stop.is_set():
                if not select.select([self.fd], [], [], 0.02)[0]:
                    continue
                chunk = os.read(self.fd, 65536)
                if not chunk:
                    break
                self.raw.extend(chunk)
                self.events.append(dict(stamp(), bytes=len(chunk), total=len(self.raw)))
                if self.first is None:
                    self.first = first_title(bytes(self.raw))
                    if self.first:
                        self.first["observed"] = stamp()
                if len(self.raw) > 2 * 1024 * 1024:
                    self.error = "capture exceeded 2 MiB"
                    break
        except OSError as exc:
            self.error = str(exc)

    def finish(self, path):
        self.stop.set()
        self.thread.join(timeout=0.2)
        Path(path).write_bytes(bytes(self.raw))
        return {"path": str(path), "bytes": len(self.raw), "sha256": sha(path),
                "first_title": self.first, "reads": self.events, "drain_error": self.error}


class Client:
    def __init__(self, case, session, control=False, rows=24, cols=80):
        self.case, self.control, self.tty, self.master = case, control, None, None
        argv = case.prefix + (["-C"] if control else []) + ["attach-session", "-t", session]
        self.record = {"argv": argv, "session": session, "control": control, "start": stamp()}
        if control:
            self.p = subprocess.Popen(argv, env=case.env, stdin=subprocess.PIPE,
                                      stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                      start_new_session=True)
            self.drains = [Drain(self.p.stdout.fileno()), Drain(self.p.stderr.fileno())]
        else:
            try:
                self.master, slave = pty.openpty()
                self.tty = os.ttyname(slave)
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
                self.p = subprocess.Popen(argv, env=case.env, stdin=slave, stdout=slave,
                                          stderr=slave, start_new_session=True)
            except OSError as exc:
                raise Skip(f"PTY unavailable: {exc}") from exc
            finally:
                if 'slave' in locals():
                    os.close(slave)
            self.drains = [Drain(self.master)]
            self.record.update(tty=self.tty, rows=rows, cols=cols)
        case.clients.append(self)
        case.r["clients"].append(self.record)
        self.record["pid"] = self.p.pid
        case.until(lambda: identity(self.p.pid), "persistent client executable", 1)
        self.record["identity"] = identity(self.p.pid)
        case.require(self.record["identity"]["path"] == case.binary, "client executable matches pin")
        if control:
            token = "LBHELLO" + uuid.uuid4().hex[:8]
            command = f"display-message -p '{token}|#{{pid}}|#{{version}}'\n"
            self.p.stdin.write(command.encode())
            self.p.stdin.flush()
            self.record["handshake_command"] = command
            expected = f"{token}|{case.server['pid']}|".encode()
            case.until(lambda: expected in self.drains[0].raw and
                       re.search(rb"%end \d+ \d+ \d+\r?\n", self.drains[0].raw),
                       "successful control handshake", 2)
            self.record["handshake_observed"] = stamp()

    def close(self):
        if self.p.poll() is None:
            # Popen retains the unreaped child identity; no process-name signalling.
            self.p.terminate()
            try:
                self.p.wait(timeout=0.4)
            except subprocess.TimeoutExpired:
                self.p.kill()
                self.p.wait(timeout=0.4)
        self.record["returncode"] = self.p.returncode
        self.record["captures"] = [d.finish(self.case.out / f"client-{self.p.pid}-{i}.bin")
                                    for i, d in enumerate(self.drains)]
        if self.master is not None:
            os.close(self.master)
        for pipe in (self.p.stdin, self.p.stdout, self.p.stderr):
            if pipe is not None:
                pipe.close()


class Case:
    def __init__(self, engine, binary, name, out, deadline):
        self.engine, self.binary, self.name = engine, os.path.realpath(binary), name
        self.out = Path(out) / name
        self.out.mkdir(parents=True)
        self.label = "LBf" + uuid.uuid4().hex[:10]
        self.prefix = [self.binary, "-L", self.label, "-f", "/dev/null"]
        removed = [k for k in os.environ if k.startswith(("TMUX", "RMUX"))]
        self.env = {k: v for k, v in os.environ.items() if k not in removed}
        self.env.update(TERM="xterm-256color", LC_ALL="C", RMUX_DISABLE_TMUX_FALLBACK="1")
        self.end = time.monotonic() + deadline
        self.clients, self.producers, self.server, self.socket = [], [], None, None
        self.r = {"engine": engine, "scenario": name, "label": self.label,
                  "binary": self.binary, "binary_sha256_before": sha(self.binary),
                  "start": stamp(), "deadline_seconds": deadline,
                  "env": {k: self.env[k] for k in ("TERM", "LC_ALL", "RMUX_DISABLE_TMUX_FALLBACK")},
                  "unset_env_names": removed, "core_limit": 0,
                  "commands": [], "clients": [], "assertions": [], "cleanup": []}

    def q(self, *args, timeout=2, check=True, cleanup=False):
        timeout = min(timeout, max(0.02, self.end - time.monotonic())) if not cleanup else timeout
        record = {"argv": self.prefix + list(args), "start": stamp(), "timeout": timeout}
        self.r["commands"].append(record)
        p = subprocess.Popen(record["argv"], env=self.env, stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
        record["pid"] = p.pid
        try:
            stdout, stderr = p.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            p.kill()
            stdout, stderr = p.communicate(timeout=0.5)
            record["timed_out"] = True
        record.update(finish=stamp(), returncode=p.returncode,
                      stdout=stdout.decode("utf-8", "replace"), stderr=stderr.decode("utf-8", "replace"))
        if check and (p.returncode or record.get("timed_out")):
            raise Failure(f"command failed: {args!r}: rc={p.returncode}, timeout={record.get('timed_out', False)}, {record['stderr']!r}")
        return record["stdout"].rstrip("\n")

    def require(self, condition, description, **details):
        self.r["assertions"].append(dict(description=description, passed=bool(condition), **details))
        if not condition:
            raise Failure(description)

    def until(self, fn, description, timeout=2):
        deadline = min(self.end, time.monotonic() + timeout)
        while time.monotonic() < deadline:
            value = fn()
            if value:
                return value
            time.sleep(0.01)
        raise Failure(f"timeout waiting for {description}")

    def handshake(self):
        value = self.q("display-message", "-p", "#{pid}|#{version}|#{socket_path}")
        parts = value.split("|")
        self.require(len(parts) == 3 and parts[0].isdigit(), "live-server handshake", value=value)
        pid, version, socket = parts
        ident = identity(int(pid))
        wanted = self.binary if self.engine == "oracle" else str(Path(self.binary).with_name("rmux-daemon"))
        self.require(ident is not None and ident["path"] == wanted, "live-server executable matches pin", identity=ident)
        self.require(Path(socket).name == self.label, "socket belongs to fresh label", socket=socket)
        if self.server:
            self.require(ident == self.server, "same server PID, executable and birth", expected=self.server, actual=ident)
        else:
            self.server, self.socket = ident, socket
            self.r.update(server=ident, version=version, socket=socket, server_sha256_before=sha(wanted))
        return ident

    def new(self, session, rows=24, cols=80, name=None):
        args = ["new-session", "-d", "-s", session, "-x", str(cols), "-y", str(rows)]
        if name:
            args += ["-n", name]
        args.append("/bin/sleep 60")
        if self.server is None:
            try:
                self.q(*args)
                self.handshake()
            except (Failure, OSError) as exc:
                raise Skip(f"startup authority unavailable: {exc}") from exc
        else:
            self.q(*args)
        pid_text = self.q("display-message", "-p", "-t", session, "#{pane_pid}")
        ident = self.until(lambda: identity(int(pid_text)), "owned quiet producer", 1)
        self.producers.append(ident)
        self.r["producers"] = self.producers

    def client_rows(self):
        out = self.q("list-clients", "-F", "#{client_pid}|#{client_name}|#{client_tty}|#{client_session}|#{client_control_mode}")
        return [line.split("|") for line in out.splitlines() if line]

    def wait_clients(self, clients):
        def match():
            rows = self.client_rows()
            expected = {(str(c.p.pid), c.record["session"], str(int(c.control))) for c in clients}
            return rows if len(rows) == len(clients) and {(r[0], r[3], r[4]) for r in rows} == expected else None
        rows = self.until(match, "exact attached client PIDs/sessions/modes")
        self.r.setdefault("client_identity_observations", []).append({"time": stamp(), "rows": rows})
        return rows

    def close(self):
        if self.server and identity(self.server["pid"]) == self.server:
            try:
                self.q("kill-server", timeout=0.7, check=False, cleanup=True)
            except Exception as exc:
                self.r["cleanup"].append({"kill_server_error": str(exc)})
        for client in reversed(self.clients):
            client.close()
        for owned in ([self.server] if self.server else []) + self.producers:
            for sig in (signal.SIGTERM, signal.SIGKILL):
                if identity(owned["pid"]) != owned:
                    break
                os.kill(owned["pid"], sig)
                self.r["cleanup"].append({"pid": owned["pid"], "signal": sig.name})
                time.sleep(0.05)
            self.r["cleanup"].append({"pid": owned["pid"], "owned_process_gone": identity(owned["pid"]) != owned})
        # Never remove a live socket or any path not returned by the owned handshake.
        if self.socket and identity(self.server["pid"]) != self.server:
            for path in (Path(self.socket), Path(self.socket + ".startup-lock")):
                if path.name in (self.label, self.label + ".startup-lock") and path.exists():
                    path.unlink()
                    self.r["cleanup"].append({"removed_owned_remnant": str(path)})
        self.r.update(finish=stamp(), binary_sha256_after=sha(self.binary))
        if self.server:
            self.r["server_sha256_after"] = sha(self.server["path"])
        save(self.out / "receipt.json", self.r)
