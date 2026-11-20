"""Isolated Unix pane fixture; requests and observations are audit files."""
import json
import os
from pathlib import Path
import select
import sys
import time
import tty


def main():
    root = Path(sys.argv[1])
    tty.setraw(0)
    (root / "pid").write_text(str(os.getpid()))
    deadline = time.monotonic() + 90
    pending = bytearray()
    step = 0
    while time.monotonic() < deadline:
        path = root / f"{step}.request.json"
        if not path.exists():
            if select.select([0], [], [], 0.01)[0]:
                pending.extend(os.read(0, 8192))
            continue
        request = json.loads(path.read_text())
        output = bytes.fromhex(request["output"])
        if output:
            os.write(1, output)
        (root / f"{step}.ready").write_text("ready")
        expected = bytes.fromhex(request["expected"])
        end = time.monotonic() + (5 if expected else 0.2)
        while time.monotonic() < end and (not expected or len(pending) < len(expected)):
            if select.select([0], [], [], min(0.02, max(0, end - time.monotonic())))[0]:
                pending.extend(os.read(0, 8192))
        actual = bytes(pending)
        pending.clear()
        result = dict(output=output.hex(), expected=expected.hex(), actual=actual.hex(), passed=actual == expected)
        tmp = root / "result.tmp"
        tmp.write_text(json.dumps(result))
        tmp.rename(root / f"{step}.result.json")
        step += 1
    raise SystemExit("fixture deadline")


if __name__ == "__main__":
    main()
