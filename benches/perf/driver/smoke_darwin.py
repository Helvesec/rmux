#!/usr/bin/env python3
"""Exercise the native adapter using supplied binaries; never emit timings.

Debug/reference binaries are allowed here only as functional process fixtures.
Their digests are recorded; this does not certify their source or performance.
"""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import select
import sys
import tempfile
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts/bench"))
import bench_unix as bench
import darwin_driver as driver


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rmux", type=Path, required=True)
    parser.add_argument("--tmux", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--cleanup-only", action="store_true",
                        help="bounded cleanup correction: cold/warm, listing, session/server exit")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("native Darwin is required")
    operations = ["new_session_cold_sh", "new_session_warm_sh", "list_sessions_default",
                  "capture_pane_80x24", "capture_pane_5000_lines",
                  "capture_pane_200x50_scrollback_10k"]
    if args.cleanup_only:
        operations = ["new_session_cold_sh", "new_session_warm_sh", "list_sessions_default",
                      "kill_session", "kill_server"]
    report = {"kind": "darwin-driver-functional-smoke", "performance_evidence": False,
              "started_at": datetime.now(timezone.utc).isoformat(),
              "binaries": {}, "cases": [], "launches": [], "daemon_cleanup": [],
              "limitations": ["Supplied binary hashes do not attest their source commit.",
                              "Functional adapters only; no baseline, speed claim or R6 helper attestation."]}
    real_run = driver.run
    native_env = bench.clean_env()
    watchers = {}
    try:
        with tempfile.TemporaryDirectory(prefix="r5-smoke-", dir="/tmp") as directory:
            config = Path(directory) / "fixture.conf"
            config.write_text("set-option -g default-shell /bin/sh\n")
            native_env.update({"RMUX_TMPDIR": directory, "TMUX_TMPDIR": directory,
                               "TERM": "xterm-256color", "LC_ALL": "C"})

            class FixtureAdapter(bench.Adapter):
                def command(self, socket, arguments):
                    # Keep the private TMPDIR + socket inside Darwin's sockaddr_un
                    # length. The operation IDs and measured command args stay intact.
                    private_socket = "r5-" + hashlib.sha256(socket.encode()).hexdigest()[:16]
                    command = super().command(private_socket, arguments)
                    return [command[0], "-f", str(config), *command[1:]]

            def recorded_run(command, **kwargs):
                try:
                    result = real_run(command, **kwargs)
                except Exception as error:
                    report["launches"].append({"argv": command, "status": "failed",
                                               "error": str(error),
                                               "stderr": (getattr(error, "stderr", b"") or b"").decode(errors="replace")})
                    raise
                report["launches"].append({"argv": command, "returncode": result.returncode,
                                           "stdout_sha256": hashlib.sha256(result.stdout).hexdigest()})
                if "new-session" in command and result.returncode == 0:
                    prefix = command[:command.index("new-session")]
                    identity = real_run([*prefix, "display-message", "-p", "#{pid}"],
                                        env=native_env, timeout=5)
                    pid = int(identity.stdout.strip())
                    if pid not in watchers:
                        queue = select.kqueue()
                        try:
                            queue.control([select.kevent(pid, filter=select.KQ_FILTER_PROC,
                                                         flags=select.KQ_EV_ADD | select.KQ_EV_ONESHOT,
                                                         fflags=select.KQ_NOTE_EXIT)], 0, 0)
                        except BaseException:
                            queue.close()
                            raise
                        watchers[pid] = queue
                return result

            with patch.object(bench, "clean_env", return_value=native_env), \
                    patch.object(driver, "run", side_effect=recorded_run):
                for tool, binary in (("rmux", args.rmux), ("tmux", args.tmux)):
                    binary = binary.resolve(strict=True)
                    report["binaries"][tool] = {"path": str(binary), "sha256": digest(binary),
                                                 "version": bench.captured([str(binary), "-V"])}
                    adapter = FixtureAdapter(tool, binary)
                    for operation in operations:
                        try:
                            getattr(bench, operation)(adapter)  # elapsed value deliberately discarded
                        except Exception as error:
                            report["cases"].append({"tool": tool, "operation": operation,
                                                    "status": "failed", "error": str(error)})
                        else:
                            report["cases"].append({"tool": tool, "operation": operation, "status": "pass"})
            for pid, queue in watchers.items():
                events = queue.control(None, 1, 5)
                exited = bool(events and events[0].ident == pid and events[0].fflags & select.KQ_NOTE_EXIT)
                report["daemon_cleanup"].append({"pid": pid, "exit_event_observed": exited})
    except Exception as error:
        report["error"] = str(error)
    finally:
        for queue in watchers.values():
            queue.close()
    report["completed_at"] = datetime.now(timezone.utc).isoformat()
    report["passed"] = ("error" not in report and len(report["cases"]) == 2 * len(operations)
                        and all(case["status"] == "pass" for case in report["cases"])
                        and len(report["daemon_cleanup"]) == 2 * len(operations)
                        and all(case["exit_event_observed"] for case in report["daemon_cleanup"]))
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"passed": report["passed"], "cases": report["cases"],
                      "daemon_cleanup": report["daemon_cleanup"], "evidence": str(args.out)}))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
