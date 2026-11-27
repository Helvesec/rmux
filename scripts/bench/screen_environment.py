"""Private screen socket namespace that fits Darwin's Unix socket paths."""
from __future__ import annotations

import atexit
import errno
import os
from pathlib import Path
import secrets
import sys


_directory: Path | None = None


def _remove_empty(directory: Path, owner: int) -> None:
    if os.getpid() != owner:
        return
    try:
        directory.rmdir()
    except OSError as error:
        if error.errno not in (errno.ENOENT, errno.ENOTEMPTY, errno.EEXIST):
            raise


def prepare() -> None:
    """Prepare once, outside timers; never reuse the user's screen sessions.

    Darwin's sun_path has 104 bytes. A nine-byte directory leaves room for
    '/', a seven-digit PID, '.', the adapter's 79-byte session name, screen's
    '-query' suffix and NUL. Keep the literal /tmp spelling: resolving its
    /private/tmp symlink would spend that budget again.
    """
    global _directory
    if sys.platform != "darwin":
        return
    if _directory is None:
        for _ in range(128):
            directory = Path("/tmp") / secrets.token_hex(2)
            try:
                directory.mkdir(mode=0o700)
            except FileExistsError:
                continue
            _directory = directory
            atexit.register(_remove_empty, directory, os.getpid())
            break
        else:
            raise FileExistsError("cannot allocate a private short screen socket directory")
    os.environ["SCREENDIR"] = str(_directory)
