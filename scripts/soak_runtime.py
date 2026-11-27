"""Bounded native workload on an exclusively owned multiplexer server."""
import os
from pathlib import Path
import secrets
import shlex
import shutil
import subprocess
import sys
import threading
import time

import candidate_identity as identity


class RuntimeBackend:
    def __init__(self, binary, directory, env):
        self.binary = str(Path(binary).absolute())
        self.env = dict(env, TERM="xterm-256color")
        self.env.pop("TMUX", None)
        self.prefix = [self.binary, "-L", "soak-" + secrets.token_hex(12), "-f", os.devnull]
        self.directory = Path(directory)
        self.clients, self.readers = [], []
        self.panes, self.markers = {}, {}
        self.started = False
        self.timeout = 10
        self.binary_stamp = identity.stamp(Path(self.binary).stat())

    def unchanged(self):
        if identity.stamp(Path(self.binary).stat()) != self.binary_stamp:
            raise ValueError("soak executable changed")

    def command(self, *args, check=True):
        self.unchanged()
        result = subprocess.run([*self.prefix, *args], env=self.env, capture_output=True,
                                text=True, timeout=self.timeout)
        if check and result.returncode:
            raise ValueError("soak command failed: " + args[0])
        return result

    def wait_for(self, predicate):
        end = time.monotonic() + self.timeout
        while not predicate():
            if time.monotonic() >= end:
                raise ValueError("soak postcondition timeout")
            time.sleep(0.02)

    def setup(self, profile):
        self.timeout = profile["command_timeout_seconds"]
        self.directory.mkdir(parents=True)
        helper = self.directory / "echo.py"
        helper.write_text("import sys\nprint('SOAK_READY', flush=True)\n"
                          "for line in sys.stdin:\n"
                          "    print('SOAK_ACK:' + line.strip(), flush=True)\n")
        argv = [sys.executable, "-u", str(helper)]
        if os.name == "nt":
            shell = str(Path(os.environ["SystemRoot"]) / "System32" / "cmd.exe")
            self.env["RMUX_CLIENT_SHELL"] = shell
            self.child_command = subprocess.list2cmdline(argv)
        else:
            shell = "/bin/sh"
            self.env["SHELL"] = shell
            self.child_command = shlex.join(argv)
        shape = profile["topology"]
        self.sessions = ["soak" + str(i) for i in range(shape["sessions"])]
        self.started = True  # Even partial setup owns cleanup of this unique server.
        for worker, session in enumerate(self.sessions):
            first = self.command("new-session", "-d", "-s", session, "-n", "base",
                                 "-P", "-F", "#{pane_id}", self.child_command).stdout.strip()
            self.command("set-option", "-t", session, "default-shell", shell)
            self.panes[worker] = [first]
            for window in range(shape["windows_per_session"]):
                if window:
                    first = self.command("new-window", "-d", "-t", session, "-P", "-F",
                                         "#{pane_id}", self.child_command).stdout.strip()
                    self.panes[worker].append(first)
                for _ in range(shape["panes_per_window"] - 1):
                    pane = self.command("split-window", "-d", "-t", first, "-P", "-F",
                                        "#{pane_id}", self.child_command).stdout.strip()
                    self.panes[worker].append(pane)
        for panes in self.panes.values():
            for pane in panes:
                self.wait_for(lambda pane=pane: "SOAK_READY" in self.capture(pane))
        for index in range(shape["clients"]):
            client = subprocess.Popen([*self.prefix, "-C", "attach-session", "-t",
                                       self.sessions[index % len(self.sessions)]], env=self.env,
                                      stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                      stderr=subprocess.STDOUT)
            self.clients.append(client)
            # Drain output continuously without accumulating 24h of pane output.
            reader = threading.Thread(target=self.drain, args=(client.stdout,), daemon=True)
            reader.start()
            self.readers.append(reader)
        self.wait_for(lambda: self.clients_ok(shape["clients"]))

    @staticmethod
    def drain(stream):
        while stream.read(65536):
            pass

    def capture(self, pane):
        return self.command("capture-pane", "-p", "-t", pane).stdout

    def roundtrip(self, pane, marker):
        self.command("send-keys", "-t", pane, "-l", marker)
        self.command("send-keys", "-t", pane, "Enter")
        self.wait_for(lambda: "SOAK_ACK:" + marker in self.capture(pane))
        self.markers[pane] = marker
        return True

    def exercise(self, operation, worker, sequence):
        panes = self.panes[worker]
        pane = panes[sequence % len(panes)]
        if operation == "input":
            return self.roundtrip(pane, "SOAK-" + str(worker) + "-" + str(sequence))
        if operation == "capture":
            return "SOAK_ACK:" + self.markers[pane] in self.capture(pane)
        if operation == "churn":
            window = self.command("new-window", "-d", "-t", self.sessions[worker],
                                  "-P", "-F", "#{window_id}", self.child_command).stdout.strip()
            self.command("kill-window", "-t", window)
            return True
        raise ValueError("unknown soak operation")

    def clients_ok(self, count):
        listed = self.command("list-clients", "-F", "#{client_session}").stdout.splitlines()
        return (len(listed) == count and all(s in self.sessions for s in listed)
                and all(client.poll() is None for client in self.clients)
                and all(reader.is_alive() for reader in self.readers))

    def verify(self, profile):
        shape = profile["topology"]
        actual_sessions = self.command("list-sessions", "-F", "#{session_name}").stdout.splitlines()
        topology = set(actual_sessions) == set(self.sessions)
        for worker, session in enumerate(self.sessions):
            windows = self.command("list-windows", "-t", session, "-F", "#{window_id}").stdout.splitlines()
            topology &= len(windows) == shape["windows_per_session"]
            actual_panes = []
            for window in windows:
                panes = self.command("list-panes", "-t", window, "-F", "#{pane_id}|#{pane_dead}").stdout.splitlines()
                topology &= (len(panes) == shape["panes_per_window"]
                             and all(p.endswith("|0") for p in panes))
                actual_panes.extend(p.split("|")[0] for p in panes)
            topology &= set(actual_panes) == set(self.panes[worker])
        for worker, panes in self.panes.items():
            for index, pane in enumerate(panes):
                self.roundtrip(pane, "SOAK-FINAL-" + str(worker) + "-" + str(index))
        self.unchanged()
        return {"topology": bool(topology), "roundtrip": True,
                "clients": self.clients_ok(shape["clients"]), "cleanup": True}

    def cleanup(self):
        cleaned = True
        try:
            if self.started:
                cleaned = self.command("kill-server", check=False).returncode == 0
                cleaned &= self.command("list-sessions", check=False).returncode != 0
        finally:
            for client in self.clients:
                try:
                    client.wait(timeout=self.timeout)
                except subprocess.TimeoutExpired:
                    cleaned = False
                    client.terminate()
                    try:
                        client.wait(timeout=self.timeout)
                    except subprocess.TimeoutExpired:
                        client.kill()
                        client.wait(timeout=self.timeout)
                client.stdin.close()
            for reader in self.readers:
                reader.join(timeout=self.timeout)
                cleaned &= not reader.is_alive()
            for client in self.clients:
                client.stdout.close()
        return bool(cleaned)


class CandidateBackend(RuntimeBackend):
    def __init__(self):
        identity.validate_context()
        source, head, target = identity.context()
        binary = shutil.which("rmux")
        if binary is None:
            raise ValueError("candidate rmux unavailable; build it before the soak")
        record = identity.verify_built(Path(binary).absolute(), source, head, target)
        identity.record_launch([record])
        self.executables = [record]
        self.source, self.head, self.target = source, head, target
        super().__init__(binary, source / ".rmux-audit" / ("soak-" + secrets.token_hex(12)), os.environ)

    def verify(self, profile):
        checks = super().verify(profile)
        record = identity.verify_built(Path(self.binary), self.source, self.head, self.target)
        if record != self.executables[0]:
            raise ValueError("soak candidate identity changed")
        return checks
