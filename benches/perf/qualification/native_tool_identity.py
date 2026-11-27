#!/usr/bin/env python3
"""One bounded private Python version helper on Mac; no RMUX/tmux execution."""
import ctypes
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import subprocess
import sys
import time
from unittest.mock import patch

from test_tool_identity import Fixture, b, pin, q, save


class BSDInfo(ctypes.Structure):
    _fields_ = [(name, ctypes.c_uint32) for name in (
        "flags", "status", "xstatus", "pid", "ppid", "uid", "gid", "ruid", "rgid", "svuid", "svgid", "rfu")]
    _fields_ += [("comm", ctypes.c_char * 16), ("name", ctypes.c_char * 32)]
    _fields_ += [(name, ctypes.c_uint32) for name in ("nfiles", "pgid", "pjobc", "tdev", "tpgid", "nice")]
    _fields_ += [("start_sec", ctypes.c_uint64), ("start_usec", ctypes.c_uint64)]


def birth(pid):
    # Exact PID returned by this test's Popen; never enumerate system processes.
    lib = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    lib.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64, ctypes.c_void_p, ctypes.c_int]
    lib.proc_pidinfo.restype = ctypes.c_int
    info = BSDInfo()
    count = lib.proc_pidinfo(pid, 3, 0, ctypes.byref(info), ctypes.sizeof(info))
    if count != ctypes.sizeof(info) or info.pid != pid or info.ppid != os.getpid():
        raise RuntimeError("owned helper PID/birth not established")
    return {"pid": info.pid, "parent_pid": info.ppid, "birth_seconds": info.start_sec, "birth_microseconds": info.start_usec}


def main():
    if sys.platform != "darwin":
        raise SystemExit("Mac-only finite private helper control")
    directory = Path(sys.argv[1]).absolute()
    fixture = Fixture(directory, tools={"rmux"})
    helper = fixture.paths["rmux"]
    helper.write_text("#!" + sys.executable + " -B\nimport sys,time\n"
                      "assert sys.argv[1:] == ['-V']\ntime.sleep(0.2)\nprint('L5-008 PRIVATE PYTHON HELPER 1')\n")
    helper.chmod(0o700)
    fixture.freeze_receipt()
    original_popen = subprocess.Popen
    original_version = b.version
    children = []
    report = {"start": datetime.now(timezone.utc).isoformat(), "pass": False,
              "helper": pin(helper), "interpreter": pin(Path(sys.executable).resolve()),
              "product_runs": 0, "performance_evidence": False, "source_receipts_are_synthetic": True,
              "child_cap": 4, "child_timeout_seconds": 5, "cleanup_budget_seconds": 30}

    class ObservedPopen(original_popen):
        def __init__(self, argv, *args, **kwargs):
            if len(children) >= 4 or argv != [str(helper), "-V"]:
                raise RuntimeError("unexpected process outside native fixture")
            self.launched_ns = time.monotonic_ns()
            super().__init__(argv, *args, **kwargs)
            children.append(self)
            self.identity = birth(self.pid)

    try:
        with patch.object(subprocess, "Popen", ObservedPopen):
            fixture.collect(version=original_version)
        artifact = fixture.artifact()
        assert artifact["complete"] and not q.validate_artifact(artifact)
        assert artifact["tool_identity"]["manifest_data"]["tools"]["rmux"]["version"]["value"] == "L5-008 PRIVATE PYTHON HELPER 1"
        assert len(children) == 1 and len(fixture.calls) == 1
        report["pass"] = True
    except BaseException as error:
        report["error"] = {"type": type(error).__name__, "message": str(error)}
        raise
    finally:
        cleanup_start = time.monotonic()
        report["children"] = []
        for child in children:
            rescued = False
            if child.poll() is None:
                if birth(child.pid) != child.identity:
                    raise RuntimeError("owned helper identity changed before cleanup")
                child.kill(); child.communicate(timeout=5); rescued = True
            reaped = False
            try:
                os.waitpid(child.pid, os.WNOHANG)
            except ChildProcessError:
                reaped = True
            report["children"].append({**child.identity, "argv": child.args, "returncode": child.returncode,
                "already_reaped": reaped, "cleanup_rescue": rescued,
                "observed_lifetime_upper_seconds": (time.monotonic_ns() - child.launched_ns) / 1e9})
            if child.returncode != 0 or not reaped or rescued:
                report["pass"] = False
        report["cleanup_seconds"] = time.monotonic() - cleanup_start
        report["pass"] = report["pass"] and report["cleanup_seconds"] < 30
        report["end"] = datetime.now(timezone.utc).isoformat()
        save(directory / "NATIVE-RECEIPT.json", report)
    print(json.dumps(report, indent=2))
    return 0 if report["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
