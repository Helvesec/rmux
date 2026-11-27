"""Inert acceptance for release identities across the complete v1 line."""

import argparse
from dataclasses import replace
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from release_tag_policy import PolicyError, ReleaseTagIdentity, parse_message

ROOT = Path(__file__).resolve().parents[2]
VALID = ("v0.11.0", "v1.0.0", "v1.1.0", "v1.99.7", "v1.100.0", "v2.0.0")
INVALID = (
    "v01.1.0", "v1.01.0", "v1.1.00", "v1.1.0-rc.0", "v1.1.0-rc.01",
    "v1.1.0-beta.1", "v1.1.0+build.1", "v1.1", "v1.1.0.1", "1.1.0",
    "v1.1.0-rc.", "v1.1.0-rc.1.2", "v1.1.0\n",
)


def identity(ref):
    return ReleaseTagIdentity(
        release_ref=ref, release_intent_id="inert-release-contract",
        release_kind="rc" if "-rc." in ref else "stable",
        source_git_sha="a" * 40, candidate_run_id=1,
        candidate_manifest_artifact_id=2,
        candidate_manifest_artifact_digest="sha256:" + "b" * 64,
        candidate_manifest_sha256="c" * 64, release_policy_root_sha256="d" * 64,
    )


def load(name):
    path = ROOT / "scripts/release" / (name + ".py")
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class PublicationV1ContractTest(unittest.TestCase):
    def test_first_crate_is_checked_before_any_upload(self):
        publisher = load("publish-crate-set")
        content = b"canonical bytes"
        packages = [{"name": name, "sha256": hashlib.sha256(content).hexdigest()}
                    for name in ("rmux-ipc", "rmux-bootstrap", "rmux")]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            args = argparse.Namespace(source_sha="a" * 40, release_ref="v0.11.0",
                                      payload_dir=root, source_dir=root, target_dir=root)
            for existing, error in ((None, "onboarding"), (b"wrong", "bytes differ")):
                with self.subTest(error=error), patch.dict(os.environ, CARGO_REGISTRY_TOKEN="fixture"), \
                     patch.object(publisher, "package_set_file"), patch.object(publisher, "unpack"), \
                     patch.object(publisher, "validate_source_checkout"), patch.object(publisher, "prepare_target_dir"), \
                     patch.object(publisher, "validate", return_value=packages), \
                     patch.object(publisher, "registry_bytes", return_value=existing) as lookup, \
                     patch.object(publisher, "run_cargo", side_effect=AssertionError("unexpected upload")), \
                     self.assertRaisesRegex(ValueError, error):
                    publisher.execute(args)
                lookup.assert_called_once_with("rmux-bootstrap", "0.11.0")
        with patch.object(publisher, "registry_bytes", return_value=content):
            publisher.verify_first_publication(packages, "0.11.0")

    def test_signed_message_stable_rc_and_invalid_versions(self):
        with patch("subprocess.run", side_effect=AssertionError("external execution")):
            for stable in VALID:
                for suffix in ("", "-rc.1", "-rc.99"):
                    ref = stable + suffix
                    with self.subTest(ref=ref):
                        value = identity(ref)
                        self.assertEqual(parse_message(value.message()), value)
                        self.assertEqual(value.version, ref[1:])
                        with self.assertRaises(PolicyError):
                            replace(value, release_kind="stable" if suffix else "rc").validate()
            for ref in INVALID:
                with self.subTest(ref=ref), self.assertRaises(PolicyError):
                    identity(ref).validate()

    def test_rehearsal_v1_order_and_invalid_versions_before_execution(self):
        module = load("publication-dry-run")
        # The repository's inert xtask alias is deliberately forbidden in a
        # publication source's ancestors. Keep this independent source outside it,
        # in the writable temporary directory rather than the checkout's parent.
        with tempfile.TemporaryDirectory(prefix=".rmux-publication-v1-") as directory:
            root = Path(directory).resolve()
            source = root / "source"
            source.mkdir()
            for index, ref in enumerate((*VALID, *(ref + "-rc.99" for ref in VALID))):
                version = ref[1:].split("-rc.")[0]
                args = argparse.Namespace(source_sha="a" * 40, source_dir=source,
                    version=version, release_ref=ref, target_dir=root / str(index),
                    cargo=root / "toolchain" / ("cargo.exe" if os.name == "nt" else "cargo"))
                packages = [
                    {"id": name, "name": name, "version": version, "publish": publish,
                     "dependencies": deps}
                    for name, publish, deps in (
                        ("rmux", None, [{"name": "rmux-core", "path": "/core", "kind": None}]),
                        ("rmux-core", None, [{"name": "rmux-types", "path": "/types",
                                              "kind": "build", "target": "cfg(windows)"}]),
                        ("rmux-types", None, [{"name": "rmux", "path": "/rmux", "kind": "dev",
                                              "req": "*"}]),
                        ("private", [], []),
                    )
                ]
                calls = []

                def cargo(command, **kwargs):
                    calls.append(command)
                    if command[1] == "metadata":
                        data = {"workspace_members": [p["id"] for p in packages], "packages": packages}
                        return subprocess.CompletedProcess(command, 0, json.dumps(data), "")
                    self.assertEqual(command[1:-1], ["publish", "--dry-run", "--locked", "--package"])
                    return subprocess.CompletedProcess(command, 0, "inert fixture", "")

                with self.subTest(ref=ref), \
                     patch.object(module.publisher, "validate_source_checkout", return_value=source), \
                     patch.object(module.subprocess, "run", side_effect=cargo), \
                     patch.object(module.publisher, "registry_bytes", side_effect=AssertionError("network")), \
                     patch.object(module.publisher, "run_cargo", side_effect=AssertionError("publication")):
                    report = module.execute(args)
                    self.assertEqual(report["status"], "success")
                    self.assertEqual(report["publish_order"], ["rmux-types", "rmux-core", "rmux"])
                    self.assertEqual([c[-1] for c in calls[1:]], report["publish_order"])
            for index, ref in enumerate(INVALID):
                args.release_ref = ref
                args.version = ref.removeprefix("v").split("-rc.")[0]
                args.target_dir = root / f"invalid-{index}"
                with self.subTest(ref=ref), \
                     patch.object(module.publisher, "validate_source_checkout",
                                  side_effect=AssertionError("invalid identity reached checkout")), \
                     patch.object(module.subprocess, "run", side_effect=AssertionError("external execution")), \
                     self.assertRaisesRegex(ValueError, "release ref|version"):
                    module.execute(args)
                self.assertFalse(args.target_dir.exists())


if __name__ == "__main__":
    unittest.main()
