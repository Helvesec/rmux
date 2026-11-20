"""Unix startup query, then the unchanged R byte-exchange worker."""
import json
import os
from pathlib import Path
import runpy
import select
import sys
import time
import tty

root = Path(sys.argv[1])
root.mkdir(parents=True, exist_ok=True)
tty.setraw(0)
os.write(1, b'\x1b[?996n')
reply = bytearray()
deadline = time.monotonic() + .25
while time.monotonic() < deadline:
    if select.select([0], [], [], max(0, deadline - time.monotonic()))[0]:
        data = os.read(0, 4096)
        if not data:
            sys.exit(0)
        reply.extend(data)
(root / 'initial.json').write_text(json.dumps({
    'pid': os.getpid(), 'tty': [os.isatty(i) for i in range(3)], 'reply': reply.hex(),
}))
support = Path(__file__).resolve().parents[1] / 'refresh_colour_reference'
sys.path.insert(0, str(support))
runpy.run_path(str(support / 'worker.py'), run_name='__main__')
