"""Consumer identity regression; persistence calls are simulated, never native cells."""
import contextlib
import hashlib
import io
import pathlib
import runpy
import shlex
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import persistence_transition

SCRIPT = pathlib.Path(__file__).with_name("persistence_acceptance.py")
REAL_RUN = subprocess.run


class TransitionConsumers(unittest.TestCase):
    def test_qualified_images_are_enforced_until_publication(self):
        for direction in ("same", "upgrade", "rollback"):
            for phase in ("alias", "initial", "switch", "plugin", "direct", "publish"):
                with self.subTest(direction=direction, phase=phase):
                    self.check_consumers(direction, phase)

    def check_consumers(self, direction, phase):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory).resolve(strict=True)
            paths = [root / name for name in ("saving", "restoring")]
            aliases = [root / name for name in ("save-alias", "restore-alias")]
            output = root / "output"
            for index, path in enumerate(paths):
                path.write_text("#!/bin/sh\nprintf 'image " + str(index) + "\\n'\n")
                path.chmod(0o755)
                aliases[index].symlink_to(path)
            pins = [hashlib.sha256(p.read_bytes()).hexdigest() for p in paths]
            versions = {"same": [(0, 10, 0), (0, 10, 0)],
                        "upgrade": [(0, 9, 0), (0, 10, 0)],
                        "rollback": [(0, 10, 0), (0, 9, 0)]}[direction]
            state = {"restoring": False, "mutated": False, "divergent": []}
            actual_validate = persistence_transition.validate_transition

            def mutate():
                paths[1].write_text("#!/bin/sh\nprintf 'divergent image\\n'\n")
                state["mutated"] = True

            def version(path):
                index = paths.index(path)
                if phase == "alias":
                    aliases[index].unlink()
                    aliases[index].symlink_to(paths[1 - index])
                return versions[index]

            def qualify(*args):
                result = actual_validate(*args)
                if phase == "initial":
                    mutate()
                return result

            def command(argv, *args, **kwargs):
                if argv[0] == "/bin/bash":
                    # Actually exercise the generated wrapper with marker fixtures.
                    wrapper = REAL_RUN([str(output / "bin/tmux"), "probe"],
                                       text=True, capture_output=True, timeout=30)
                    expected = "image " + str(int(state["restoring"]))
                    if wrapper.stdout.strip() != expected:
                        state["divergent"].append("wrapper/plugin")
                    if pathlib.Path(argv[1]).name.endswith("save.sh"):
                        (output / "save").mkdir(exist_ok=True)
                        (output / "save/last").write_text("state\t0\t0\n")
                        (output / "save-done").touch()
                    return subprocess.CompletedProcess(argv, 0, "", "")
                index = int(state["restoring"])
                image = pathlib.Path(argv[0])
                if image != paths[index] or hashlib.sha256(image.read_bytes()).hexdigest() != pins[index]:
                    state["divergent"].append(list(argv))
                operation = argv[3:]
                stdout, rc = "", 0
                if "display-message" in operation:
                    stdout = "424242\n" if operation[-1] == "#{pid}" else "sleep\n"
                elif "list-panes" in operation:
                    stdout = "restore-fixture:0.0|/fixture|saved-title|b001,100x30,0,0,0\n"
                    if state["restoring"] and "#{pane_pid}" in operation[-1]:
                        if phase == "direct" and not state["mutated"]:
                            mutate()
                        elif phase == "publish":
                            state["ready_to_publish"] = True
                elif "source-file" in operation:
                    rc, stdout = 1, "layout invalid\n"
                elif operation == ["kill-server"] and not state["restoring"]:
                    state["restoring"] = True
                    if phase == "switch":
                        mutate()
                if phase == "plugin" and state["restoring"] and "@continuum-restore" in operation:
                    mutate()
                if phase == "publish" and state.get("ready_to_publish") and "list-panes" in operation:
                    # Mutation after the last consumer's output, before JSON publication.
                    if state.get("publication_read"):
                        mutate()
                    state["publication_read"] = True
                return subprocess.CompletedProcess(argv, rc, stdout, "")

            argv = [str(SCRIPT), str(aliases[0]), str(root / "plugins"), str(output),
                    "clean", "resurrect", str(aliases[1]), direction]
            emitted = io.StringIO()
            error = None
            with mock.patch.object(sys, "argv", argv), \
                    mock.patch.object(persistence_transition, "image_version", version), \
                    mock.patch.object(persistence_transition, "validate_transition", qualify), \
                    mock.patch.object(subprocess, "run", command), contextlib.redirect_stdout(emitted):
                try:
                    runpy.run_path(str(SCRIPT), run_name="__main__")
                except ValueError as exc:
                    error = exc
            self.assertEqual(state["divergent"], [], "consumed an image other than the qualified pin")
            if phase == "alias":
                self.assertIsNone(error)
                self.assertTrue(emitted.getvalue(), "canonical images should complete")
            else:
                self.assertTrue(state["mutated"])
                self.assertIsInstance(error, ValueError)
                self.assertIn("persistence transition image changed", str(error))
                self.assertFalse(emitted.getvalue(), "mutated pin published a valid result")


if __name__ == "__main__":
    unittest.main()
