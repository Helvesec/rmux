"""Lightweight acceptance: matching unexecuted scenes must never pass."""
import base64
import copy
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "error_contract", Path(__file__).with_name("error_contract.py"))
contract = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(contract)


def encoded(value):
    return base64.b64encode(value).decode()


def scene():
    return {
        "binding": {
            "before": {"exit": 0, "stdout": encoded(b"yes\n"), "stderr": ""},
            "after": {"exit": 0, "stdout": "", "stderr": ""},
            "error_visible": True, "client_exit": 0, "terminal_sha256": "hash",
        },
        "control": {"exit": 0, "stderr": "", "blocks": [
            {"terminal": "%end", "body": []},
            {"terminal": "%end", "body": ["CONTROL_BEFORE"]},
            {"terminal": "%error", "body": ["channel contract-unlocked not locked"]},
            {"terminal": "%end", "body": ["CONTROL_AFTER"]},
            {"terminal": "%end", "body": []},
        ]},
    }


class ReachabilityAcceptance(unittest.TestCase):
    def run_matrix(self, root, matrices):
        binary = root / "binary"
        binary.write_bytes(b"fixture, never executed")
        names = iter(("tmux", "full", "tiny"))
        with patch.object(contract.Peer, "matrix", side_effect=lambda _: matrices[next(names)]), \
                patch.object(contract, "capture", return_value={
                    "exit": 0, "stdout": encoded(b"tmux 3.7b\n"), "stderr": ""}), \
                patch("sys.argv", ["error_contract.py", "--tmux", str(binary),
                                   "--binary", str(binary), "--tiny", str(binary),
                                   "--output", str(root / "report")]):
            return contract.main()

    def test_unreached_scenes_are_rejected_even_when_both_peers_match(self):
        for peer in ("tmux", "full", "tiny", "all"):
            for defect in ("binding", "control", "interrupted-control"):
                with self.subTest(peer=peer, defect=defect), tempfile.TemporaryDirectory() as tmp:
                    matrices = {name: scene() for name in ("tmux", "full", "tiny")}
                    for name, matrix in matrices.items():
                        if peer not in (name, "all"):
                            continue
                        if defect == "binding":
                            matrix["binding"]["before"]["stdout"] = ""
                            matrix["binding"]["error_visible"] = False
                        elif defect == "control":
                            matrix["control"]["blocks"] = []
                        else:
                            matrix["control"]["blocks"] = matrix["control"]["blocks"][:3]
                    with self.assertRaisesRegex(RuntimeError, "not reached"):
                        self.run_matrix(Path(tmp), matrices)

    def test_reached_scenes_still_compare_every_field(self):
        with tempfile.TemporaryDirectory() as tmp:
            matrices = {name: scene() for name in ("tmux", "full", "tiny")}
            self.assertFalse(self.run_matrix(Path(tmp), matrices))
        with tempfile.TemporaryDirectory() as tmp:
            matrices = {name: scene() for name in ("tmux", "full", "tiny")}
            matrices["full"]["binding"]["after"]["stdout"] = encoded(b"bad\n")
            self.assertTrue(self.run_matrix(Path(tmp), matrices))

    def test_binding_requires_both_witnesses_and_a_finished_client(self):
        for defect in ("before", "error", "client"):
            with self.subTest(defect=defect), tempfile.TemporaryDirectory() as tmp:
                matrices = {name: scene() for name in ("tmux", "full", "tiny")}
                for matrix in matrices.values():
                    binding = matrix["binding"]
                    if defect == "before":
                        binding["before"]["stdout"] = ""
                    elif defect == "error":
                        binding["error_visible"] = False
                    else:
                        binding["client_exit"] = -15
                with self.assertRaisesRegex(RuntimeError, "not reached"):
                    self.run_matrix(Path(tmp), matrices)

    def test_control_rejects_an_unclosed_block(self):
        control = scene()["control"]
        raw = "".join("%begin 1 1 1\n" + "".join(line + "\n" for line in block["body"])
                      + block["terminal"] + " 1 1 1\n" for block in control["blocks"])
        raw += "%begin 1 2 1\n"
        peer = contract.Peer("unused", Path("unused"), {})
        with patch.object(peer, "run", return_value={
                "exit": 0, "stderr": "", "stdout": encoded(raw.encode())}):
            with self.assertRaisesRegex(RuntimeError, "not reached"):
                peer.control()


if __name__ == "__main__":
    unittest.main()
