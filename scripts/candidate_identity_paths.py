"""Short Windows target names keep the full HEAD and all 128 command bits."""
import base64
from pathlib import Path
import re


def windows_directory(g, source, head, command=None):
    # The source is private and the HEAD is complete; this layout is Windows only.
    # Neither the public consumer names nor its nested target are shortened.
    root = Path(source).resolve() / '.rmux-audit' / head
    if command is None:
        return root
    if re.fullmatch('[0-9a-f]{32}', command):
        raw = bytes.fromhex(command)
    elif re.fullmatch('[a-z2-7]{26}', command):
        raw = base64.b32decode(command.upper() + '======')
    else:
        raise g.IdentityError('qualified command identity required')
    encoded = base64.b32encode(raw).decode().lower().rstrip('=')
    if len(raw) != 16 or (len(command) == 26 and command != encoded):
        raise g.IdentityError('noncanonical command identity required')
    return root / encoded
