#!/usr/bin/env python3
"""Private CLI canary derived from R2's detached sleep/FIFO cleanup probe.

The added display-message command exposes only this fixture's daemon identity.
Failure modes exercise the actual adapter, driver and readiness validator.
"""

import json
import os
from pathlib import Path
import signal
import subprocess
import sys

state = Path(os.environ["R5_FIXTURE_STATE"])
mode = os.environ["R5_FIXTURE_MODE"]
args = sys.argv[1:]
assert args[0] == "-L"
socket, command = args[1:3]
arguments = args[3:]

if command in ("new-session", "start-server"):
    if not state.exists():
        child = subprocess.Popen(["/bin/sleep", "30"], start_new_session=True,
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                 stderr=subprocess.DEVNULL)
        state.write_text(json.dumps({"pid": child.pid, "socket": socket}))
elif command == "display-message":
    assert arguments == ["-p", "#{pid}"]
    print(json.loads(state.read_text())["pid"])
elif command == "send-keys":
    challenge = arguments[-2]
    assert challenge.startswith("printf '%s\\n' ") and " > " in challenge
    subprocess.run(["/bin/sh", "-c", challenge], check=True)
elif command in ("list-sessions", "list-windows", "list-panes"):
    if mode in ("operation_fail", "double_fail"):
        print("R5 intentional primary operation failure", file=sys.stderr)
        raise SystemExit(9)
    print("bench: fixture only")
elif command == "new-window":
    if mode == "setup_fail":
        print("R5 intentional primary setup failure", file=sys.stderr)
        raise SystemExit(10)
elif command == "kill-server":
    if not state.exists() and mode != "initial_fail":
        print(f"error connecting to /private/fixture/{socket} (No such file or directory)",
              file=sys.stderr)
        raise SystemExit(1)
    if mode == "cleanup_fail_after_exit":
        os.kill(json.loads(state.read_text())["pid"], signal.SIGKILL)
    if mode in ("cleanup_fail", "double_fail", "initial_fail", "setup_fail", "cleanup_fail_after_exit"):
        print("R2 intentional cleanup failure: owned daemon still running", file=sys.stderr)
        raise SystemExit(7)
    if mode != "false_success":
        os.kill(json.loads(state.read_text())["pid"], signal.SIGKILL)
elif command == "kill-session":
    os.kill(json.loads(state.read_text())["pid"], signal.SIGKILL)
else:
    raise SystemExit(f"unsupported fixture command: {command}")
