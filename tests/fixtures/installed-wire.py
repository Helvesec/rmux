"""Process/file fixture only. No RMUX wire/native product evidence is emitted."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

VERSION = "@VERSION@"
WIRE = int("@WIRE@")


def save(endpoint, value):
    temporary = endpoint.with_suffix(".tmp")
    temporary.write_text(json.dumps(value))
    temporary.replace(endpoint)


def start(endpoint):
    if endpoint.exists():
        return
    # This executable is itself part of the installed fixture package.
    child = subprocess.Popen([sys.executable, __file__, "--daemon", str(endpoint)],
                             stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                             stderr=subprocess.DEVNULL, start_new_session=True)
    deadline = time.monotonic() + 5
    while not endpoint.exists():
        if child.poll() is not None or time.monotonic() >= deadline:
            raise RuntimeError("fixture daemon startup failed")
        time.sleep(0.01)


def main():
    args = sys.argv[1:]
    if args[0] == "--daemon":
        endpoint = Path(args[1])
        save(endpoint, {"pid": os.getpid(), "wire": WIRE, "sessions": []})
        def stop(*_):
            endpoint.unlink()
            sys.exit(0)
        signal.signal(signal.SIGTERM, stop)
        while True:
            time.sleep(1)
    if args[0] in ("--version", "-V"):
        print("rmux " + VERSION)
        return 0
    if args[0] == "--help":
        print("usage: rmux installed migration fixture")
        return 0
    assert args[0] == "-S"
    endpoint = Path(args[1])
    args = args[2:]
    if args[0] == "-f":
        args = args[2:]
    command = args[0]
    if command in ("start-server", "new-session"):
        start(endpoint)
    if not endpoint.exists():
        return 1
    state = json.loads(endpoint.read_text())
    if state["wire"] != WIRE:
        print(f"rmux: incompatible protocol {state['wire']}; client supports protocol {WIRE}.\n"
              f"rmux: run `rmux -S {endpoint} kill-server` to stop it, then retry.", file=sys.stderr)
        return 1
    if command == "kill-server":
        os.kill(state["pid"], signal.SIGTERM)
        deadline = time.monotonic() + 5
        while endpoint.exists():
            if time.monotonic() >= deadline:
                return 2
            time.sleep(0.01)
    elif command == "new-session":
        state["sessions"].append(args[args.index("-s") + 1])
        save(endpoint, state)
    elif command == "list-sessions":
        print("\n".join(state["sessions"]))
    elif command == "display-message":
        print(state["pid"] if args[-1] == "#{pid}" else len(state["sessions"]))
    elif command != "start-server":
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
