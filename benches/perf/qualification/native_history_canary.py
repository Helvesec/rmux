#!/usr/bin/env python3
"""Finite own-helper counterexample for ordinary Darwin process notifications.

No product is instrumented or suspended. Known helper commands execute freely;
the observer subscribes only to its direct, unreaped child. Coalesced notifications
and text-vnode snapshots cannot certify every intermediate image/descendant.
"""
import json
import os
from pathlib import Path
import select
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "scripts/bench"))
import darwin_driver as driver
import tool_identity


def fork_helpers():
    receipts = []
    for _ in range(2):
        pid = os.fork()
        if pid == 0:
            os.execl("/bin/sh", "sh", "-c", "exec /usr/bin/true")
        collected, status = os.waitpid(pid, 0)
        receipts.append({"pid": collected, "raw_wait_status": status,
                         "exit_code": os.waitstatus_to_exitcode(status)})
    print(json.dumps({"fixture_owned_waits": receipts}), flush=True)
    return int(any(row["exit_code"] for row in receipts))


def observe(argv):
    queue = select.kqueue()
    notifications = []; snapshots = []; registration = {}

    def callback(stage, pid):
        observation = tool_identity.darwin_process_image(pid, expected_parent=os.getpid())
        snapshots.append({"stage": stage, **observation})
        if stage == "spawn":
            registration.update(at_ns=time.perf_counter_ns(), after_posix_spawn=True)
            try:
                queue.control([select.kevent(pid, filter=select.KQ_FILTER_PROC,
                    flags=select.KQ_EV_ADD | select.KQ_EV_CLEAR,
                    fflags=select.KQ_NOTE_EXEC | select.KQ_NOTE_FORK | select.KQ_NOTE_EXIT)], 0, 0)
                registration["success"] = True
            except ProcessLookupError:
                registration.update(success=False, cause="child exited before subscription")
        else:
            for event in queue.control(None, 16, 0):
                if event.ident != pid or event.flags & select.KQ_EV_ERROR:
                    raise RuntimeError("own process notification mismatch")
                notifications.append({"ident": event.ident, "flags": event.flags,
                    "fflags": event.fflags, "data": event.data,
                    "observed_ns": time.perf_counter_ns(), "numeric_exit_status_claim": False})
        return {"stage": stage, **observation}

    try:
        result = driver.run(argv, env={"PATH": "/usr/bin:/bin", "PYTHONDONTWRITEBYTECODE": "1"},
                            timeout=10, observer=callback)
        return {"argv": argv, "pid": result.pid, "returncode": result.returncode,
                "driver": result.observation, "stdout": result.stdout.decode(),
                "stderr": result.stderr.decode(), "registration": registration,
                "notifications": notifications, "snapshots": snapshots,
                "complete_history": False,
                "cause": "No pre-spawn subscription, image list/count or fork-child identity in these notifications"}
    finally:
        queue.close()


def main():
    if sys.argv[1:] == ["--fork-helper"]:
        return fork_helpers()
    output = Path(sys.argv[1]); output.mkdir(mode=0o700)
    cases = [
        ("fast_exec_chain", ["/bin/sh", "-c", "exec /bin/bash --noprofile --norc -c 'exec /usr/bin/true'"],
         {"success_requires": ["/bin/sh", "/bin/bash", "/usr/bin/true"], "same_pid_exec_transitions": 2}),
        ("two_forks_with_exec_delegation", [sys.executable, "-B", str(Path(__file__).resolve()), "--fork-helper"],
         {"own_direct_child": "Python", "forks": 2, "each_child_execs": ["/bin/sh", "/usr/bin/true"]}),
    ]
    rows = []
    for name, argv, truth in cases:
        row = {"case": name, "fixture_source_expectations": truth, **observe(argv)}
        if name == "two_forks_with_exec_delegation":
            waits = json.loads(row["stdout"])["fixture_owned_waits"]
            if len(waits) != 2 or any(v["exit_code"] != 0 for v in waits):
                raise RuntimeError("helper did not collect both owned children")
            row["fixture_owned_waits"] = waits
            row["fork_notification_records"] = sum(bool(e["fflags"] & select.KQ_NOTE_FORK)
                                                    for e in row["notifications"])
        rows.append(row)
        (output / (name + ".json")).write_text(json.dumps(row, indent=2) + "\n")
    result = {"cases": rows, "attempts": 2,
              "native_canary_execution": "PASS", "complete_history_qualification": "FAIL",
              "capability": tool_identity.DARWIN_HISTORY_CAPABILITY,
              "event_loss_detection": "No loss counter or per-exec image list; absence/completeness remains unknown",
              "cleanup": "Direct helper waitpid records and two fixture-reported own waits; no detached helpers",
              "no_product_or_global_observation": True}
    (output / "RESULT.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({k: result[k] for k in ("attempts", "native_canary_execution", "complete_history_qualification")}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
