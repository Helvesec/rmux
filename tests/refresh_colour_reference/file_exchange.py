"""Atomic probe messages with bounded retries for Windows sharing violations."""
import json
import os
import time


def retry_sharing(operation):
    deadline = time.monotonic() + 1
    while True:
        try:
            return operation()
        except PermissionError:
            # A reader can briefly deny rename/delete on Windows. In
            # particular, opening the replacement during rename can fail too.
            if os.name != 'nt' or time.monotonic() >= deadline:
                raise
            time.sleep(.005)


def read_json(path):
    return json.loads(retry_sharing(path.read_text))


def publish_json(path, value):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value))
    retry_sharing(lambda: temporary.replace(path))
