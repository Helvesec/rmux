"""A real pane process: emit requested bytes and record terminal replies."""
import json
import os
from pathlib import Path
import select
import sys
import time
import threading
import queue
from file_exchange import publish_json, read_json

if os.name != "nt":
    import tty

root = Path(sys.argv[1])
root.mkdir(parents=True, exist_ok=True)
if os.name == 'nt':
    import ctypes
    import msvcrt
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel.GetConsoleMode.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_uint)]
    kernel.SetConsoleMode.argtypes = [ctypes.c_void_p, ctypes.c_uint]
    for fd, add, remove in [(0, 0x200, 6), (1, 5, 0)]:
        handle = msvcrt.get_osfhandle(fd)
        mode = ctypes.c_uint()
        assert kernel.GetConsoleMode(handle, ctypes.byref(mode))
        assert kernel.SetConsoleMode(handle, (mode.value & ~remove) | add)
    received = queue.Queue()
    def reader():
        while True:
            data = os.read(0, 4096)
            if not data: break
            received.put(data)
    threading.Thread(target=reader, daemon=True).start()
else:
    tty.setraw(0)
(root / 'ready').write_text(str(os.getpid()))
last = 0
while True:
    try:
        request = read_json(root / 'request.json')
    except (FileNotFoundError, ValueError):
        time.sleep(.005)
        continue
    if request['id'] <= last:
        time.sleep(.005)
        continue
    last = request['id']
    os.write(1, bytes.fromhex(request['bytes']))
    reply = bytearray()
    deadline = time.monotonic() + .12
    while time.monotonic() < deadline:
        if os.name == 'nt':
            try:
                reply.extend(received.get(timeout=max(.001, deadline - time.monotonic())))
            except queue.Empty:
                pass
        elif select.select([0], [], [], max(0, deadline - time.monotonic()))[0]:
            data = os.read(0, 65536)
            if not data:
                sys.exit(0)
            reply.extend(data)
    publish_json(root / 'reply.json', {'id': last, 'bytes': reply.hex()})
