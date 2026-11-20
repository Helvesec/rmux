"""Owned Unix process/PTY support for the bounded format-job consumer matrix."""
import ctypes
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import shlex
import signal
import struct
import subprocess
import termios
import threading
import time
import uuid
import sys


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


if sys.platform == "darwin":
    LIBPROC = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    LIBPROC.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
    LIBPROC.proc_pidpath.restype = ctypes.c_int


    def mac_identity(pid):
        buf = ctypes.create_string_buffer(4096)
        if LIBPROC.proc_pidpath(pid, buf, len(buf)) <= 0:
            return None
        p = subprocess.run(["/bin/ps", "-p", str(pid), "-o", "lstart=", "-o", "command="],
                           capture_output=True, text=True, timeout=1)
        if p.returncode or not p.stdout.strip():
            return None
        return {"pid": pid, "path": os.path.realpath(os.fsdecode(buf.value)),
                "ps_lstart_argv": p.stdout.strip()}

def identity(pid):
    if sys.platform == "darwin":
        return mac_identity(pid)
    try:
        root = Path("/proc") / str(pid)
        fields = (root / "stat").read_text().rsplit(")", 1)[1].split()
        if fields[0] == "Z":
            return None
        return {"pid": pid, "path": os.path.realpath(root / "exe"), "start_ticks": fields[19]}
    except (OSError, IndexError):
        return None


def pid_marker(path):
    try:
        value = Path(path).read_text().strip()
        return int(value) if value.isdecimal() and int(value) > 0 else None
    except OSError:
        return None


def marker_matches_identity(path, expected):
    reported = pid_marker(path)
    return (reported is not None and expected is not None and
            reported == expected["pid"] and identity(reported) == expected)


def producer_argv_matches(pid, marker):
    """Read the unique creation argument from the OS, never from pane_pid."""
    try:
        if sys.platform == "darwin":
            result = subprocess.run(["/bin/ps", "-ww", "-p", str(pid), "-o", "command="],
                                    capture_output=True, text=True, timeout=1)
            return result.returncode == 0 and result.stdout.rstrip("\n").endswith(" " + str(marker))
        return os.fsencode(marker) in (Path("/proc") / str(pid) / "cmdline").read_bytes().split(b"\0")
    except (OSError, subprocess.TimeoutExpired):
        return False


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
        argv = getattr(case, 'execution_prefix', case.prefix) + (["-C"] if control else []) + ["attach-session", "-t", session]
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
        case.until(lambda: (value if value and value['path'] == case.binary else None)
                   if (value := identity(self.p.pid)) else None, "persistent client executable", 1)
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
        self.out.mkdir(parents=True, exist_ok=False)
        self.label = "LBf" + uuid.uuid4().hex[:10]
        self.prefix = [self.binary, "-L", self.label, "-f", "/dev/null"]
        removed = [k for k in os.environ if k.startswith(("TMUX", "RMUX"))]
        self.env = {k: v for k, v in os.environ.items() if k not in removed}
        self.env.update(TERM="xterm-256color", LC_ALL="C", RMUX_DISABLE_TMUX_FALLBACK="1")
        socket_root = Path(os.environ["FORMAT_JOBS_SOCKET_ROOT"]).resolve()
        socket_root.mkdir(parents=True, exist_ok=True)
        self.env.update(TMUX_TMPDIR=str(socket_root), RMUX_TMPDIR=str(socket_root))
        self.end = time.monotonic() + deadline
        self.clients, self.producers, self.server, self.socket = [], [], None, None
        self._producer_fixtures = []
        self.r = {"engine": engine, "scenario": name, "label": self.label,
                  "binary": self.binary, "binary_sha256_before": sha(self.binary),
                  "start": stamp(), "deadline_seconds": deadline,
                  "env": {k: self.env[k] for k in ("TERM", "LC_ALL", "RMUX_DISABLE_TMUX_FALLBACK")},
                  "unset_env_names": removed, "core_limit": 0,
                  "commands": [], "clients": [], "assertions": [], "cleanup": []}

    def q(self, *args, timeout=5, check=True, cleanup=False):
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
        return self._new_producer(args, session, startup=self.server is None)

    def new_self_identifying(self, session, rows=24, cols=80, name=None):
        return self.new(session, rows, cols, name)

    def new_window(self, target, cwd=None):
        args = ["new-window", "-d", "-t", target]
        if cwd is not None:
            args += ["-c", str(cwd)]
        return self._new_producer(args, target + ".0")

    def split_window(self, target):
        return self._new_producer(["split-window", "-d", "-t", target], target + ".1")

    def _new_producer(self, args, target, startup=False):
        marker = (self.out / ("producer-" + uuid.uuid4().hex + ".pid")).resolve()
        self.require(not marker.exists(), "producer PID marker starts absent",
                     target=target, marker=str(marker))
        # Keep the marker in the producer's native argv for its entire life.
        # O_EXCL also refuses a marker appearing between the check and creation.
        script = ("import os,sys,time; "
                  "fd=os.open(sys.argv[1],os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600); "
                  "os.write(fd,(str(os.getpid())+'\\n').encode()); os.close(fd); time.sleep(90)")
        executable = identity(os.getpid())["path"]
        argv = args + ["exec " + shlex.join([sys.executable, "-c", script, str(marker)])]
        fixture = {"target": target, "marker": str(marker), "creation_argv": argv,
                   "executable": executable,
                   "association_source": "unique creation argv, PID marker and native identity"}
        try:
            try:
                self.q(*argv)
            except (Failure, OSError) as exc:
                # A command can fail after creating its producer. Prove that
                # child before giving close any authority over it.
                try:
                    self._register_producer(fixture)
                except (Failure, OSError):
                    pass
                if startup:
                    raise Skip(f"startup authority unavailable: {exc}") from exc
                raise
            self._register_producer(fixture)
        finally:
            # Even a rejected producer can have started a server. Preserve its
            # independent handshake/cleanup without trusting that producer PID.
            if startup:
                try:
                    self.handshake()
                except (Failure, OSError) as exc:
                    raise Skip(f"startup authority unavailable: {exc}") from exc
        return fixture

    def _register_producer(self, fixture):
        marker = fixture["marker"]
        def producer_identity():
            reported = pid_marker(marker)
            observed = identity(reported) if reported is not None else None
            return (observed if observed and observed["path"] == fixture["executable"] and
                    producer_argv_matches(reported, marker) else None)

        ident = self.until(producer_identity, "self-reported producer OS identity", 1)
        self.require(marker_matches_identity(marker, ident),
                     "producer PID marker matches stable OS identity",
                     target=fixture["target"], marker=marker, identity=ident)
        fixture["identity"] = ident
        self._producer_fixtures.append(fixture)
        self.producers.append(ident)
        self.r["producers"] = self.producers
        self.r.setdefault("self_identifying_producers", []).append(fixture)

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
        owned_processes = [(self.server, None)] if self.server else []
        owned_processes += [(f["identity"], f) for f in self._producer_fixtures]
        for owned, fixture in owned_processes:
            for sig in (signal.SIGTERM, signal.SIGKILL):
                if identity(owned["pid"]) != owned:
                    break
                if fixture and not (marker_matches_identity(fixture["marker"], owned) and
                                    producer_argv_matches(owned["pid"], fixture["marker"])):
                    self.r["cleanup"].append({"pid": owned["pid"], "ownership_recheck_failed": True})
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
