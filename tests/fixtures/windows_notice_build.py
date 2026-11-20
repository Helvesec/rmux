"""Exercise the notice build driver; Cargo payloads are fixtures, not PE proof."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from unittest.mock import patch

root = Path(sys.argv[1]).resolve()
spec = importlib.util.spec_from_file_location("notices", root / "scripts/packaging/notices.py")
notices = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notices)
flags = "-C debuginfo=1 -C target-feature=+crt-static"
base = ["cargo", "build", "--package", "rmux", "--locked", "--target",
        "x86_64-pc-windows-msvc", "--release", "--message-format=json-render-diagnostics"]
expected = [base + ["--bin", "rmux"],
            base + ["--features", "tiny-cli", "--bin", "rmux"],
            base + ["--bin", "rmux-daemon"]]
roles = ["full", "tiny", "daemon"]
real_run = subprocess.run
suffix = ".exe" if sys.platform == "win32" else ""

for failure in [None, 0, 1, 2]:
    with tempfile.TemporaryDirectory(prefix="rmux-notice-crt-") as temporary:
        directory = Path(temporary)
        seen = []

        def cargo(command, **kwargs):
            index = len(seen)
            assert command == expected[index], (command, expected[index])
            assert kwargs["cwd"] == notices.ROOT
            assert "env" not in kwargs, "Cargo must inherit the caller's CRT flags"
            seen.append(command)
            # Use an actual child to observe inheritance at the subprocess boundary.
            result = real_run([sys.executable, "-c",
                               "import json, os; print(json.dumps(os.environ['RUSTFLAGS']))"],
                              **kwargs)
            assert result.returncode == 0
            if index == failure:
                return subprocess.CompletedProcess(command, 23)
            name = "rmux-daemon" if index == 2 else "rmux"
            (directory / (name + suffix)).write_bytes(roles[index].encode())
            return subprocess.CompletedProcess(command, 0)

        def record(binary_dir, logs):
            assert binary_dir == directory
            assert [path.name for path in logs] == [role + ".jsonl" for role in roles]
            assert all(json.loads(path.read_text()) == flags for path in logs)
            assert (directory / ("rmux" + suffix)).read_bytes() == b"tiny"
            assert (directory / ("rmux-full" + suffix)).read_bytes() == b"full"
            assert (directory / ("rmux-daemon" + suffix)).read_bytes() == b"daemon"

        with patch.dict(os.environ, {"RUSTFLAGS": flags}), \
                patch.object(notices.subprocess, "run", side_effect=cargo), \
                patch.object(notices, "record", side_effect=record) as recorded:
            try:
                notices.build(directory, ["--", *base[:-1]])
            except RuntimeError as error:
                assert failure is not None and "child exited 23" in str(error), error
            else:
                assert failure is None, "a failed Cargo build was accepted"
            assert seen == expected[:3 if failure is None else failure + 1]
            assert recorded.call_count == (1 if failure is None else 0)
            assert os.environ["RUSTFLAGS"] == flags

print("full/tiny/daemon: exact argv, inherited CRT flags, payloads and fail-closed builds PASS")
