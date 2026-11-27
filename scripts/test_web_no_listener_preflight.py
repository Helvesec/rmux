"""The no-listener clock must exclude cold identity qualification, not runtime."""
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from contextlib import redirect_stdout
from unittest.mock import patch

import regression_registry as registry
import regression_registry_checks as probe
import web_no_listener_preflight as preflight


PROBE = dict(id="web-no-listener", argv=[
    "python", "scripts/regression_registry_checks.py", "web-no-listener"])


class NoListenerPreflight(unittest.TestCase):
    def test_registry_qualifies_before_original_probe_and_preserves_environment(self):
        environment = {"RMUX_IDENTITY_HEAD": "pinned", "CARGO_TARGET_DIR": "/private/target"}
        calls = []

        def run(argv, **kwargs):
            calls.append(argv)
            self.assertIs(kwargs["env"], environment)
            self.assertEqual(kwargs["cwd"], registry.ROOT)
            return subprocess.CompletedProcess(argv, 0)

        with patch.object(registry.subprocess, "run", side_effect=run), redirect_stdout(io.StringIO()):
            self.assertEqual(registry.run_commands([PROBE], env=environment), 0)
        self.assertEqual(calls, [
            [os.path.abspath(preflight.sys.executable),
             str(registry.ROOT / "scripts/web_no_listener_preflight.py")],
            [os.path.abspath(preflight.sys.executable), *PROBE["argv"][1:]]])

    def test_failed_qualification_never_runs_the_probe(self):
        for code, expected in ((17, 17), (-9, 137)):
            with self.subTest(code=code), patch.object(registry.subprocess, "run",
                    return_value=subprocess.CompletedProcess([], code)) as run, \
                    redirect_stdout(io.StringIO()):
                self.assertEqual(registry.run_commands([PROBE]), expected)
                self.assertEqual(run.call_count, 1)
                self.assertTrue(run.call_args.args[0][1].endswith("web_no_listener_preflight.py"))

    def test_neighbor_commands_and_probe_failure_keep_their_exit_codes(self):
        neighbor = dict(id="neighbor", argv=["python", "scripts/regression_registry_checks.py", "notice-build"])
        for commands, codes, expected, count in (
                ([neighbor], [0], 0, 1), ([neighbor], [13], 13, 1),
                ([PROBE, neighbor], [0, 11], 11, 2),
                ([PROBE, neighbor], [0, 0, 0], 0, 3)):
            with self.subTest(codes=codes), patch.object(registry.subprocess, "run",
                    side_effect=[subprocess.CompletedProcess([], n) for n in codes]) as run, \
                    redirect_stdout(io.StringIO()):
                self.assertEqual(registry.run_commands(commands), expected)
                self.assertEqual(run.call_count, count)

    def test_preflight_build_recipe_matches_the_unchanged_probe(self):
        class BuildObserved(Exception):
            pass

        with patch.object(probe.shutil, "which", return_value="/tool/cargo"), \
                patch.object(probe, "run", side_effect=BuildObserved) as run:
            with self.assertRaises(BuildObserved):
                probe.web_no_listener(Path("/private/target"))
        self.assertEqual(run.call_args.args[0], ["/tool/cargo", *preflight.BUILD_ARGS])

    def test_qualification_covers_alias_retained_producer_and_selected_launchers(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            alias, producer, selected = (root / name for name in ("alias", "producer", "launcher"))
            for kind in ("rust-bin", "unix-launcher"):
                receipt = root / "build.json"
                receipt.write_text(json.dumps(dict(kind=kind, path=str(producer), producer=str(producer))))
                with self.subTest(kind=kind), patch.object(preflight.identity, "verify_built",
                        return_value=dict(build=str(receipt))) as verify, \
                        patch.object(preflight.subprocess, "run",
                            return_value=subprocess.CompletedProcess([], 0, str(selected) + "\n")) as run:
                    preflight.qualify(alias, root, "head", root)
                    verify.assert_called_once_with(alias, root, "head", root)
                    self.assertEqual([call.args[0][-1] for call in run.call_args_list],
                                     list(map(str, (alias, selected, producer, selected))))
                    self.assertTrue(all(call.kwargs["check"] for call in run.call_args_list))

    def test_invalid_identity_stops_before_any_launch(self):
        with patch.object(preflight.identity, "verify_built", side_effect=preflight.identity.IdentityError("invalid")), \
                patch.object(preflight.subprocess, "run") as run:
            with self.assertRaises(preflight.identity.IdentityError):
                preflight.qualify(Path("/alias"), Path("/source"), "head", Path("/target"))
            run.assert_not_called()

    def test_preflight_selects_unique_cargo_artifact_and_propagates_build_failure(self):
        artifact = dict(reason="compiler-artifact", target=dict(name="rmux"), executable="/actual/rmux")
        for artifacts, good in (([artifact], True), ([], False), ([artifact, artifact], False)):
            with self.subTest(artifacts=artifacts), patch.object(preflight.identity, "validate_context"), \
                    patch.object(preflight.identity, "context", return_value=(Path("/source"), "head", Path("/target"))), \
                    patch.object(preflight.sys, "platform", "linux"), \
                    patch.object(preflight.shutil, "which", return_value="/tool/cargo"), \
                    patch.object(preflight.subprocess, "run", return_value=subprocess.CompletedProcess(
                        [], 0, "\n".join(map(json.dumps, artifacts)))), \
                    patch.object(preflight, "qualify") as qualify, redirect_stdout(io.StringIO()):
                if good:
                    preflight.main()
                    qualify.assert_called_once_with(Path("/actual/rmux"), Path("/source"), "head", Path("/target"))
                else:
                    with self.assertRaisesRegex(RuntimeError, "unique"):
                        preflight.main()
                    qualify.assert_not_called()
        with patch.object(preflight.identity, "validate_context"), \
                patch.object(preflight.identity, "context", return_value=(None, None, None)), \
                patch.object(preflight.sys, "platform", "linux"), \
                patch.object(preflight.shutil, "which", return_value="/tool/cargo"), \
                patch.object(preflight.subprocess, "run", side_effect=subprocess.CalledProcessError(7, [])), \
                patch.object(preflight, "qualify") as qualify:
            with self.assertRaises(subprocess.CalledProcessError):
                preflight.main()
            qualify.assert_not_called()


if __name__ == "__main__":
    unittest.main()
