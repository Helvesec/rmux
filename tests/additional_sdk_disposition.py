"""Portable package-policy regressions; fixtures never execute packaged code."""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile

ROOT = Path(os.environ.get("RMUX_TEST_RELEASE_ROOT", Path(__file__).resolve().parents[1]))
RELEASE = ROOT / "scripts/release"
sys.path.insert(0, str(RELEASE))
SHA = "a" * 40


def module(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), RELEASE / f"{name}.py")
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def run(script, *args):
    return subprocess.run([sys.executable, str(RELEASE / script), *map(str, args)],
                          capture_output=True, text=True, cwd=ROOT)


def write_tar(path, payloads, mode="w:gz"):
    with tarfile.open(path, mode) as archive:
        for name, data in sorted(payloads.items()):
            info = tarfile.TarInfo(name)
            info.size = len(data)
            info.mode = 0o644
            archive.addfile(info, io.BytesIO(data))


class Disposition(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="rmux-sdk-disposition-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def assert_ok(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def assert_rejected(self, result, message):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(message, result.stderr)

    def native(self, key="macos-aarch64", version="1.0.0", extra=None, support=False):
        contract = json.loads((ROOT / ".github/release/canonical-build-contract.json").read_text(encoding="utf-8"))
        platform = next(p for p in contract["platforms"] if p["key"] == key)
        fmt = platform["archive_format"]
        windows = fmt == "zip"
        native = ("rmux.exe", "libexec/rmux/rmux.exe", "rmux-daemon.exe") if windows else (
            "bin/rmux", "libexec/rmux/rmux", "bin/rmux-daemon")
        package = f"rmux-{version}-{key}"
        files = dict.fromkeys(native + (
            "install.ps1" if windows else "install.sh", "LICENSE-APACHE", "LICENSE-MIT",
            "SHA256SUMS.txt", "rmux.1" if windows else "share/man/man1/rmux.1",
            "share/bash-completion/completions/rmux", "share/zsh/site-functions/_rmux",
            "share/fish/vendor_completions.d/rmux.fish", "share/powershell/Completions/_rmux.ps1",
            "share/elvish/lib/rmux.elv"), b"fixture, never executable\n")
        if windows:
            files["README.md"] = (ROOT / "README.md").read_bytes()
            for name in ("conpty.dll", "OpenConsole.exe", "libexec/rmux/conpty.dll",
                         "libexec/rmux/OpenConsole.exe", "share/rmux/conpty-runtime.json",
                         "share/licenses/Microsoft.Windows.Console.ConPTY/LICENSE"):
                files[name] = b"fixture, never executable\n"
        metadata = {
            "schema": 1, "artifact_kind": "windows-package-binary" if windows else "unix-package-binary",
            "rmux_version": version, "git_commit": SHA, "git_dirty": False,
            "target": platform["target_triple"], "platform_label": key, "configuration": "release",
            "package_schema": 1, "package_name": package, "package_target": platform["target_triple"],
            "package_target_label": key, "package_layout": "rmux-windows-package-v2" if windows else "rmux-package-v2",
            "archive_format": fmt, "skip_build": False, "release_artifact": True,
            "generated_at_utc": "2026-09-07T00:00:00Z",
        }
        for prefix, name in zip(("binary", "helper_binary", "daemon_binary"), native):
            metadata.update({f"{prefix}_path": name, f"{prefix}_sha256": hashlib.sha256(files[name]).hexdigest(),
                             f"{prefix}_bytes": len(files[name])})
        if windows:
            metadata["reuse_release_binaries"] = False
            metadata["conpty_runtime_manifest"] = "share/rmux/conpty-runtime.json"
        else:
            metadata["archive_reproducibility"] = "normalized-mtime-gzip-no-name"
        if key.startswith("linux-"):
            metadata.update(dict.fromkeys(("binary_glibc_min", "helper_binary_glibc_min", "daemon_binary_glibc_min",
                                          "package_glibc_min", "max_supported_glibc"), "2.31"))
        if support:
            metadata["supported_sdks"] = ["librmux", "@rmux/sdk"]
        files["share/rmux/artifact-metadata.json"] = json.dumps(metadata).encode()
        if extra:
            files[extra] = b"excluded implementation fixture\n"
        assets = self.root / f"assets-{len(list(self.root.iterdir()))}"
        assets.mkdir()
        archive = assets / f"{package}.{fmt}"
        payloads = {f"{package}/{name}": data for name, data in files.items()}
        if windows:
            with zipfile.ZipFile(archive, "w") as bundle:
                for name, data in payloads.items():
                    bundle.writestr(name, data)
        else:
            write_tar(archive, payloads)
        # Supplemental roles are already exact allowlists. These inert bytes are
        # not a claim of Debian/RPM/Snap/native or WASM qualification.
        suffixes = {"debian": "fixture.deb", "rpm": "fixture.rpm", "snap-amd64": "fixture.snap",
                    "snap-arm64": "fixture.snap", "crate-package-set": "rmux-fixture-crate-package-set.tar",
                    "wasm-byte-set": "rmux-web-crypto-wasm-fixture.tar",
                    "wasm-provenance": "rmux-web-crypto-wasm-fixture.provenance.json",
                    "chocolatey-package": "rmux.fixture.nupkg"}
        for role in platform["supplemental_roles"] + (["debian", "rpm"] if platform["linux_packages"] else []):
            if role == "chocolatey-package":
                with zipfile.ZipFile(assets / suffixes[role], "w") as bundle:
                    bundle.writestr("rmux.nuspec", '<package><metadata><id>rmux</id><version>1.0.0</version><dependencies><dependency id="vcredist140" /></dependencies></metadata></package>')
                    for name in ("tools/chocolateyInstall.ps1", "tools/chocolateyUninstall.ps1", "_rels/.rels",
                                 "[Content_Types].xml", "package/services/metadata/core-properties/abc.psmdcp"):
                        bundle.writestr(name, "inert fixture")
            else:
                (assets / suffixes[role]).write_bytes(b"supplemental role fixture")
        self.assert_ok(run("canonical-build-record.py", "checksums", "--assets-dir", assets))
        rustc = assets.parent / "rustc.txt"
        rustc.write_text(f"rustc 1.96.1\nhost: {platform['target_triple']}\nrelease: 1.96.1\ncommit-hash: {SHA}\n")
        common = ["--source-sha", SHA, "--fast-run-id", "1", "--candidate-run-id", "2",
                  "--release-intent-id", "sdk-scope-test", "--planned-release-ref", f"v{version}",
                  "--release-kind", "stable", "--platform-key", key, "--assets-dir", assets]
        record = assets.parent / f"{assets.name}.json"
        create = common + ["--candidate-run-attempt", "1", "--runner-image", platform["runner_image"],
                           "--runner-os", platform["runner_os"], "--runner-arch", platform["runner_arch"],
                           "--runner-environment", "github-hosted", "--rustc-verbose", rustc, "--output", record]
        return assets, record, common, create

    def test_current_and_10_native_producer_verifier_subjects(self):
        for version in ("0.10.0", "1.0.0"):
            for key in ("macos-aarch64", "windows-x86_64", "linux-x86_64", "linux-aarch64", "macos-x86_64"):
                with self.subTest(version=version, key=key):
                    assets, record, common, create = self.native(key, version)
                    self.assert_ok(run("canonical-build-record.py", "create", *create))
                    self.assert_ok(run("canonical-build-record.py", "verify", *common, "--record", record,
                                       "--expected-record-sha256", hashlib.sha256(record.read_bytes()).hexdigest()))
                    self.assert_ok(run("canonical-build-record.py", "subjects", "--record", record,
                                       "--assets-dir", assets, "--output", assets.parent / "subjects.txt"))

    def test_native_rejects_bundled_implementation_and_support(self):
        for key in ("macos-aarch64", "windows-x86_64"):
            for extra, support in (("sdk/librmux/__init__.py", False), ("sdk/dist/rmux.js", False),
                                   ("sdk/librmux-0.6.1.whl", False), (None, True)):
                with self.subTest(key=key, extra=extra, support=support):
                    _, _, _, create = self.native(key, extra=extra, support=support)
                    self.assert_rejected(run("canonical-build-record.py", "create", *create), "1.0")

    def test_verifier_and_subjects_reject_rehashed_bundle(self):
        for key in ("macos-aarch64", "windows-x86_64"):
            assets, record, common, create = self.native(key, extra="sdk/librmux/__init__.py")
            # Seal the fixture using the prior 0.10 behavior, then bind a 1.0
            # intent. Recomputed hashes must not substitute for the new policy.
            legacy = list(create)
            legacy[legacy.index("--planned-release-ref") + 1] = "v0.10.0"
            self.assert_ok(run("canonical-build-record.py", "create", *legacy))
            value = json.loads(record.read_text()); value["planned_release_ref"] = "v1.0.0"
            record.write_text(json.dumps(value))
            self.assert_rejected(run("canonical-build-record.py", "verify", *common, "--record", record,
                                     "--expected-record-sha256", hashlib.sha256(record.read_bytes()).hexdigest()), "1.0")
            self.assert_rejected(run("canonical-build-record.py", "subjects", "--record", record,
                                     "--assets-dir", assets, "--output", assets.parent / "subjects.txt"), "1.0")

    def test_added_distribution_and_unknown_role_still_rejected(self):
        for name in ("librmux-0.6.1.whl", "librmux-0.6.1.tar.gz", "sdk-0.6.5.tgz", "unknown.payload"):
            assets, _, _, create = self.native()
            (assets / name).write_bytes(b"distribution fixture")
            self.assert_ok(run("canonical-build-record.py", "checksums", "--assets-dir", assets))
            self.assertNotEqual(run("canonical-build-record.py", "create", *create).returncode, 0)

    def test_crate_producer_and_downstream_reader(self):
        producer = module("canonical-crate-set")
        reader = module("crate_package_set")
        for name, extra in (("rmux-sdk", None), ("librmux", None), ("rmux-sdk", "src/client.py"),
                            ("rmux-sdk", "sdk/dist/rmux.js"), ("rmux-sdk", "librmux/__init__.py"),
                            ("rmux-sdk", "sdk/librmux-0.6.1.tar.gz"), ("rmux-sdk", "tests/sdk/client.py"),
                            ("rmux-sdk", "src/scripts/share/sdk.js"), ("rmux-sdk", "Cargo.toml")):
            with self.subTest(name=name, extra=extra):
                package_dir = self.root / str(len(list(self.root.iterdir()))); package_dir.mkdir()
                archive = package_dir / f"{name}-1.0.0.crate"
                contents = {f"{name}-1.0.0/Cargo.toml": f'[package]\nname = "{name}"\nversion = "1.0.0"\n'.encode(),
                            f"{name}-1.0.0/src/lib.rs": b"// Rust SDK fixture\n",
                            f"{name}-1.0.0/README.md": (ROOT / "README.md").read_bytes(),
                            f"{name}-1.0.0/tests/fixtures/bootstrap-boundary/check_removed.py": b"# existing fixture\n"}
                if extra:
                    contents[f"{name}-1.0.0/{extra}"] = (
                        f'[package]\nname = "{name}"\nversion = "1.0.0"\n[package.metadata]\nsupported_sdks = ["librmux"]\n'.encode()
                        if extra == "Cargo.toml" else b"excluded payload")
                write_tar(archive, contents)
                bad = extra is not None or name == "librmux"
                if bad:
                    with self.assertRaisesRegex(ValueError, "excluded|payload"):
                        producer.build_manifest(SHA, "1.0.0", [name], {name: []}, package_dir)
                # Construct the downstream package-set without producer policy.
                manifest, payloads = producer.build_manifest(SHA, "0.10.0", [], {}, self.root)
                data = archive.read_bytes()
                manifest.update(version="1.0.0", publish_order=[name], package_count=1, packages=[{
                    "name": name, "version": "1.0.0", "file": archive.name, "size": len(data),
                    "sha256": hashlib.sha256(data).hexdigest(), "workspace_dependencies": []}])
                payloads = {"crate-package-set.json": json.dumps(manifest).encode(), f"crates/{archive.name}": data}
                outer = package_dir / "package-set.tar"; producer.write_tar(outer, payloads)
                root = package_dir / "unpacked"; parsed = reader.unpack(outer, root)
                if bad:
                    with self.assertRaisesRegex(ValueError, "excluded|payload"):
                        reader.validate(parsed, root, source_sha=SHA, version="1.0.0")
                else:
                    producer.build_manifest(SHA, "1.0.0", [name], {name: []}, package_dir)
                    # Reach the command dispatcher using bounded Cargo metadata,
                    # with already-built inert package bytes and no Cargo process.
                    producer.cargo_metadata = lambda: {"workspace_members": [name], "packages": [{
                        "id": name, "name": name, "version": "1.0.0", "publish": None, "dependencies": []}]}
                    target = package_dir / "target"; target.mkdir()
                    (target / "package").mkdir()
                    (target / "package" / archive.name).write_bytes(data)
                    _, canonical_payloads = producer.build_manifest(SHA, "1.0.0", [name], {name: []}, package_dir)
                    producer.write_tar(package_dir / "rmux-1.0.0-crate-package-set.tar", canonical_payloads)
                    producer.execute(argparse.Namespace(command="verify", source_sha=SHA, version="1.0.0",
                                                        target_dir=target, output_dir=package_dir))
                    reader.validate(parsed, root, source_sha=SHA, version="1.0.0")
                    parsed["supported_sdks"] = ["@rmux/sdk"]
                    with self.assertRaisesRegex(ValueError, "fields differ"):
                        reader.validate(parsed, root, source_sha=SHA, version="1.0.0")

    def test_cargo_normalized_build_metadata_both_paths(self):
        # Cargo 1.95.0 normalized this offline inert package in the independent
        # counterexample. Keep its manifest bytes, including the real Web build
        # configuration; no Cargo download/build is needed for this regression.
        normalized = (Path(__file__).resolve().parent /
                      "fixtures/sdk-policy/normalized-web-crypto.toml").read_text(encoding="utf-8")
        producer = module("canonical-crate-set")
        reader = module("crate_package_set")
        variants = {
            "normalized": (normalized, True),
            "no-metadata": (normalized.replace(
                '[package.metadata.wasm-pack.profile.release]\nwasm-opt = false\n', ''), True),
            "features": (normalized + '\n[features]\ndefault = ["wasm"]\nwasm = []\n'
                         'wasm-test = ["wasm"]\nsdk = []\npython = []\n', True),
            "support": (normalized + '\n[package.metadata]\nsupported_sdks = ["@rmux/sdk"]\n', False),
            "unknown": (normalized + '\n[package.metadata.other]\nvalue = true\n', False),
            "nested-support": (normalized.replace('wasm-opt = false',
                               'wasm-opt = false\nsupported_sdks = ["librmux"]'), False),
            "profile": (normalized + '\n[package.metadata.wasm-pack.profile.dev]\nwasm-opt = false\n', False),
            "wrong-value": (normalized.replace('wasm-opt = false', 'wasm-opt = true'), False),
            "integer-zero": (normalized.replace('wasm-opt = false', 'wasm-opt = 0'), False),
            "other-crate": (normalized.replace('"rmux-web-crypto"', '"rmux-sdk"'), False),
        }
        for version in ("1.0.0", "1.0.1-rc.1", "0.10.0"):
            for label, (text, approved) in variants.items():
                name = "rmux-sdk" if label == "other-crate" else "rmux-web-crypto"
                directory = self.root / f"{version}-{label}"; directory.mkdir()
                archive = directory / f"{name}-{version}.crate"
                write_tar(archive, {
                    f"{name}-{version}/Cargo.toml": text.replace('"1.0.0"', f'"{version}"').encode(),
                    f"{name}-{version}/src/lib.rs": b"// inert local fixture\n",
                })
                data = archive.read_bytes()
                manifest = {"schema_version": 1, "repository_id": 1239918790,
                            "source_git_sha": SHA, "version": version, "publish_order": [name],
                            "package_count": 1, "packages": [{"name": name, "version": version,
                            "file": archive.name, "size": len(data),
                            "sha256": hashlib.sha256(data).hexdigest(), "workspace_dependencies": []}]}
                outer = directory / "set.tar"
                producer.write_tar(outer, {"crate-package-set.json": json.dumps(manifest).encode(),
                                          f"crates/{archive.name}": data})
                extracted = directory / "unpacked"; parsed = reader.unpack(outer, extracted)
                # Independent subtests ensure a producer failure never prevents
                # testing the downstream reader on the very same archive bytes.
                for entry, check in (
                    ("producer", lambda: producer.build_manifest(SHA, version, [name], {name: []}, directory)),
                    ("reader", lambda: reader.validate(parsed, extracted, source_sha=SHA, version=version)),
                ):
                    with self.subTest(version=version, variant=label, entry=entry):
                        if approved or version == "0.10.0":
                            check()
                        else:
                            with self.assertRaisesRegex(ValueError, "identity/metadata differs"):
                                check()

    def linux_tree_fixture(self, layout):
        root = self.root / layout; root.mkdir()
        files = {
            "usr/share/bash-completion/completions/rmux", "usr/share/zsh/site-functions/_rmux",
            "usr/share/fish/vendor_completions.d/rmux.fish", "usr/share/powershell/Completions/_rmux.ps1",
            "usr/share/elvish/lib/rmux.elv",
            "usr/bin/rmux", "usr/bin/rmux-daemon", "usr/libexec/rmux/rmux",
            "usr/share/doc/rmux/README.md", "usr/share/doc/rmux/LICENSE-APACHE",
            "usr/share/doc/rmux/LICENSE-MIT", "usr/share/man/man1/rmux.1.gz",
            "usr/share/rmux/artifact-metadata.json"}
        for name in files:
            path = root / name; path.parent.mkdir(parents=True, exist_ok=True); path.write_bytes(b"fixture")
        copyright_path = root / "usr/share/doc/rmux/copyright"
        if layout == "deb":
            # Use the unchanged producer's verbatim file, independently of
            # the acceptance allowlist (the omitted file caused F2).
            producer = (ROOT / "scripts/package-debian.sh").read_text(encoding="utf-8")
            copyright_text = producer.split(
                'cat > "$stage_dir/usr/share/doc/rmux/copyright" <<EOF\n', 1)[1].split('\nEOF', 1)[0] + '\n'
            copyright_path.write_text(copyright_text)
        metadata_path = root / "usr/share/rmux/artifact-metadata.json"
        assets, _, _, _ = self.native("linux-x86_64")
        with tarfile.open(assets / "rmux-1.0.0-linux-x86_64.tar.gz") as archive:
            metadata = json.load(archive.extractfile("rmux-1.0.0-linux-x86_64/share/rmux/artifact-metadata.json"))
        del metadata["archive_reproducibility"]
        metadata["reuse_release_binary"] = False
        metadata_path.write_text(json.dumps(metadata))
        return root

    def test_linux_tree_cli_exclusion(self):
        for layout in ("deb", "rpm"):
            root = self.linux_tree_fixture(layout)
            metadata_path = root / "usr/share/rmux/artifact-metadata.json"
            metadata = json.loads(metadata_path.read_text())
            copyright_path = root / "usr/share/doc/rmux/copyright"
            if layout == "deb":
                copyright_text = copyright_path.read_text()
            args = ("--root", root, "--layout", layout, "--version", "1.0.0")
            self.assert_ok(run("sdk_distribution_policy.py", *args))
            if layout == "deb":
                copyright_path.unlink()
                self.assert_rejected(run("sdk_distribution_policy.py", *args),
                                     "missing=['usr/share/doc/rmux/copyright']")
                self.assert_ok(run("sdk_distribution_policy.py", "--root", root,
                                   "--layout", layout, "--version", "0.10.0"))
                copyright_path.write_text(copyright_text)
            else:
                copyright_path.write_bytes(b"Debian-only file\n")
                self.assert_rejected(run("sdk_distribution_policy.py", *args),
                                     "extra=['usr/share/doc/rmux/copyright']")
                copyright_path.unlink()
            metadata["supported_sdks"] = ["librmux"]
            metadata_path.write_text(json.dumps(metadata))
            self.assert_rejected(run("sdk_distribution_policy.py", *args), "metadata fields differ")
            del metadata["supported_sdks"]; metadata_path.write_text(json.dumps(metadata))
            added = root / "usr/lib/librmux/__init__.py"; added.parent.mkdir(parents=True); added.write_bytes(b"fixture")
            self.assert_rejected(run("sdk_distribution_policy.py", *args), "file set differs")
            self.assert_ok(run("sdk_distribution_policy.py", "--root", root, "--layout", layout, "--version", "0.10.0"))

    @unittest.skipIf(os.name == "nt", "symlink fixture requires Unix; RPM native proof runs on Linux")
    def test_rpm_duplicate_build_id_links_cli(self):
        root = self.linux_tree_fixture("rpm")
        # Native RPM 4.18.2 generated the unsuffixed name and .1 through .11
        # from identical ELF bytes in the measured native producer experiment.
        prefix = "usr/lib/.build-id/01/3ae8aeb5fcd7d3f418ef08a31adfde6a2773c0"
        args = ("--root", root, "--layout", "rpm", "--version", "1.0.0")
        for suffix in ("", ".1", ".2", ".10", ".11", ".0", ".01", ".a", ".1a", ".-1", ".+1", ".1.1", "."):
            with self.subTest(suffix=suffix):
                link = root / (prefix + suffix); link.parent.mkdir(parents=True, exist_ok=True)
                link.symlink_to("../../../../usr/bin/rmux")
                try:
                    result = run("sdk_distribution_policy.py", *args)
                    if suffix in ("", ".1", ".2", ".10", ".11"):
                        self.assert_ok(result)
                    else:
                        self.assert_rejected(result, "unexpected link")
                finally:
                    link.unlink()
        for target in ("../../../../usr/share/doc/rmux/README.md", "/usr/bin/rmux",
                       "../../../../../outside-rmux"):
            with self.subTest(target=target):
                link = root / (prefix + ".1"); link.symlink_to(target)
                try:
                    self.assert_rejected(run("sdk_distribution_policy.py", *args), "unexpected link")
                finally:
                    link.unlink()
        link = root / (prefix + ".1"); link.symlink_to("../../../../usr/libexec/rmux/rmux")
        self.assert_ok(run("sdk_distribution_policy.py", "--root", root,
                           "--layout", "rpm", "--version", "0.10.0"))
        # Debian must not acquire RPM's link exception.
        self.assert_rejected(run("sdk_distribution_policy.py", "--root", root,
                                 "--layout", "deb", "--version", "1.0.0"), "unexpected link")

    def test_wasm_exact_producer_and_downstream_file_set(self):
        producer = module("canonical-wasm-bundle")
        downstream = module("owned_repository_payload")
        package = self.root / "wasm"; package.mkdir()
        output = self.root / "output"; output.mkdir()
        for name in producer.PACKAGE_FILES:
            (package / name).write_bytes(b"browser fixture")
        (package / "README.md").write_bytes((ROOT / "README.md").read_bytes())
        metadata = {"name": "rmux-web-crypto", "version": "1.0.0", "type": "module"}
        (package / "package.json").write_text(json.dumps(metadata))
        args = ("--source-sha", SHA, "--version", "1.0.0", "--package-dir", package, "--output-dir", output)
        self.assert_ok(run("canonical-wasm-bundle.py", "create", *args))
        self.assert_ok(run("canonical-wasm-bundle.py", "verify", *args))
        downstream.wasm_files(output, source_sha=SHA, version="1.0.0")
        archive = output / "rmux-web-crypto-wasm-1.0.0.tar"
        with tarfile.open(archive) as source:
            contents = {member.name: source.extractfile(member).read() for member in source}
        contents["sdk/rmux.js"] = b"excluded SDK fixture"
        write_tar(archive, contents, "w")
        self.assert_rejected(run("canonical-wasm-bundle.py", "verify", *args), "member set or ordering changed")
        with self.assertRaisesRegex(ValueError, "member set differs"):
            downstream.wasm_files(output, source_sha=SHA, version="1.0.0")
        for addition in ({"name": "@rmux/sdk"}, {"supported_sdks": ["@rmux/sdk"]}):
            (package / "package.json").write_text(json.dumps({**metadata, **addition}))
            fresh = self.root / f"bad-wasm-{len(list(self.root.iterdir()))}"; fresh.mkdir()
            bad_args = list(args); bad_args[-1] = fresh
            self.assert_rejected(run("canonical-wasm-bundle.py", "create", *bad_args), "identity/fields differ")
            files = producer.package_files(package)
            producer.write_tar(fresh / "rmux-web-crypto-wasm-1.0.0.tar", files)
            (fresh / "rmux-web-crypto-wasm-1.0.0.provenance.json").write_text(json.dumps(producer.provenance(SHA, "1.0.0", files)))
            with self.assertRaisesRegex(ValueError, "identity/fields differ"):
                downstream.wasm_files(fresh, source_sha=SHA, version="1.0.0")

    def test_snap_payload_cli(self):
        root = self.root / "snap"; root.mkdir()
        (root / "bin").mkdir(); (root / "bin/rmux").write_bytes(b"native fixture")
        (root / "README.md").write_bytes((ROOT / "README.md").read_bytes())
        args = ("--root", root, "--layout", "snap", "--version", "1.0.0")
        self.assert_ok(run("sdk_distribution_policy.py", *args))
        for name in ("librmux-0.6.1.whl", "client.js", "package.json"):
            path = root / name; path.write_bytes(b"additional SDK fixture")
            self.assert_rejected(run("sdk_distribution_policy.py", *args), "excluded")
            path.unlink()

    def test_chocolatey_rejects_added_sdk(self):
        assets, _, _, create = self.native("windows-x86_64")
        with zipfile.ZipFile(assets / "rmux.fixture.nupkg", "a") as bundle:
            bundle.writestr("tools/librmux/__init__.py", "excluded SDK fixture")
        self.assert_ok(run("canonical-build-record.py", "checksums", "--assets-dir", assets))
        self.assert_rejected(run("canonical-build-record.py", "create", *create), "Chocolatey package file set differs")

    def test_release_candidate_rc_uses_package_version(self):
        _, _, _, create = self.native()
        create[create.index("--planned-release-ref") + 1] = "v1.0.0-rc.1"
        create[create.index("--release-kind") + 1] = "rc"
        self.assert_ok(run("canonical-build-record.py", "create", *create))

    def test_documented_historical_versions_and_entrypoint_bindings(self):
        for name in ("README.md", "docs/i18n/README.fr.md", "docs/i18n/README.ja.md", "docs/i18n/README.zh-CN.md"):
            text = (ROOT / name).read_text(encoding="utf-8")
            self.assertIn("pip install librmux==0.6.1", text)
            self.assertIn("npm install @rmux/sdk@0.6.5", text)
            self.assertNotIn("pip install librmux\n", text)
            self.assertNotIn("npm install @rmux/sdk\n", text)
            self.assertIn("Post-1.0 Python/TypeScript SDK qualification", text)
            self.assertIn("https://pypi.org/project/librmux/0.6.1/", text)
        self.assertIn('"/README.md"', (ROOT / "Cargo.toml").read_text(encoding="utf-8"))
        self.assertIn('"canonical-build-record.py"', (RELEASE / "verify-candidate-artifacts.py").read_text(encoding="utf-8"))
        for layout in ("debian", "rpm"):
            self.assertIn("release/sdk_distribution_policy.py", (ROOT / f"scripts/verify-{layout}-package.sh").read_text(encoding="utf-8"))
        self.assertIn("release/sdk_distribution_policy.py", (ROOT / "scripts/smoke-snap-package.sh").read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main(verbosity=2)
