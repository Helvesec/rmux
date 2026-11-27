#!/usr/bin/env python3
"""Native PTY fixture: deterministic cells, cursor transitions and mouse receipts."""
import os
from pathlib import Path
import sys
import termios
import tty

receipt = Path(sys.argv[1])
tty.setraw(0)
rows, cols = 50, 140

def draw():
    output = '\x1b[?25l\x1b[2J'
    for row in range(rows):
        line = ''.join(f'{row:02d}:{col:03d}|' for col in range(20))
        output += f'\x1b[{row+1};1H' + line
    output += '\x1b[1;1H\x1b[?25h'
    os.write(1, output.encode())

draw()
receipt.write_text('ready\n')
while True:
    data = os.read(0, 4096)
    if not data:
        break
    with receipt.open('a') as out:
        out.write(data.hex() + '\n')
    # Only the explicit harness key changes output. Mouse escapes are recorded.
    if data == b'f': os.write(1, b'\x1b[40;121H')
    elif data == b'm': os.write(1, b'\x1b[26;81H')
    elif data == b'h': os.write(1, b'\x1b[?25l')
    elif data == b'v': os.write(1, b'\x1b[?25h')
    elif data == b'o': os.write(1, b'\x1b[1;1H')
    elif data == b'w': os.write(1, b'\x1b[10;21HUPDATED\x1b[1;1H')
    elif data == b'e': os.write(1, b'\x1b[?1000h\x1b[?1006h')
