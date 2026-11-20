"""F1/F2 collector regressions with synthetic packages, never native evidence."""
import itertools
import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from installed_wire_migration import Migration
from installed_consumer_diagnostics import validate_wire_refusal


class Package:
    def __init__(self, root, wire, state, fixture=True, corrupt=None):
        self.prefix = root / str(wire)
        self.version = {8: "0.10.0", 9: "0.11.0"}[wire]
        self.wire, self.state, self.fixture, self.corrupt = wire, state, fixture, corrupt

    def verify(self):
        path = str(self.prefix / "bin/rmux")
        return {"path": path, "installed_path": path, "sha256": "a" * 64,
                "installed_sha256": "a" * 64, "wire": self.wire, "version": self.version}

    def verify_process(self, pid):
        return {"path": str(self.prefix / "bin/rmux-daemon"), "sha256": "b" * 64, "pid": pid}

    def call(self, args, env):
        origin = self.verify()
        record = {"client": origin, "command": [origin["path"], *map(str, args)],
                  "returncode": 0, "stdout": "", "stderr": ""}
        if args == ["--version"]:
            record["stdout"] = "rmux " + self.version
            return record
        endpoint = str(args[1])
        command = args[4] if args[2] == "-f" else args[2]
        current = self.state.get(endpoint)
        if command in ("start-server", "new-session") and current is None:
            self.state["next_pid"] = self.state.get("next_pid", 100) + 1
            current = self.state[endpoint] = {"pid": self.state["next_pid"], "sessions": [], "wire": self.wire}
        if current is None:
            record["returncode"] = 1
        elif current["wire"] != self.wire:
            record.update(returncode=1, stderr=(
                f"rmux: incompatible protocol {current['wire']}; client supports protocol {self.wire}.\n"
                f"rmux: run `rmux -S {endpoint} kill-server` to stop it, then retry.\n"))
            if self.corrupt:
                record["stderr"] = self.corrupt(record["stderr"], endpoint)
        elif command == "kill-server":
            del self.state[endpoint]
        elif command == "new-session":
            current["sessions"].append(args[-1])
        elif command == "list-sessions":
            record["stdout"] = "\n".join(current["sessions"])
        elif command == "display-message":
            record["stdout"] = str(current["pid"] if args[-1] == "#{pid}" else len(current["sessions"]))
        return record


class CorrectionSpec(unittest.TestCase):
    def test_migration_accepts_legacy_cli_with_only_short_version_option(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            shared = {}
            old, new = (Package(root, wire, shared) for wire in (8, 9))
            original_call = old.call

            def legacy_call(args, env):
                if args == ["--version"]:
                    record = original_call(["--version"], env)
                    record.update(returncode=1, stdout="", stderr="usage: rmux [-V] [command]\n")
                    return record
                if args == ["-V"]:
                    record = original_call(["--version"], env)
                    record["command"] = [old.verify()["path"], "-V"]
                    return record
                return original_call(args, env)

            old.call = legacy_call
            receipt = Migration(old, new, root / "cells").run()
            self.assertEqual(receipt["status"], "pass")
            self.assertEqual(len(receipt["cells"]), 12)
            self.assertEqual(shared, {"next_pid": shared["next_pid"]})

    def test_snapshot_rejects_endpoint_replacement_during_collection(self):
        for wire, sessions in itertools.product((8, 9), ([], ["kept"])):
            with self.subTest(wire=wire, sessions=sessions), tempfile.TemporaryDirectory() as directory:
                root = Path(directory).resolve()
                endpoint = str(root / "socket")
                shared = {endpoint: {"pid": 123, "sessions": sessions, "wire": wire}}
                package = Package(root, wire, shared, fixture=False)
                other = Package(root, 17 - wire, shared, fixture=False)
                runner = Migration(package, other, root / "cells")
                runner.current = {"commands": []}
                original_call = package.call

                def replace_after_pid(args, env):
                    record = original_call(args, env)
                    if args[-1] == "#{pid}" and shared[endpoint]["pid"] == 123:
                        shared[endpoint] = {"pid": 124, "sessions": sessions, "wire": wire}
                    return record

                package.call = replace_after_pid
                with self.assertRaisesRegex(ValueError, "daemon changed during snapshot"):
                    runner.snapshot(package, endpoint)

    def test_validator_rejects_diagnostic_redefined_by_observation(self):
        for client_wire, daemon_wire in ((8, 9), (9, 8)):
            for fixture in (True, False):
                for endpoint in ("/private/socket", "/private/a'b socket", r"\\.\pipe\private-socket"):
                    with self.subTest(pair=(client_wire, daemon_wire), fixture=fixture, endpoint=endpoint):
                        root = Path("/private/prefix")
                        client, daemon = (Package(root, wire, {}) for wire in (client_wire, daemon_wire))
                        advice = endpoint if fixture or endpoint == "/private/socket" else "'" + endpoint.replace("'", "'\\''") + "'"
                        message = (f"rmux: incompatible protocol {daemon_wire}; client supports protocol {client_wire}." if fixture else
                                   f"rmux: running daemon from an older release uses incompatible protocol {daemon_wire}; this client supports protocol {client_wire} on '{endpoint}'.")
                        published_client = not fixture and (client_wire, daemon_wire) == (8, 9)
                        if published_client:
                            message = f"rmux: running daemon on '{endpoint}' uses an incompatible protocol."
                        expected = message + f"\nrmux: run `rmux -S {advice} kill-server` to stop it, then retry."
                        before = {"pid": 123, "sessions": ["kept"]}
                        record = {"client": client.verify(), "command": [client.verify()["path"], "-S", endpoint, "start-server"],
                                  "fixture": fixture, "returncode": 1, "expected_returncode": 1,
                                  "diagnostic": expected, "stderr": expected + "\n", "before": before, "after_refusal": before,
                                  "restart": {"client": daemon.verify(), "command": [daemon.verify()["path"], "-S", endpoint, "kill-server"],
                                              "returncode": 0, "explicit": True, "sessions_lost_acknowledged": True}}
                        validate_wire_refusal(record)
                        for wrong in (expected.replace("an incompatible protocol.", "incompatible protocol 99.") if published_client else
                                      expected.replace(f"incompatible protocol {daemon_wire}", "incompatible protocol 99"),
                                      expected.replace("running daemon on", "running daemon from an older release on") if published_client else
                                      expected.replace(f"supports protocol {client_wire}", "supports protocol 99"),
                                      expected.replace("-S " + advice, "-S wrong-endpoint"),
                                      expected + "\nadditional unrelated failure"):
                            broken = copy.deepcopy(record)
                            broken.update(stderr=wrong, diagnostic=wrong)
                            with self.assertRaises(ValueError):
                                validate_wire_refusal(broken)

    def test_wrong_versions_and_advised_endpoint_fail_both_refusal_pairs(self):
        mutations = {
            "generic": lambda text, endpoint: "incompatible protocol 99; kill-server",
            "daemon_wire": lambda text, endpoint: text.replace("incompatible protocol ", "incompatible protocol 99"),
            "client_wire": lambda text, endpoint: text.replace("supports protocol ", "supports protocol 99"),
            "endpoint": lambda text, endpoint: text.replace("-S " + endpoint, "-S wrong-endpoint"),
        }
        for pair, state, (name, corrupt) in itertools.product(((8, 9), (9, 8)), ("idle", "active"), mutations.items()):
            with self.subTest(pair=pair, state=state, mutation=name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory).resolve()
                shared = {}
                client = Package(root, pair[0], shared, corrupt=corrupt)
                daemon = Package(root, pair[1], shared)
                runner = Migration(client, daemon, root / "cells")
                with self.assertRaises(ValueError):
                    runner.cell(0, client, daemon, state)
                self.assertEqual(runner.current["status"], "fail")
                self.assertNotIn(runner.current["endpoint"], shared)

    def test_absent_snapshot_retains_serialized_daemon_image_for_all_four_pairs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            shared = {}
            old, new = (Package(root, wire, shared, fixture=False) for wire in (8, 9))
            runner = Migration(old, new, root / "cells")
            for index, (client, daemon) in enumerate(itertools.product((old, new), repeat=2)):
                runner.cell(index, client, daemon, "absent")
                saved = json.loads((runner.output / "receipt.json").read_text())["cells"][-1]
                snapshot = saved.get("after_start")
                self.assertIsNotNone(snapshot, "absent cell discarded its verified daemon snapshot")
                self.assertEqual(snapshot["sessions"], [])
                self.assertEqual(snapshot["daemon_image"], client.verify_process(snapshot["pid"]))
                self.assertEqual(saved["effective_daemon_wire"], client.wire)


if __name__ == "__main__":
    unittest.main()
