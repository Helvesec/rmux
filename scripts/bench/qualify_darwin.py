#!/usr/bin/env python3
"""Native common-harness qualification; never a public performance certificate.

The same existing Darwin driver executes both products. A frozen manifest is
required before any launch. Unknown image/exec history stays a failing R6 line.
"""
from __future__ import annotations

import argparse
import copy
import ctypes
import datetime
import errno
import hashlib
import json
import os
from pathlib import Path
import shlex
import socket
import stat
import sys
import time

import darwin_driver as driver
import qualification as gate
import tool_identity

# Strong references retain owned kqueues if closure becomes unknown. The CLI
# then parks for an explicit operator disposition instead of dropping ownership
# and admitting another lot.
UNCLOSED_FIXTURES = []


def pin(path):
    path = Path(path)
    raw = path.read_bytes()
    return {"path": str(path), "bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest()}


def write_once(path, value):
    with Path(path).open("x") as stream:
        json.dump(value, stream, indent=2)
        stream.write("\n")


def checkpoint(path, value):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def thermal():
    """Read NSProcessInfo.thermalState without changing host settings."""
    row = {"observed_ns": time.perf_counter_ns(), "state": None,
           "source": "NSProcessInfo.thermalState", "stabilization_proven": False}
    try:
        ctypes.CDLL("/System/Library/Frameworks/Foundation.framework/Foundation")
        objc = ctypes.CDLL("/usr/lib/libobjc.A.dylib")
        objc.objc_getClass.argtypes = [ctypes.c_char_p]
        objc.objc_getClass.restype = ctypes.c_void_p
        objc.sel_registerName.argtypes = [ctypes.c_char_p]
        objc.sel_registerName.restype = ctypes.c_void_p
        send = ctypes.CFUNCTYPE(ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p)(
            ("objc_msgSend", objc))
        get_integer = ctypes.CFUNCTYPE(ctypes.c_long, ctypes.c_void_p, ctypes.c_void_p)(
            ("objc_msgSend", objc))
        obj = send(objc.objc_getClass(b"NSProcessInfo"), objc.sel_registerName(b"processInfo"))
        if not obj:
            raise RuntimeError("NSProcessInfo unavailable")
        row["state"] = get_integer(obj, objc.sel_registerName(b"thermalState"))
    except Exception as error:
        row["error"] = str(error)
    return row


def aggregate_cpu_ticks():
    """Host aggregate counters only; no task or process enumeration."""
    lib = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
    lib.mach_host_self.restype = ctypes.c_uint
    lib.host_statistics.argtypes = [ctypes.c_uint, ctypes.c_int,
                                    ctypes.POINTER(ctypes.c_int), ctypes.POINTER(ctypes.c_uint)]
    lib.host_statistics.restype = ctypes.c_int
    lib.mach_port_deallocate.argtypes = [ctypes.c_uint, ctypes.c_uint]
    lib.mach_port_deallocate.restype = ctypes.c_int
    host = lib.mach_host_self()
    try:
        ticks = (ctypes.c_uint * 4)()
        count = ctypes.c_uint(4)
        code = lib.host_statistics(host, 3, ctypes.cast(ticks, ctypes.POINTER(ctypes.c_int)),
                                   ctypes.byref(count))
        if code or count.value != 4:
            raise RuntimeError("HOST_CPU_LOAD_INFO unavailable: " + str(code))
        return list(ticks)
    finally:
        task = ctypes.c_uint.in_dll(lib, "mach_task_self_").value
        if lib.mach_port_deallocate(task, host):
            raise RuntimeError("host counter observation port not released")


def quiet_window(cohort_id, reservation, checkpoint_path):
    """Exactly one prospective60s window shared by both products; no retries."""
    window = {"cohort_id": cohort_id, "policy": copy.deepcopy(gate.QUIET_POLICY),
              "source": "host_statistics.HOST_CPU_LOAD_INFO+NSProcessInfo.thermalState",
              "attempt_number": 1, "reservation": reservation, "own_builds": 0,
              "products": ["tmux", "rmux"], "samples": [], "errors": [],
              "started_at": datetime.datetime.now(datetime.timezone.utc).isoformat()}
    start = time.perf_counter_ns()
    try:
        while True:
            ticks = aggregate_cpu_ticks()
            pressure = thermal()
            window["samples"].append({"ns": time.perf_counter_ns(), "cpu_ticks": ticks,
                                      "thermal_state": pressure["state"], "pressure": pressure})
            checkpoint(checkpoint_path, window)
            if window["samples"][-1]["ns"] - window["samples"][0]["ns"] >= 60_000_000_000:
                break
            # Thermal sampling is not a product readiness mechanism. Product
            # work uses the existing fresh FIFO/kqueue challenge independently.
            next_sample = start + len(window["samples"]) * 2_000_000_000
            time.sleep(max(0, (next_sample - time.perf_counter_ns()) / 1e9))
    except Exception as error:
        window["errors"].append(error_record(error))
    window["start_ns"] = window["samples"][0]["ns"] if window["samples"] else start
    window["end_ns"] = window["samples"][-1]["ns"] if window["samples"] else time.perf_counter_ns()
    window["finished_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    window["qualification_errors"] = gate.quiet_window_errors(
        window, {"quiet_policy": gate.QUIET_POLICY, "cohort_id": cohort_id}, [])
    window["qualified"] = not window["qualification_errors"]
    checkpoint(checkpoint_path, window)
    return window


def identity_observer(stage, pid):
    return {"stage": stage, **tool_identity.darwin_process_image(pid, expected_parent=os.getpid())}


def process_key(observation):
    if observation.get("before") != observation.get("after") or not observation.get("before"):
        raise RuntimeError("owned process birth not stable")
    return observation["before"]


def error_record(error):
    return {"type": type(error).__name__, "message": str(error),
            "returncode": getattr(error, "returncode", None)}


class Fixture:
    def __init__(self, root, manifest, binaries):
        self.root, self.manifest, self.binaries = root, manifest, binaries
        root.mkdir(mode=0o700)
        for name in ("home", "xdg", "tmp"):
            (root / name).mkdir(mode=0o700)
        self.endpoint = root / "s"
        self.config = root / "config"
        self.config.write_text("set-option -g default-shell /bin/bash\n"
                               "set-option -g default-command ''\n"
                               "set-option -g exit-empty off\n")
        self.env = {"HOME": str(root / "home"), "XDG_CONFIG_HOME": str(root / "xdg"),
                    "TMPDIR": str(root / "tmp"), "PATH": "/usr/bin:/bin",
                    "TERM": "xterm", "LANG": "C", "SHELL": "/bin/bash",
                    "BASH_ENV": "/dev/null", "ENV": "/dev/null",
                    "RMUX_DISABLE_TMUX_SHIM": "1"}
        self.base = [binaries[manifest["tool"]]["path"], "-f", str(self.config), "-S", str(self.endpoint)]
        self.commands, self.owned = [], {}
        self.endpoint_stamp = None

    def check_disk(self):
        for ref in self.binaries.values():
            if pin(ref["path"]) != ref:
                raise RuntimeError("frozen executable changed: " + ref["path"])

    def command(self, args, *, check=True):
        self.check_disk()
        if self.base[0] != self.binaries[self.manifest["tool"]]["path"]:
            raise RuntimeError("measured executable differs from frozen selected artifact")
        result = driver.run(self.base + args, env=self.env,
                            timeout=self.manifest["timeout_seconds"], check=check,
                            observer=identity_observer)
        self.commands.append({"argv": result.args, "pid": result.pid,
                              "returncode": result.returncode,
                              "stdout": result.stdout.decode(errors="replace"),
                              "stderr": result.stderr.decode(errors="replace"),
                              "driver": result.observation, "elapsed_ms": result.elapsed_ms})
        return result

    def hold(self, pid, role):
        if pid in self.owned:
            observation = tool_identity.darwin_process_image(pid)
            if process_key(observation) != self.owned[pid]["key"]:
                raise RuntimeError("fixture process changed")
            return observation
        observation = tool_identity.darwin_process_image(pid)
        key = process_key(observation)
        watch = driver.FixtureProcess(pid)
        again = tool_identity.darwin_process_image(pid)
        if process_key(again) != key:
            watch.close()
            raise RuntimeError("fixture identity changed while registering exit")
        self.owned[pid] = {"key": key, "watch": watch, "role": role, "image": observation}
        if self.endpoint_stamp is None and self.endpoint.exists():
            s = self.endpoint.lstat()
            if not stat.S_ISSOCK(s.st_mode) or s.st_uid != os.getuid():
                raise RuntimeError("private endpoint type/owner differs")
            self.endpoint_stamp = [s.st_dev, s.st_ino, s.st_uid]
        return observation

    def daemon(self):
        pid = int(self.command(["display-message", "-p", "#{pid}"]).stdout.strip())
        return self.hold(pid, "daemon")

    def warm(self):
        self.command(["start-server", ";", "set-option", "-g", "exit-empty", "off"])
        return self.daemon()

    def close(self):
        start = time.monotonic()
        result = {"closed": False, "rescue": False, "processes": []}
        deadline = start + self.manifest["cleanup_seconds"]
        try:
            if not self.owned and self.endpoint.exists():
                self.daemon()
            live = [v for v in self.owned.values() if not v["watch"].poll_exit()]
            if live:
                self.command(["kill-server"], check=False)
            for pid, item in self.owned.items():
                try:
                    item["watch"].wait(max(.001, deadline - time.monotonic()))
                except Exception:
                    # Rescue only a held, still-identical fixture process. Its
                    # use remains red even when it disposes of the fixture.
                    current = tool_identity.darwin_process_image(pid)
                    if current.get("before") != item["key"]:
                        raise RuntimeError("cleanup lost owned identity")
                    result["rescue"] = True
                    item["watch"].retire_after_failure(max(.001, deadline - time.monotonic()))
                after = tool_identity.darwin_process_image(pid)
                absent = after.get("before") is None
                result["processes"].append({"identity": item["key"], "role": item["role"],
                                            "NOTE_EXIT": item["watch"].exited,
                                            "birth_absent": absent,
                                            "numeric_wait_status": None,
                                            "wait_note": "indirect process; no numeric reap inferred"})
                if not absent:
                    raise RuntimeError("fixture birth still observable after exit")
            if self.endpoint.exists():
                s = self.endpoint.lstat()
                if [s.st_dev, s.st_ino, s.st_uid] != self.endpoint_stamp:
                    raise RuntimeError("cleanup endpoint identity changed")
                with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as probe:
                    probe.settimeout(.5)
                    code = probe.connect_ex(str(self.endpoint))
                result["endpoint_errno"] = code
                if code != errno.ECONNREFUSED:
                    raise RuntimeError("private endpoint still accepts connections or is unknown")
                self.endpoint.unlink()
            result["closed"] = not self.endpoint.exists()
        except BaseException as error:
            result["error"] = error_record(error)
        finally:
            result["seconds"] = time.monotonic() - start
            # Keep an explicit owner and admission stop if closure is unknown.
            if result["closed"]:
                for item in self.owned.values():
                    item["watch"].close()
            else:
                result["owner"] = "current qualification process; no new lot admitted"
                UNCLOSED_FIXTURES.append(self)
                write_once(self.root / "UNKNOWN-CLOSURE.json", result)
        return result


def sample(root, manifest, binaries, identity, *, fault=None):
    fixture = Fixture(root, manifest, binaries)
    attempt = {"identity": identity, "status": "running", "timeout": False,
               "thermal": [thermal()], "commands": fixture.commands,
               "path_context": {"config": str(fixture.config), "socket": str(fixture.endpoint)}}
    try:
        fixture.check_disk()
        if fixture.endpoint.exists():
            raise RuntimeError("fresh fixture contains residual endpoint")
        before_daemon = None
        if identity["regime"] == "warm" or fault == "residual":
            before_daemon = process_key(fixture.warm())
        attempt["state_before"] = {"endpoint_present": fixture.endpoint.exists(),
                                   "daemon": before_daemon}
        # FIFO challenge and all environment/path preparation occur before the
        # measured command. Sending and validating it is an independent check.
        with driver.Readiness() as readiness:
            attempt["environment_prepared_ns"] = time.perf_counter_ns()
            argv = ["new-session", "-d", "-s", "proof", "-x", "80", "-y", "24",
                    "/bin/bash", "--noprofile", "--norc"]
            if fault == "wrong-artifact":
                fixture.base[0] = "/usr/bin/true"
                try:
                    fixture.command(argv)
                finally:
                    # Restore only the command path for fixture disposal, not
                    # for another attempt. The sample remains a terminal error.
                    fixture.base[0] = binaries[manifest["tool"]]["path"]
            measured = fixture.command(argv)
            attempt.update(client_pid=measured.pid, returncode=measured.returncode,
                           elapsed_ms=measured.elapsed_ms, driver=measured.observation,
                           timed_argv=measured.args)
            daemon = fixture.daemon()
            public = fixture.command(["list-panes", "-a", "-F",
                "#{pid}|#{session_name}|#{pane_pid}|#{window_width}|#{window_height}"])
            lines = public.stdout.decode().splitlines()
            if len(lines) != 1:
                raise RuntimeError("qualification requires exactly one live pane")
            dpid, session, ppid, width, height = lines[0].split("|")
            if session != "proof" or int(dpid) != daemon["pid"]:
                raise RuntimeError("unexpected public session/daemon identity")
            pane = fixture.hold(int(ppid), "pane")
            signal_command = readiness.shell_signal
            if fault == "omit-work":
                signal_command = ":"
            elif fault == "fixed-cache":
                signal_command = "printf '%s\\n' " + "0" * 64 + " > " + shlex.quote(str(readiness.path))
            fixture.command(["send-keys", "-l", "-t", "proof:0.0", signal_command])
            fixture.command(["send-keys", "-t", "proof:0.0", "Enter"])
            readiness.wait(manifest["readiness_seconds"])
            sessions = fixture.command(["list-sessions", "-F", "#{session_name}"]).stdout.decode().splitlines()
            attempt["state_after"] = {"endpoint_present": fixture.endpoint.exists(),
                                      "daemon": process_key(daemon)}
            attempt["work"] = {"challenge": readiness.challenge, "acknowledged": True,
                "readiness": "fresh_fifo_kqueue", "session_count": len(sessions),
                "pane_count": len(lines), "size": [int(width), int(height)],
                "daemon_pid": int(dpid), "pane_pid": int(ppid), "observed_pane": pane}
            fixture.check_disk()
            roles = manifest.get("source_role_graph")
            client_roles = roles["client_roles"] if roles else [manifest["tool"]]
            expected_cdhashes = [manifest["code_directory_hashes"][r] for r in client_roles]
            events = measured.observation["events"]
            client_bound = any(e.get("mapped_cdhash") in expected_cdhashes
                               and e.get("status") == "observed_text_vnode" for e in events)
            daemon_role = roles["daemon_role"] if roles else (
                "rmux_daemon" if manifest["tool"] == "rmux" else "tmux")
            daemon_bound = (daemon.get("status") == "observed_text_vnode"
                            and daemon.get("mapped_cdhash") == manifest["code_directory_hashes"][daemon_role])
            observed_roles = [role for role, value in manifest["code_directory_hashes"].items()
                              if daemon.get("mapped_cdhash") == value]
            attempt["image_identity"] = {"disk_unchanged": True, "client_events": events,
                "daemon": daemon, "client_observation_bound": client_bound,
                "daemon_observation_bound": daemon_bound,
                "expected_daemon_role": daemon_role, "observed_daemon_roles": observed_roles,
                "all_measured_images_bound": False,
                "exec_fork_history_complete": False,
                "unexpected_images": [] if daemon_bound else [{"role": "daemon",
                    "expected_artifact_role": daemon_role, "observed_artifact_roles": observed_roles}],
                "cause": "bounded text-vnode observations do not establish every transient helper/exec"}
            attempt["status"] = "pass"
    except BaseException as error:
        attempt.update(status="failed", error=error_record(error),
                       timeout=isinstance(error, (TimeoutError, __import__("subprocess").TimeoutExpired)))
    finally:
        attempt["thermal"].append(thermal())
        attempt["cleanup"] = fixture.close()
        if not attempt["cleanup"]["closed"] or attempt["cleanup"]["rescue"]:
            attempt["status"] = "failed"
    return attempt


def run_pair(manifests, output, reservation):
    """One finite, paired functional population, preserving each product's gate.

    The sole shared quiet window is retained even when red. Functional sampling
    may continue, but neither thermal nor strict baseline can become PASS.
    """
    if set(manifests) != {"tmux", "rmux"}:
        raise ValueError("both exact product manifests required")
    output.mkdir(mode=0o700)
    states = {}
    start = time.monotonic()
    for tool in ("tmux", "rmux"):
        manifest = manifests[tool]
        if (manifest["tool"] != tool or manifest["samples_per_cell"] != 4
                or manifest["quiet_policy"] != gate.QUIET_POLICY
                or manifest["cohort_id"] != manifests["tmux"]["cohort_id"]
                or manifest["paired_order"] != ["tmux", "rmux"]):
            raise ValueError("paired population/policy changed")
        if gate.role_graph_errors(manifest):
            raise ValueError("invalid frozen role graph")
        refs = manifest["source_pins"] + list(manifest["binaries"].values())
        refs += [manifest["package_receipt"], reservation]
        refs += manifest["source_role_graph"]["source_pins"]
        for ref in refs:
            if pin(ref["path"]) != ref:
                raise RuntimeError("frozen paired input changed: " + ref["path"])
    # Validate both manifests before even a diagnostic child is launched.
    for tool in ("tmux", "rmux"):
        manifest = manifests[tool]
        (output / tool).mkdir(mode=0o700)
        state = {"manifest": manifest, "manifest_sha256": gate.digest(manifest),
                 "started_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                 "performance_claim": False, "sampling_finished": False,
                 "cleanup_finished": False, "attempts": [], "errors": [], "statistics": {},
                 "installed_package_bound": manifest["installed_package_bound"],
                 "floor": {"diagnostic_only": True, "raw_ms": [], "commands": []}}
        states[tool] = state
        for _ in range(4):
            r = driver.run(["/usr/bin/true"], env={"PATH": "/usr/bin:/bin"}, timeout=5,
                           observer=identity_observer)
            state["floor"]["raw_ms"].append(r.elapsed_ms)
            state["floor"]["commands"].append({"argv": r.args, "pid": r.pid,
                "returncode": r.returncode, "driver": r.observation})
    window = quiet_window(manifests["tmux"]["cohort_id"], reservation, output / "QUIET.json")
    plans = {tool: gate.qualification_plan(m) for tool, m in manifests.items()}
    order = []; failed = False
    for index in range(8):
        for tool in ("tmux", "rmux"):
            if time.monotonic() - start > 540 or UNCLOSED_FIXTURES:
                states[tool]["errors"].append("campaign deadline or unknown closure; remaining NOT_RUN")
                failed = True
                break
            state = states[tool]
            expected = plans[tool][index]
            state["next_intent"] = expected
            checkpoint(output / tool / "RESULT.json", state)
            attempt = sample(output / tool / str(index + 1), manifests[tool],
                             manifests[tool]["binaries"], expected)
            attempt["quiet_window_id"] = window["cohort_id"]
            state["attempts"].append(attempt)
            order.append({"tool": tool, "sequence": index + 1, "identity": expected})
            checkpoint(output / tool / "RESULT.json", state)
            if attempt["status"] != "pass":
                state["errors"].append("first operation failed; remaining population NOT_RUN")
                failed = True
                break
        if failed:
            break
    for tool, state in states.items():
        manifest = manifests[tool]
        state["quiet_window"] = window
        state["sampling_finished"] = len(state["attempts"]) == len(plans[tool])
        state["cleanup_finished"] = all(a["cleanup"]["closed"] for a in state["attempts"])
        state["unattempted"] = plans[tool][len(state["attempts"]):]
        for op in manifest["operations"]:
            values = [a["elapsed_ms"] for a in state["attempts"]
                      if a["identity"]["operation"] == op["id"] and a["status"] == "pass"]
            if values:
                state["statistics"][op["id"]] = gate.sample_statistics(
                    values, seed=manifest["statistics"]["seed"], resamples=manifest["statistics"]["resamples"])
        state["thermal_errors"] = gate.quiet_window_errors(window, manifest, state["attempts"])
        state["functional_errors"] = gate.qualification_errors(state, state["manifest_sha256"], require_r6=False)
        state["r5_r6_errors"] = gate.qualification_errors(state, state["manifest_sha256"])
        state["functional_pass"] = not state["functional_errors"]
        state["qualified_r5_r6"] = not state["r5_r6_errors"]
        state["finished_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        checkpoint(output / tool / "RESULT.json", state)
    result = {"cohort_id": window["cohort_id"], "order": order,
              "expected_order": [{"tool": t, "sequence": i + 1, "identity": plans[t][i]}
                                 for i in range(8) for t in ("tmux", "rmux")],
              "quiet_window": pin(output / "QUIET.json"),
              "products": {t: pin(output / t / "RESULT.json") for t in states},
              "functional_pass": all(s["functional_pass"] for s in states.values()),
              "thermal_pass": all(s["sampling_finished"] and not s["thermal_errors"] for s in states.values()),
              "qualified_r5_r6": all(s["qualified_r5_r6"] for s in states.values()),
              "seconds": time.monotonic() - start, "performance_claim": False}
    if result["order"] != result["expected_order"]:
        result.update(functional_pass=False, qualified_r5_r6=False)
    checkpoint(output / "PAIR-RESULT.json", result)
    return result


def run(manifest, output, *, fault=None):
    output.mkdir(mode=0o700)
    digest = gate.digest(manifest)
    plan = gate.qualification_plan(manifest)
    references = manifest["source_pins"] + list(manifest["binaries"].values())
    if manifest.get("package_receipt"):
        references.append(manifest["package_receipt"])
    for ref in references:
        if pin(ref["path"]) != ref:
            raise RuntimeError("frozen source/tool changed: " + ref["path"])
    state = {"manifest": manifest, "manifest_sha256": digest,
             "started_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
             "performance_claim": False, "sampling_finished": False,
             "cleanup_finished": False, "attempts": [], "errors": [], "statistics": {},
             "installed_package_bound": manifest.get("installed_package_bound", False),
             "floor": {"diagnostic_only": True, "raw_ms": [], "commands": []}}
    checkpoint(output / "RESULT.json", state)
    start = time.monotonic()
    for _ in range(4):
        r = driver.run(["/usr/bin/true"], env={"PATH": "/usr/bin:/bin"}, timeout=5,
                       observer=identity_observer)
        state["floor"]["raw_ms"].append(r.elapsed_ms)
        state["floor"]["commands"].append({"argv": r.args, "pid": r.pid,
                                          "returncode": r.returncode, "driver": r.observation})
    for expected in plan:
        if time.monotonic() - start > 540:
            state["errors"].append("lot execution deadline, cleanup reserved")
            break
        # A crash leaves the planned identity visible before any operation.
        state["next_intent"] = expected
        checkpoint(output / "RESULT.json", state)
        attempt = sample(output / str(expected["sequence"]), manifest,
                         manifest["binaries"], expected, fault=fault)
        state["attempts"].append(attempt)
        checkpoint(output / "RESULT.json", state)
        if attempt["status"] != "pass":
            state["errors"].append("first operation failed; remaining population NOT_RUN")
            break
    state["sampling_finished"] = len(state["attempts"]) == len(plan)
    state["cleanup_finished"] = all(a["cleanup"]["closed"] for a in state["attempts"])
    state["unattempted"] = plan[len(state["attempts"]):]
    for op in manifest["operations"]:
        values = [a["elapsed_ms"] for a in state["attempts"]
                  if a["identity"]["operation"] == op["id"] and a["status"] == "pass"]
        if values:
            state["statistics"][op["id"]] = gate.sample_statistics(
                values, seed=manifest["statistics"]["seed"], resamples=manifest["statistics"]["resamples"])
    state["functional_errors"] = gate.qualification_errors(state, digest, require_r6=False)
    state["r5_r6_errors"] = gate.qualification_errors(state, digest)
    state["functional_pass"] = not state["functional_errors"]
    state["qualified_r5_r6"] = not state["r5_r6_errors"]
    state["finished_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    checkpoint(output / "RESULT.json", state)
    return state


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--manifest-sha256", required=True)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--paired", action="store_true",
                        help="manifest contains both frozen products and a shared quiet reservation")
    parser.add_argument("--intentional-canary", choices=("residual", "omit-work", "fixed-cache", "wrong-artifact"))
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("native Darwin required")
    manifest = json.loads(args.manifest.read_text())
    if gate.digest(manifest) != args.manifest_sha256:
        parser.error("prospectively pinned manifest differs")
    if args.paired:
        if args.intentional_canary:
            parser.error("paired baseline cannot hide a canary or retry")
        result = run_pair(manifest["manifests"], args.out.absolute(), manifest["reservation"])
    else:
        result = run(manifest, args.out.absolute(), fault=args.intentional_canary)
    print(json.dumps({"functional_pass": result["functional_pass"],
                      "qualified_r5_r6": result["qualified_r5_r6"],
                      "functional_errors": result.get("functional_errors"),
                      "r5_r6_errors": result.get("r5_r6_errors"),
                      "thermal_pass": result.get("thermal_pass")}))
    if UNCLOSED_FIXTURES:
        import signal
        write_once(args.out / "OWNERSHIP-HELD.json",
                   {"owner_pid": os.getpid(), "new_lot_admitted": False,
                    "cause": "unknown closure; owned kqueues retained until explicit disposition"})
        sys.stdout.flush()
        while UNCLOSED_FIXTURES:
            signal.pause()
    # A functional qualification command is not an R6 certificate. Its strict
    # R6 result is explicit in the artifact and must remain red when unknown.
    return 0 if result["functional_pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
