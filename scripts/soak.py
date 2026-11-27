#!/usr/bin/env python3
"""Run the versioned soak alone (brief for smoke checks), or from qualification_recipe.

Uses an existing clean candidate identity context; never builds or starts a gate.
Resource monitors remain the responsibility of the qualification environment.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import sys
import time

from soak_profile import soak_profile


def run_soak(profile, backend, *, clock=time):
    if profile != soak_profile(profile["mode"], profile["duration_seconds"]):
        raise ValueError("soak profile differs from version 1")
    operations = dict.fromkeys(profile["minimum_operations"], 0)
    report = {"schema": 1, "profile": profile, "status": "FAIL", "skipped": 0,
              "operations": operations, "postconditions": {}, "elapsed_seconds": 0}
    started = None

    def exercise(worker, names, sequence):
        for name in names:
            if backend.exercise(name, worker, sequence) is not True:
                raise ValueError("soak operation failed: " + name)
        return names

    try:
        backend.setup(profile)
        started = clock.monotonic()
        duration = profile["duration_seconds"]
        with ThreadPoolExecutor(max_workers=profile["concurrency"]) as pool:
            for sequence in range(duration * 2):
                delay = started + sequence / 2 - clock.monotonic()
                if delay > 0:
                    clock.sleep(delay)
                if clock.monotonic() >= started + duration:
                    raise ValueError("soak throughput minimum missed")
                names = ["input", "capture"] + (["churn"] if sequence % 2 == 0 else [])
                futures = [pool.submit(exercise, worker, names, sequence)
                           for worker in range(profile["concurrency"])]
                for future in futures:
                    for name in future.result():
                        operations[name] += 1
        remaining = started + duration - clock.monotonic()
        if remaining > 0:
            clock.sleep(remaining)
        report["elapsed_seconds"] = clock.monotonic() - started
        checks = backend.verify(profile)
        if not isinstance(checks, dict):
            raise ValueError("soak postconditions absent")
        report["postconditions"] = dict(checks)
        if (set(checks) != set(profile["postconditions"])
                or any(value is not True for value in checks.values())):
            raise ValueError("soak postcondition failed or absent")
        if any(operations[name] < minimum
               for name, minimum in profile["minimum_operations"].items()):
            raise ValueError("soak operation minimum missed")
        report["status"] = "PASS_BRIEF" if profile["mode"] == "brief" else "PASS_LONG"
    except Exception as error:
        report["error"] = str(error)
    finally:
        if started is not None:
            report["elapsed_seconds"] = clock.monotonic() - started
        try:
            cleaned = backend.cleanup() is True
        except Exception as error:
            cleaned = False
            report["cleanup_error"] = str(error)
        report["postconditions"]["cleanup"] = cleaned
        if not cleaned:
            report["status"] = "FAIL"
            report.setdefault("error", "soak cleanup failed")
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=("brief", "long"), default="long")
    parser.add_argument("--duration", type=int, help="whole seconds; long minimum 86400")
    args = parser.parse_args(argv)
    try:
        profile = soak_profile(args.mode, args.duration)
        from soak_runtime import CandidateBackend
        backend = CandidateBackend()
        report = run_soak(profile, backend)
        report["executables"] = backend.executables
        print(json.dumps(report, sort_keys=True, allow_nan=False))
        return 0 if report["status"] != "FAIL" else 1
    except Exception as error:
        print(json.dumps({"status": "FAIL", "error": str(error), "skipped": 0}))
        return 1


if __name__ == "__main__":
    sys.exit(main())
