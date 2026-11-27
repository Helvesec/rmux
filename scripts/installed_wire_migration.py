#!/usr/bin/env python3
"""L15.1/L15.2 installed archive cells, without protocol emulation or publication.

Both extracted packages must already be installed with their own install.sh or
install.ps1. Every launch is checked against that package's installed layout.
Only private endpoints and sessions created here are stopped. Receipts are
runtime observations: release signature verification and the native campaign
must join them to signed local packages/RCs before any product cell can close.
"""
import argparse
import itertools
import json
import os
from pathlib import Path
import sys
import subprocess
import time
import uuid

from installed_consumer_diagnostics import validate_wire_refusal, wire_refusal_diagnostic
from installed_wire_packages import InstalledPackage


def require_success(record):
    if record["returncode"] != 0:
        raise ValueError(f"installed command failed: {record['command']}: {record['stderr']}")
    return record["stdout"]


class Migration:
    def __init__(self, old, new, output):
        self.packages = (old, new)
        if old.prefix == new.prefix or old.prefix.is_relative_to(new.prefix) or new.prefix.is_relative_to(old.prefix):
            raise ValueError("old/new installed prefixes must be disjoint")
        self.output = Path(output)
        if not self.output.is_absolute() or self.output.parent.resolve() != self.output.parent:
            raise ValueError("absolute unredirected private output required")
        self.output.mkdir(mode=0o700, exist_ok=False)
        home = self.output / "home"
        home.mkdir()
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(("RMUX_", "TMUX"))}
        self.env.update(HOME=str(home), TERM="xterm-256color")
        self.config = self.output / "idle.conf"
        self.config.write_text("set-option -s exit-empty off\n")
        self.receipt = {"schema": 1, "line": "L15.2", "status": "incomplete",
                        "evidence_kind": "fixture" if old.fixture else "installed-runtime-observation",
                        "product_qualification": "pending_signed_package_or_RC_and_native_campaign",
                        "packages": [p.verify() for p in self.packages], "cells": []}
        self.save()

    def save(self):
        temporary = self.output / "receipt.tmp"
        temporary.write_text(json.dumps(self.receipt, indent=2, sort_keys=True) + "\n")
        temporary.replace(self.output / "receipt.json")

    def call(self, package, endpoint, *args):
        record = package.call(["-S", endpoint, *args], self.env)
        self.current["commands"].append(record)
        self.save()
        return record

    def snapshot(self, package, endpoint):
        result = self.call(package, endpoint, "display-message", "-p", "#{pid}")
        pid = int(require_success(result).strip())
        count = int(require_success(self.call(package, endpoint, "display-message", "-p", "#{server_sessions}")).strip())
        names = []
        if count:
            names = sorted(require_success(self.call(package, endpoint, "list-sessions", "-F", "#{session_name}")).splitlines())
        if pid <= 0 or count != len(names):
            raise ValueError("daemon snapshot has inconsistent PID/session population")
        snapshot = {"pid": pid, "sessions": names}
        if not package.fixture:
            snapshot["daemon_image"] = package.verify_process(pid)
        final_pid = int(require_success(self.call(package, endpoint, "display-message", "-p", "#{pid}")).strip())
        if final_pid != pid:
            raise ValueError("daemon changed during snapshot")
        return snapshot

    def start(self, package, endpoint):
        require_success(self.call(package, endpoint, "-f", self.config, "start-server"))

    def check_read_refusals(self, client, daemon, endpoint, witness):
        expected = wire_refusal_diagnostic(client.verify(), daemon.verify(), endpoint)
        diagnostics = {expected}
        if os.name == "nt":
            # The full Windows CLI quotes its generated pipe for PowerShell.
            diagnostics.add(expected.replace(f"-S '{endpoint}' ", f'-S "{endpoint}" '))
        for command in (("list-sessions",), ("has-session", "-t", witness),
                        ("display-message", "-p", "#{pid}")):
            record = self.call(client, endpoint, *command)
            if (record["returncode"] != 1 or record["stdout"]
                    or record["stderr"].removesuffix("\n").removesuffix("\r") not in diagnostics):
                raise ValueError(f"{command[0]} did not give the installed wire migration warning")

    def stop(self, package, endpoint):
        record = self.call(package, endpoint, "kill-server")
        require_success(record)
        deadline = time.monotonic() + 5
        while True:
            absent = self.call(package, endpoint, "display-message", "-p", "#{pid}")
            if absent["returncode"] == 1:
                return record
            if absent["returncode"] != 0 or time.monotonic() >= deadline:
                raise ValueError("explicit shutdown did not leave the private daemon absent")
            time.sleep(0.05)

    def cell(self, index, client, daemon, state):
        endpoint = (rf"\\.\pipe\rmux-migration-{uuid.uuid4().hex}" if os.name == "nt"
                    else str(self.output / f"s{index}"))
        if os.name != "nt" and len(os.fsencode(endpoint)) >= 104:
            raise ValueError("private output is too long for a Unix socket")
        self.current = {"id": f"{client.version}/{daemon.version}/{state}",
                        "client_wire": client.wire, "daemon_wire": daemon.wire,
                        "state": state, "endpoint": endpoint, "status": "incomplete", "commands": []}
        self.receipt["cells"].append(self.current)
        owner = None
        try:
            absent = self.call(daemon, endpoint, "display-message", "-p", "#{pid}")
            if absent["returncode"] != 1:
                raise ValueError("cell endpoint is not absent before owned startup")
            if state == "absent":
                # With no daemon, the selected client launches its own package.
                owner = client
                self.start(client, endpoint)
                self.current["effective_daemon_wire"] = client.wire
                self.current["after_start"] = self.snapshot(client, endpoint)
                if self.current["after_start"]["sessions"]:
                    raise ValueError("absent startup inherited sessions")
            else:
                owner = daemon
                self.start(daemon, endpoint)
                witness = f"migration-{index}"
                if state == "active":
                    require_success(self.call(daemon, endpoint, "new-session", "-d", "-s", witness))
                before = self.snapshot(daemon, endpoint)
                if before["sessions"] != ([witness] if state == "active" else []):
                    raise ValueError("cell did not establish its requested daemon state")
                result = self.call(client, endpoint, "start-server")
                if not client.fixture and (client.wire, daemon.wire) == (9, 8):
                    self.check_read_refusals(client, daemon, endpoint, witness)
                after = self.snapshot(daemon, endpoint)
                if before != after:
                    raise ValueError("client probe implicitly replaced daemon/session state")
                self.current.update(before=before, after_probe=after)
                if client.wire == daemon.wire:
                    require_success(result)
                else:
                    self.current["loss_notice"] = "Explicit restart loses the sessions created by this cell."
                    restart = self.stop(daemon, endpoint)
                    owner = None
                    restart.update(explicit=True, sessions_lost_acknowledged=True)
                    refusal = {**result, "expected_returncode": 1, "fixture": client.fixture,
                               "diagnostic": wire_refusal_diagnostic(result["client"], restart["client"], endpoint, client.fixture),
                               "before": before,
                               "after_refusal": after, "restart": restart}
                    validate_wire_refusal(refusal)
                    self.current["refusal"] = refusal
                    owner = client
                    self.start(client, endpoint)
                    restarted = self.snapshot(client, endpoint)
                    if restarted["pid"] == before["pid"] or restarted["sessions"]:
                        raise ValueError("explicit restart retained daemon or old sessions")
                    self.current["after_restart"] = restarted
                    require_success(self.call(client, endpoint, "new-session", "-d", "-s", "after-restart"))
                    if self.snapshot(client, endpoint)["sessions"] != ["after-restart"]:
                        raise ValueError("client cannot use its restarted installed daemon")
            self.current["status"] = "pass"
        except BaseException as error:
            self.current.update(status="fail", error=str(error))
            raise
        finally:
            if owner is not None:
                try:
                    self.current["cleanup"] = self.stop(owner, endpoint)
                except Exception as error:
                    self.current.update(status="fail", cleanup_error=str(error))
                    self.save()
                    raise ValueError("owned daemon cleanup failed; see receipt") from error
            self.save()

    def run(self):
        if self.packages[0].fixture != self.packages[1].fixture:
            raise ValueError("fixture and product packages cannot be mixed")
        for package in self.packages:
            # The released v8 CLI only accepts the tmux-compatible short flag.
            version = package.call(["-V" if package.wire == 8 else "--version"], self.env)
            if require_success(version).strip() != "rmux " + package.version:
                raise ValueError("installed CLI version differs from selected package")
        for index, (client, daemon, state) in enumerate(itertools.product(self.packages, self.packages, ("absent", "idle", "active"))):
            self.cell(index, client, daemon, state)
        self.receipt["status"] = "pass" if all(c["status"] == "pass" for c in self.receipt["cells"]) else "fail"
        self.save()
        return self.receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("old-package", "old-prefix", "new-package", "new-prefix", "output"):
        parser.add_argument("--" + name, required=True, type=Path)
    for name in ("old-commit", "new-commit"):
        parser.add_argument("--" + name)
    parser.add_argument("--fixture", action="store_true", help="tooling fixtures only; no product credit")
    args = parser.parse_args()
    old = InstalledPackage(args.old_package, args.old_prefix, "0.10.0", 8, args.fixture, args.old_commit)
    new = InstalledPackage(args.new_package, args.new_prefix, "0.11.0", 9, args.fixture, args.new_commit)
    receipt = Migration(old, new, args.output).run()
    return 0 if receipt["status"] == "pass" else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, OSError, subprocess.TimeoutExpired) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
