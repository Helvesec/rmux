"""The shared package layout descriptor, its projections and its refusals.

Two independent things are checked here. First, that the projections still
describe the layout the accepted packaging scripts produced before the
descriptor existed: the expected sets below were read from those scripts at
956d566 and are written out by hand, so a descriptor edit cannot move the
contract by moving the test with it. Second, that every consumer's check
refuses an omission, an extra entry, a wrong mode and an unknown variant, on
trees built here file by file rather than by any builder under test.
"""

import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts" / "packaging"))

import layout  # noqa: E402
import notices  # noqa: E402

TOOL = ROOT / "scripts" / "packaging" / "layout.py"

class ReleaseBuildPaths(unittest.TestCase):
    @unittest.skipIf(os.name == "nt", "Unix package command")
    def test_native_release_remaps_source_and_toolchain_paths_even_with_spaces(self):
        with tempfile.TemporaryDirectory() as tmp, patch.dict(os.environ, CARGO_HOME=tmp + "/cargo home"):
            with patch.object(notices, "capture", side_effect=StopIteration) as capture:
                with self.assertRaises(StopIteration):
                    notices.build(Path(tmp), ["--", "cargo", "build", "--target", "x86_64-unknown-linux-gnu", "--release"])
            command = capture.call_args.args[0]
            config = command[command.index("--config") + 1]
            key, flags = config.split("=", 1)
            self.assertEqual(key, "target.x86_64-unknown-linux-gnu.rustflags")
            self.assertIn(f"--remap-path-prefix={Path(tmp).resolve()}/cargo home=/cargo", json.loads(flags))
            self.assertIn(f"--remap-path-prefix={ROOT}=/rmux-source", json.loads(flags))


# What scripts/package-unix.sh staged at 956d566, plus the layout manifest the
# packaged installer now reads instead of repeating the layout.
UNIX_ARCHIVE = {
    "bin/rmux": "0755",
    "bin/rmux-daemon": "0755",
    "libexec/rmux/rmux": "0755",
    "install.sh": "0755",
    "share/man/man1/rmux.1": "0644",
    "share/bash-completion/completions/rmux": "0644",
    "share/zsh/site-functions/_rmux": "0644",
    "share/fish/vendor_completions.d/rmux.fish": "0644",
    "share/powershell/Completions/_rmux.ps1": "0644",
    "share/elvish/lib/rmux.elv": "0644",
    "share/rmux/artifact-metadata.json": "0644",
    "share/rmux/package-layout.tsv": "0644",
    "SHA256SUMS.txt": "0644",
    "LICENSE": "0644",
    "LICENSE-APACHE": "0644",
    "LICENSE-MIT": "0644",
}

# What scripts/package-debian.sh staged at 956d566, under its payload root.
DEBIAN = {
    "usr/bin/rmux": "0755",
    "usr/bin/rmux-daemon": "0755",
    "usr/libexec/rmux/rmux": "0755",
    "usr/share/man/man1/rmux.1.gz": "0644",
    "usr/share/bash-completion/completions/rmux": "0644",
    "usr/share/zsh/site-functions/_rmux": "0644",
    "usr/share/fish/vendor_completions.d/rmux.fish": "0644",
    "usr/share/powershell/Completions/_rmux.ps1": "0644",
    "usr/share/elvish/lib/rmux.elv": "0644",
    "usr/share/rmux/artifact-metadata.json": "0644",
    "usr/share/doc/rmux/README.md": "0644",
    "usr/share/doc/rmux/LICENSE-APACHE": "0644",
    "usr/share/doc/rmux/LICENSE-MIT": "0644",
    "usr/share/doc/rmux/copyright": "0644",
}

# What the rmux.spec of scripts/package-rpm.sh installed and owned at 956d566.
RPM = dict(DEBIAN)
del RPM["usr/share/doc/rmux/copyright"]
del RPM["usr/share/man/man1/rmux.1.gz"]
RPM["usr/share/man/man1/rmux.1"] = "0644"

# Where the Unix archive installs what it carries, and with which mode. Read
# from the descriptor's unix-archive install destinations by hand, so a prefix
# is judged against this table instead of against the same install-plan that
# built it.
INSTALLED_UNIX = {
    "libexec/rmux/rmux": "0755",
    "bin/rmux-daemon": "0755",
    "bin/rmux": "0755",
    "share/man/man1/rmux.1": "0644",
    "share/bash-completion/completions/rmux": "0644",
    "share/zsh/site-functions/_rmux": "0644",
    "share/fish/vendor_completions.d/rmux.fish": "0644",
    "share/powershell/Completions/_rmux.ps1": "0644",
    "share/elvish/lib/rmux.elv": "0644",
    "share/rmux/artifact-metadata.json": "0644",
    "share/rmux/package-layout.tsv": "0644",
}

# The RPM classifications the spec of 956d566 carried, transcribed by hand:
# %doc for the README and %license for the two licences, which the base wrote
# as %{_docdir}/rmux/... Paths and modes do not carry these; RPM mechanisms
# outside the file list read them.
RPM_ATTRIBUTES = {
    "usr/share/doc/rmux/README.md": "%doc",
    "usr/share/doc/rmux/LICENSE-APACHE": "%license",
    "usr/share/doc/rmux/LICENSE-MIT": "%license",
}

# What scripts/package-windows.ps1 staged at 956d566, ConPTY runtime included.
WINDOWS_ARCHIVE = [
    "rmux.exe",
    "rmux-daemon.exe",
    "libexec/rmux/rmux.exe",
    "install.ps1",
    "README.md",
    "LICENSE-APACHE",
    "LICENSE-MIT",
    "rmux.1",
    "SHA256SUMS.txt",
    "share/rmux/artifact-metadata.json",
    "share/rmux/package-layout.tsv",
    "share/rmux/conpty-runtime.json",
    "share/licenses/Microsoft.Windows.Console.ConPTY/LICENSE",
    "conpty.dll",
    "OpenConsole.exe",
    "libexec/rmux/conpty.dll",
    "libexec/rmux/OpenConsole.exe",
    "share/bash-completion/completions/rmux",
    "share/zsh/site-functions/_rmux",
    "share/fish/vendor_completions.d/rmux.fish",
    "share/powershell/Completions/_rmux.ps1",
    "share/elvish/lib/rmux.elv",
]


def tool(*arguments):
    return subprocess.run([sys.executable, str(TOOL), *arguments],
                          capture_output=True, text=True)


def raw(*arguments):
    return subprocess.run([sys.executable, str(TOOL), *arguments], capture_output=True)


# The Unix, Debian and RPM variants declare POSIX modes. A host that does not
# keep them cannot build a fixture tree in those variants, so those cases are
# skipped there instead of being weakened for every host.
posix_modes = unittest.skipUnless(
    os.name == "posix", "this host does not keep POSIX file modes")


def fixture_root(label):
    """Where a case builds its tree, and what becomes of it afterwards.

    RMUX_TEST_FIXTURE_ROOT names a directory that keeps the fixtures: the case
    writes in a fresh directory prefixed by its label on every call. Nothing
    is removed at the end, so earlier fixtures remain available and unchanged.
    Without it the case uses a temporary
    directory and its finalizer removes it, as before.
    """
    retained = os.environ.get("RMUX_TEST_FIXTURE_ROOT")
    if retained:
        parent = Path(retained)
        parent.mkdir(parents=True, exist_ok=True)
        prefix = label.replace(os.sep, "-") + "-"
        return Path(tempfile.mkdtemp(prefix=prefix, dir=parent)), None
    temporary = tempfile.TemporaryDirectory()
    return Path(temporary.name), temporary.cleanup


class DescriptorProjections(unittest.TestCase):
    def setUp(self):
        self.descriptor = layout.load()

    def paths(self, variant):
        return {placement["path"]: placement.get("mode")
                for _entry, placement in layout.entries(self.descriptor, variant)}

    def test_unix_archive_projects_the_accepted_layout(self):
        self.assertEqual(self.paths("unix-archive"), UNIX_ARCHIVE)

    def test_debian_projects_the_accepted_layout(self):
        self.assertEqual(self.paths("debian"), DEBIAN)

    def test_rpm_projects_the_accepted_layout(self):
        self.assertEqual(self.paths("rpm"), RPM)

    def test_windows_archive_projects_the_accepted_layout(self):
        self.assertEqual(sorted(self.paths("windows-archive")), sorted(WINDOWS_ARCHIVE))

    def test_rpm_spec_installs_and_owns_every_declared_path(self):
        installed = tool("rpm-spec-install", "--binaries-dir", "/binaries")
        owned = tool("rpm-files")
        self.assertEqual(installed.returncode, 0, installed.stderr)
        self.assertEqual(owned.returncode, 0, owned.stderr)
        for path, mode in RPM.items():
            self.assertIn(f"%{{buildroot}}/{path}", installed.stdout)
            self.assertIn(f"install -Dm{mode} ", installed.stdout)
            owns = f"/{path}" in owned.stdout or f"/{path}*" in owned.stdout
            self.assertTrue(owns, f"the RPM spec does not own {path}")
        self.assertEqual(len(installed.stdout.strip().splitlines()), len(RPM))
        self.assertEqual(len(owned.stdout.strip().splitlines()), len(RPM))

    def test_the_rpm_file_list_keeps_its_native_classifications(self):
        owned = tool("rpm-files")
        self.assertEqual(owned.returncode, 0, owned.stderr)
        flagged = {}
        for line in owned.stdout.strip().splitlines():
            fields = line.split(" ", 1)
            if fields[0].startswith("%attr("):
                continue
            directive, rest = fields
            flagged[rest.split(" ", 1)[1].lstrip("/").rstrip("*")] = directive
        self.assertEqual(flagged, RPM_ATTRIBUTES)
        for path, directive in RPM_ATTRIBUTES.items():
            self.assertIn(f"{directive} %attr({RPM[path]},root,root) /{path}", owned.stdout)

    def test_an_entry_class_the_rpm_variant_cannot_express_is_refused(self):
        descriptor = layout.load()
        for entry in descriptor["entries"]:
            if entry["id"] == "readme":
                entry["content_class"] = "manual"
        root, retain = fixture_root(self.id())
        if retain is not None:
            self.addCleanup(retain)
        path = root / "descriptor.json"
        path.write_text(json.dumps(descriptor), encoding="utf-8")
        result = tool("--descriptor", str(path), "rpm-files")
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("the rpm variant declares no attribute for the manual class: readme",
                      result.stderr)

    def test_homebrew_install_block_names_packaged_paths(self):
        block = tool("homebrew-install")
        self.assertEqual(block.returncode, 0, block.stderr)
        # The installed path is the last quoted token: a keg location may
        # itself be qualified, as (libexec/"rmux") is.
        installed = [line.rsplit('"', 2)[1] for line in block.stdout.strip().splitlines()]
        for quoted in installed:
            self.assertTrue(
                quoted in UNIX_ARCHIVE or any(path.startswith(quoted + "/")
                                              for path in UNIX_ARCHIVE),
                f"the formula installs a path the archive does not carry: {quoted}")
        # Every line installs the placement its entry has in the archive.
        mapping = self.descriptor["downstream"]["homebrew"]
        expected = [layout.placement_of(self.descriptor, mapping["from_variant"],
                                        item["entry"])[1]["path"]
                    for item in mapping["install_map"]]
        self.assertEqual(installed, expected)

    def test_winget_and_chocolatey_read_the_windows_placements(self):
        nested = tool("nested-exe", "--package-name", "rmux-1.2.3-windows-x86_64")
        self.assertEqual(nested.returncode, 0, nested.stderr)
        self.assertEqual(nested.stdout.strip(), "rmux-1.2.3-windows-x86_64\\rmux.exe")
        daemon = tool("nested-exe", "--package-name", "", "--entry", "daemon", "--separator", "")
        self.assertEqual(daemon.stdout.strip(), "rmux-daemon.exe")

    def test_the_installers_name_the_formats_the_descriptor_declares(self):
        # The packaged installers ship inside the archive, without the
        # descriptor, so they carry these identities as constants. This is
        # what keeps the two from drifting apart.
        cases = (
            (ROOT / "scripts" / "install-unix-archive.sh", "unix-archive",
             'layout_current="{}"', 'layout_legacy="{}"'),
            (ROOT / "scripts" / "install-windows.ps1", "windows-archive",
             '$layoutCurrent = "{}"', '$layoutLegacy = "{}"'),
        )
        for path, name, current, legacy in cases:
            settings = self.descriptor["variants"][name]
            text = path.read_text(encoding="utf-8")
            self.assertIn(current.format(settings["package_layout"]), text)
            self.assertEqual(len(settings["legacy_package_layouts"]), 1, name)
            self.assertIn(legacy.format(settings["legacy_package_layouts"][0]), text)
            projected = tool("layout-id", "--variant", name)
            self.assertEqual(projected.stdout.strip(), settings["package_layout"])
            projected = tool("layout-id", "--variant", name, "--legacy")
            self.assertEqual(projected.stdout.strip().splitlines(),
                             settings["legacy_package_layouts"])

    def test_only_the_archive_formats_changed_their_identity(self):
        # deb and rpm carry exactly what they carried before the descriptor,
        # so their identity does not move; the two archive formats gained a
        # mandatory manifest, so theirs does.
        identities = {name: settings["package_layout"]
                      for name, settings in self.descriptor["variants"].items()}
        self.assertEqual(identities, {
            "unix-archive": "rmux-package-v3",
            "windows-archive": "rmux-windows-package-v3",
            "debian": "rmux-debian-package-v2",
            "rpm": "rmux-rpm-package-v2",
        })

    def test_the_projections_end_their_lines_with_a_single_byte(self):
        # A shell consumer reads these with `read`; a CR left at the end of a
        # line travels into the generated script as part of a path.
        for command in (("manifest", "--variant", "unix-archive"),
                        ("chocolatey-shims",),
                        ("nested-exe", "--package-name", "", "--entry", "cli",
                         "--separator", ""),
                        ("command-alias",),
                        ("homebrew-install",),
                        ("rpm-files",),
                        ("layout-id", "--variant", "unix-archive")):
            result = raw(*command)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn(b"\r", result.stdout, str(command))

    def test_the_completion_sources_a_builder_prepares_are_projected(self):
        # The builders read this list instead of keeping one of their own, so
        # a renamed source is generated, cached and staged under its new name.
        for name in ("unix-archive", "windows-archive"):
            names = tool("source-names", "--variant", name, "--kind", "completion")
            self.assertEqual(names.returncode, 0, names.stderr)
            projected = names.stdout.strip().splitlines()
            expected = [entry["source"]["name"]
                        for entry, _ in layout.entries(self.descriptor, name)
                        if entry["source"]["kind"] == "completion"]
            self.assertEqual(projected, expected)
            self.assertIn("_rmux", projected)
        unknown = tool("source-names", "--variant", "unix-archive", "--kind", "sparkle")
        self.assertEqual(unknown.returncode, 2, unknown.stdout)
        self.assertIn("declares no sparkle source", unknown.stderr)

    def test_every_completion_names_the_shell_its_generator_reports(self):
        # The generator prints the shell it completes beside each file it
        # wrote; the packaging preparation joins on that name. The vocabulary
        # is the generator's, read here from its own source rather than
        # restated, so the two cannot drift into a silent mismatch.
        generator = (ROOT / "xtask" / "src" / "completions.rs").read_text(encoding="utf-8")
        display = generator[generator.index("impl fmt::Display for Shell"):]
        reported = set(re.findall(r'Self::\w+ => "([a-z]+)"', display))
        self.assertTrue(reported, "the generator declares no reported shell name")
        for variant in ("unix-archive", "windows-archive"):
            pairs = tool("source-names", "--variant", variant, "--kind", "completion",
                         "--with-origin")
            self.assertEqual(pairs.returncode, 0, pairs.stderr)
            rows = [line.split("\t") for line in pairs.stdout.strip().splitlines()]
            self.assertTrue(rows)
            for shell, name in rows:
                self.assertIn(shell, reported,
                              f"{name} is generated from a shell the generator never reports")
            names = tool("source-names", "--variant", variant, "--kind", "completion")
            self.assertEqual([name for _shell, name in rows],
                             names.stdout.strip().splitlines())

    def test_a_source_with_no_origin_is_refused_by_name(self):
        descriptor = layout.load()
        for entry in descriptor["entries"]:
            if entry["id"] == "completion-zsh":
                del entry["source"]["shell"]
        root, retain = fixture_root(self.id())
        if retain is not None:
            self.addCleanup(retain)
        path = root / "descriptor.json"
        path.write_text(json.dumps(descriptor), encoding="utf-8")
        result = tool("--descriptor", str(path), "source-names",
                      "--variant", "windows-archive", "--kind", "completion",
                      "--with-origin")
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("the completion source of completion-zsh declares no origin", result.stderr)

    def test_an_unknown_variant_is_refused(self):
        for command in (["paths", "--variant", "snap"],
                        ["check-tree", "--variant", "flatpak", "--root", str(ROOT)],
                        ["install-plan", "--variant", "pacman"]):
            result = tool(*command)
            self.assertEqual(result.returncode, 2, result.stdout)
            self.assertIn("unknown package variant", result.stderr)


class TreeChecks(unittest.TestCase):
    """Trees built here file by file, never by a builder under test."""

    def setUp(self):
        self.root, retain = fixture_root(self.id())
        if retain is not None:
            self.addCleanup(retain)

    def build(self, variant, expected, skip=(), extra=None, modes=None):
        root = self.root / variant
        for path, mode in expected.items():
            if path in skip:
                continue
            target = root / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(b"payload\n")
            os.chmod(target, int((modes or {}).get(path, mode), 8))
        for path in (extra or []):
            target = root / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(b"extra\n")
            os.chmod(target, 0o644)
        return root

    def check(self, variant, root):
        return tool("check-tree", "--variant", variant, "--root", str(root))

    @posix_modes
    def test_a_complete_tree_is_accepted(self):
        for variant, expected in (("unix-archive", UNIX_ARCHIVE),
                                  ("debian", DEBIAN),
                                  ("rpm", RPM)):
            result = self.check(variant, self.build(variant, expected))
            self.assertEqual(result.returncode, 0, result.stderr)

    @posix_modes
    def test_the_man_page_may_be_compressed_only_where_rpm_allows_it(self):
        expected = dict(RPM)
        del expected["usr/share/man/man1/rmux.1"]
        expected["usr/share/man/man1/rmux.1.gz"] = "0644"
        result = self.check("rpm", self.build("rpm-gz", expected))
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_an_omitted_file_is_refused_by_name(self):
        # The completions every validator used to ignore before the descriptor.
        for variant, expected, missing in (
                ("unix-archive", UNIX_ARCHIVE, "share/zsh/site-functions/_rmux"),
                ("debian", DEBIAN, "usr/share/elvish/lib/rmux.elv"),
                ("rpm", RPM, "usr/share/fish/vendor_completions.d/rmux.fish")):
            root = self.build(f"{variant}-omission", expected, skip={missing})
            result = self.check(variant, root)
            self.assertEqual(result.returncode, 2, result.stdout)
            self.assertIn(f"missing required package file: {missing}", result.stderr)

    def test_an_extra_file_is_refused_by_name(self):
        root = self.build("unix-extra", UNIX_ARCHIVE, extra=["share/rmux/leftover.tmp"])
        result = self.check("unix-archive", root)
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("unexpected package file: share/rmux/leftover.tmp", result.stderr)

    @posix_modes
    def test_a_wrong_mode_is_refused_by_name(self):
        root = self.build("unix-mode", UNIX_ARCHIVE, modes={"bin/rmux": "0644"})
        result = self.check("unix-archive", root)
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("package file mode is 0644, expected 0755: bin/rmux", result.stderr)

    def test_an_extra_directory_link_is_refused_on_every_variant(self):
        # os.walk never descends into a link, so a link to a directory has to
        # be refused as the entry it is instead of disappearing silently.
        for variant, expected in (("unix-archive", UNIX_ARCHIVE),
                                  ("debian", DEBIAN),
                                  ("rpm", RPM),
                                  ("windows-archive", {path: "0644" for path in WINDOWS_ARCHIVE})):
            outside = self.root / f"outside-{variant}"
            outside.mkdir(parents=True, exist_ok=True)
            (outside / "payload").write_bytes(b"outside\n")
            root = self.build(f"{variant}-dirlink", expected)
            os.symlink(outside, root / "extra-link")
            result = self.check(variant, root)
            self.assertEqual(result.returncode, 2, result.stdout)
            self.assertIn("unexpected package entry (symbolic link): extra-link",
                          result.stderr)

    def test_a_declared_file_reached_through_a_linked_directory_is_refused(self):
        for variant, expected, declared, parent in (
                ("unix-archive", UNIX_ARCHIVE,
                 "share/zsh/site-functions/_rmux", "share/zsh/site-functions"),
                ("rpm", RPM,
                 "usr/share/zsh/site-functions/_rmux", "usr/share/zsh/site-functions")):
            outside = self.root / f"linked-{variant}"
            outside.mkdir(parents=True, exist_ok=True)
            (outside / "_rmux").write_bytes(b"payload\n")
            os.chmod(outside / "_rmux", 0o644)
            root = self.build(f"{variant}-through-link", expected, skip={declared})
            (root / parent).parent.mkdir(parents=True, exist_ok=True)
            os.symlink(outside, root / parent)
            result = self.check(variant, root)
            self.assertEqual(result.returncode, 2, result.stdout)
            self.assertIn(f"package file is reached through a symbolic link directory: "
                          f"{declared} (through {parent})", result.stderr)

    def test_a_leaf_link_is_refused_where_a_regular_file_is_declared(self):
        outside = self.root / "leaf-target"
        outside.mkdir(parents=True, exist_ok=True)
        (outside / "rmux").write_bytes(b"payload\n")
        root = self.build("unix-leaf-link", UNIX_ARCHIVE, skip={"bin/rmux"})
        os.symlink(outside / "rmux", root / "bin/rmux")
        result = self.check("unix-archive", root)
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("package file is a symbolic link: bin/rmux", result.stderr)

    @posix_modes
    def test_an_undeclared_special_mode_is_refused_on_every_mode_variant(self):
        # 04755 is not 0755: the validator used to mask the special bits away
        # and accept a setuid dispatcher the Unix installer then refuses.
        for variant, expected, path in (("unix-archive", UNIX_ARCHIVE, "bin/rmux"),
                                        ("debian", DEBIAN, "usr/bin/rmux"),
                                        ("rpm", RPM, "usr/bin/rmux")):
            root = self.build(f"{variant}-setuid", expected)
            os.chmod(root / path, 0o4755)
            result = self.check(variant, root)
            self.assertEqual(result.returncode, 2, result.stdout)
            self.assertIn(f"package file mode is 4755, expected 0755: {path}", result.stderr)
            root = self.build(f"{variant}-sticky", expected)
            os.chmod(root / path, 0o1755)
            result = self.check(variant, root)
            self.assertEqual(result.returncode, 2, result.stdout)
            self.assertIn(f"package file mode is 1755, expected 0755: {path}", result.stderr)

    def test_windows_refuses_links_without_inventing_posix_modes(self):
        expected = {path: "0644" for path in WINDOWS_ARCHIVE}
        root = self.build("windows-setuid", expected)
        os.chmod(root / "rmux.exe", 0o4755)
        self.assertEqual(self.check("windows-archive", root).returncode, 0,
                         "the Windows variant declares no POSIX mode")
        outside = self.root / "windows-outside"
        outside.mkdir(parents=True, exist_ok=True)
        (outside / "rmux.exe").write_bytes(b"payload\n")
        root = self.build("windows-leaf-link", expected, skip={"rmux.exe"})
        os.symlink(outside / "rmux.exe", root / "rmux.exe")
        result = self.check("windows-archive", root)
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("package file is a symbolic link: rmux.exe", result.stderr)

    def test_windows_modes_are_not_imposed_but_its_files_are(self):
        expected = {path: "0644" for path in WINDOWS_ARCHIVE}
        root = self.build("windows", expected)
        self.assertEqual(self.check("windows-archive", root).returncode, 0)
        root = self.build("windows-omission", expected, skip={"libexec/rmux/conpty.dll"})
        result = self.check("windows-archive", root)
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("missing required package file: libexec/rmux/conpty.dll", result.stderr)


@posix_modes
class InstallProjection(unittest.TestCase):
    def setUp(self):
        self.root, retain = fixture_root(self.id())
        if retain is not None:
            self.addCleanup(retain)
        self.package = self.root / "package"
        for path, mode in UNIX_ARCHIVE.items():
            target = self.package / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(b"payload\n")
            os.chmod(target, int(mode, 8))

    def test_the_plan_installs_the_dispatcher_last(self):
        plan = tool("install-plan", "--variant", "unix-archive")
        self.assertEqual(plan.returncode, 0, plan.stderr)
        executables = [line.split("\t")[1] for line in plan.stdout.strip().splitlines()
                       if line.split("\t")[2] == "0755"]
        self.assertEqual(executables, ["libexec/rmux/rmux", "bin/rmux-daemon", "bin/rmux"])

    def test_the_manifest_declares_every_package_file_with_its_destination(self):
        manifest = tool("manifest", "--variant", "unix-archive")
        self.assertEqual(manifest.returncode, 0, manifest.stderr)
        declared = {}
        for line in manifest.stdout.strip().splitlines():
            path, mode, destination, required, entry, role = line.split("\t")
            declared[path] = (mode, destination, required, entry, role)
        self.assertEqual(set(declared), set(UNIX_ARCHIVE))
        for path, mode in UNIX_ARCHIVE.items():
            self.assertEqual(declared[path][0], mode)
        # What the package carries but never installs stays declared, with no
        # destination, so an installer can still refuse an undeclared file.
        self.assertEqual(declared["install.sh"][1], "-")
        self.assertEqual(declared["LICENSE-MIT"][1], "-")
        self.assertEqual(declared["bin/rmux"][1], "bin/rmux")
        # The identity and the role come with each line, so the installer
        # classifies an entry by what the descriptor declares it to be rather
        # than by its mode.
        self.assertEqual(declared["bin/rmux"][3:], ("cli", "executable"))
        self.assertEqual(declared["libexec/rmux/rmux"][3:], ("helper", "executable"))
        self.assertEqual(declared["share/man/man1/rmux.1"][3:], ("man-page", "file"))
        self.assertEqual({role for _, _, _, _, role in declared.values()},
                         {"executable", "file"})

    def test_the_plan_carries_every_installed_entry_and_nothing_else(self):
        plan = tool("install-plan", "--variant", "unix-archive")
        destinations = {line.split("\t")[1] for line in plan.stdout.strip().splitlines()}
        not_installed = {"install.sh", "SHA256SUMS.txt", "LICENSE", "LICENSE-APACHE", "LICENSE-MIT"}
        self.assertEqual(destinations, set(UNIX_ARCHIVE) - not_installed)

    def test_an_installed_prefix_is_checked_against_its_package(self):
        prefix = self.root / "prefix"
        plan = tool("install-plan", "--variant", "unix-archive",
                    "--package-root", str(self.package), "--prefix", str(prefix))
        for line in plan.stdout.strip().splitlines():
            source, destination, mode, _required = line.split("\t")
            Path(destination).parent.mkdir(parents=True, exist_ok=True)
            Path(destination).write_bytes(Path(source).read_bytes())
            os.chmod(destination, int(mode, 8))
        good = tool("check-installed", "--variant", "unix-archive",
                    "--package-root", str(self.package), "--prefix", str(prefix))
        self.assertEqual(good.returncode, 0, good.stderr)

        (prefix / "share/zsh/site-functions/_rmux").unlink()
        missing = tool("check-installed", "--variant", "unix-archive",
                       "--package-root", str(self.package), "--prefix", str(prefix))
        self.assertEqual(missing.returncode, 2, missing.stdout)
        self.assertIn("missing installed file: share/zsh/site-functions/_rmux", missing.stderr)

    def test_a_package_missing_a_required_file_has_no_install_plan(self):
        (self.package / "share/rmux/artifact-metadata.json").unlink()
        result = tool("install-plan", "--variant", "unix-archive",
                      "--package-root", str(self.package), "--prefix", str(self.root / "p"))
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("missing required package file: share/rmux/artifact-metadata.json",
                      result.stderr)


@posix_modes
class InstalledPrefix(unittest.TestCase):
    """Prefixes built from INSTALLED_UNIX, never from the projection checked."""

    def setUp(self):
        self.root, retain = fixture_root(self.id())
        if retain is not None:
            self.addCleanup(retain)
        self.package = self.root / "package"
        for path, mode in UNIX_ARCHIVE.items():
            target = self.package / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(f"payload of {path}\n".encode())
            os.chmod(target, int(mode, 8))

    def install(self, label, modes=None, contents=None, skip=()):
        prefix = self.root / label
        for destination, mode in INSTALLED_UNIX.items():
            if destination in skip:
                continue
            target = prefix / destination
            target.parent.mkdir(parents=True, exist_ok=True)
            body = (contents or {}).get(destination)
            if body is None:
                body = (self.package / destination).read_bytes()
            target.write_bytes(body)
            os.chmod(target, int((modes or {}).get(destination, mode), 8))
        # A prefix is shared with the rest of the system; this file belongs to
        # nobody in the package and must survive the check.
        foreign = prefix / "share/rmux/local-only"
        foreign.parent.mkdir(parents=True, exist_ok=True)
        foreign.write_bytes(b"keep-local-asset\n")
        return prefix

    def check(self, prefix):
        return tool("check-installed", "--variant", "unix-archive",
                    "--package-root", str(self.package), "--prefix", str(prefix))

    def test_a_faithful_prefix_is_accepted_and_keeps_its_foreign_files(self):
        prefix = self.install("good")
        result = self.check(prefix)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((prefix / "share/rmux/local-only").read_bytes(), b"keep-local-asset\n")

    def test_an_asset_installed_with_an_executable_mode_is_refused(self):
        result = self.check(self.install("asset-mode",
                                         modes={"share/man/man1/rmux.1": "0755"}))
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("installed file mode is 0755, expected 0644: share/man/man1/rmux.1",
                      result.stderr)

    def test_an_executable_installed_with_another_mode_is_refused(self):
        result = self.check(self.install("exec-mode", modes={"bin/rmux": "0750"}))
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("installed file mode is 0750, expected 0755: bin/rmux", result.stderr)

    def test_installed_bytes_that_are_not_the_package_bytes_are_refused(self):
        result = self.check(self.install(
            "bytes", contents={"share/man/man1/rmux.1": b"other bytes\n"}))
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("installed file differs from the package file it comes from: "
                      "share/man/man1/rmux.1", result.stderr)

    def test_an_installed_file_replaced_by_a_link_is_refused(self):
        prefix = self.install("leaf-link", skip={"share/man/man1/rmux.1"})
        outside = self.root / "elsewhere"
        outside.mkdir(parents=True, exist_ok=True)
        target = outside / "rmux.1"
        target.write_bytes((self.package / "share/man/man1/rmux.1").read_bytes())
        (prefix / "share/man/man1").mkdir(parents=True, exist_ok=True)
        os.symlink(target, prefix / "share/man/man1/rmux.1")
        result = self.check(prefix)
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("installed file is a symbolic link: share/man/man1/rmux.1",
                      result.stderr)

    def test_a_package_file_reached_through_a_link_is_refused(self):
        # The package side of the comparison is held to the same rule.
        prefix = self.install("package-link")
        (self.package / "share/man/man1/rmux.1").unlink()
        outside = self.root / "package-elsewhere"
        outside.mkdir(parents=True, exist_ok=True)
        (outside / "rmux.1").write_bytes(b"payload of share/man/man1/rmux.1\n")
        os.symlink(outside / "rmux.1", self.package / "share/man/man1/rmux.1")
        result = self.check(prefix)
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("package file is a symbolic link: share/man/man1/rmux.1", result.stderr)


class DestinationCollisions(unittest.TestCase):
    """A descriptor whose entries would install onto each other is refused."""

    def setUp(self):
        self.root, retain = fixture_root(self.id())
        if retain is not None:
            self.addCleanup(retain)

    def descriptor_with(self, label, mutate):
        descriptor = layout.load()
        mutate(descriptor)
        path = self.root / f"{label}.json"
        path.write_text(json.dumps(descriptor), encoding="utf-8")
        return path

    def entry(self, descriptor, identifier):
        for entry in descriptor["entries"]:
            if entry["id"] == identifier:
                return entry
        raise AssertionError(f"no entry {identifier}")

    def refusals(self, path, variant="unix-archive"):
        return {command: tool("--descriptor", str(path), *command)
                for command in (("install-plan", "--variant", variant),
                                ("manifest", "--variant", variant),
                                ("stage-plan", "--variant", variant),
                                ("uninstall-plan", "--variant", variant))}

    def test_an_asset_aimed_at_the_dispatcher_is_refused_by_every_projection(self):
        def mutate(descriptor):
            self.entry(descriptor, "man-page")["variants"]["unix-archive"]["install"] = "bin/rmux"
        path = self.descriptor_with("collision", mutate)
        for command, result in self.refusals(path).items():
            self.assertEqual(result.returncode, 2, f"{command}: {result.stdout}")
            self.assertIn("two entries install to the same destination: "
                          "man-page (file, bin/rmux) and cli (executable, bin/rmux)",
                          result.stderr, str(command))

    def test_a_destination_differing_only_by_case_is_refused(self):
        def mutate(descriptor):
            self.entry(descriptor, "man-page")["variants"]["unix-archive"]["install"] = "bin/RMUX"
        path = self.descriptor_with("case", mutate)
        result = self.refusals(path)[("install-plan", "--variant", "unix-archive")]
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("two entries install to the same destination", result.stderr)

    def test_two_entries_claiming_one_package_path_are_refused(self):
        def mutate(descriptor):
            self.entry(descriptor, "readme")["variants"]["rpm"]["path"] = "usr/bin/rmux"
        path = self.descriptor_with("path-collision", mutate)
        result = tool("--descriptor", str(path), "check-tree", "--variant", "rpm",
                      "--root", str(self.root))
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("two entries claim the same package path", result.stderr)

    def test_the_windows_variant_is_held_to_the_same_rule(self):
        def mutate(descriptor):
            self.entry(descriptor, "readme")["variants"]["windows-archive"]["install"] = "bin/rmux.exe"
        path = self.descriptor_with("windows-collision", mutate)
        result = self.refusals(path, "windows-archive")[
            ("install-plan", "--variant", "windows-archive")]
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("two entries install to the same destination: "
                      "readme (file, bin/rmux.exe) and cli (executable, bin/rmux.exe)",
                      result.stderr)

    def test_an_equivalent_spelling_of_a_destination_is_refused(self):
        # bin/rmux, bin/./rmux and bin//rmux are one file under three names,
        # and only the first compares equal to the dispatcher's destination.
        for label, spelling, reason in (
                ("dot", "bin/./rmux", "relative segment"),
                ("slashes", "bin//rmux", "empty segment"),
                ("trailing", "bin/rmux/", "trailing separator"),
                ("parent", "bin/../bin/rmux", "relative segment"),
                ("absolute", "/bin/rmux", "absolute"),
                ("backslash", "bin\\rmux", "backslash")):
            def mutate(descriptor, spelling=spelling):
                self.entry(descriptor, "man-page")["variants"]["unix-archive"]["install"] = spelling
            path = self.descriptor_with(f"spelling-{label}", mutate)
            for command, result in self.refusals(path).items():
                self.assertEqual(result.returncode, 2, f"{command}: {result.stdout}")
                self.assertIn(f"the destination of man-page is not canonical ({reason}): {spelling}",
                              result.stderr, str(command))

    def test_a_package_path_that_is_not_canonical_is_refused_on_every_variant(self):
        for name, identifier in (("unix-archive", "man-page"), ("debian", "readme"),
                                 ("rpm", "readme"), ("windows-archive", "readme")):
            def mutate(descriptor, name=name, identifier=identifier):
                self.entry(descriptor, identifier)["variants"][name]["path"] = "usr/./doc"
            path = self.descriptor_with(f"path-{name}", mutate)
            result = tool("--descriptor", str(path), "check-tree", "--variant", name,
                          "--root", str(self.root))
            self.assertEqual(result.returncode, 2, result.stdout)
            self.assertIn(f"the package path of {identifier} is not canonical "
                          f"(relative segment): usr/./doc", result.stderr)

    def test_a_repeated_or_unusable_entry_id_is_refused(self):
        def duplicate(descriptor):
            self.entry(descriptor, "daemon")["id"] = "helper"
        path = self.descriptor_with("duplicate-id", duplicate)
        result = tool("--descriptor", str(path), "manifest", "--variant", "unix-archive")
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("two entries share the id helper", result.stderr)

        for identifier in ("../escape", "Helper", ""):
            def unusable(descriptor, identifier=identifier):
                self.entry(descriptor, "daemon")["id"] = identifier
            path = self.descriptor_with("unusable-id", unusable)
            result = tool("--descriptor", str(path), "manifest", "--variant", "unix-archive")
            self.assertEqual(result.returncode, 2, result.stdout)
            self.assertIn(f"entry id is not usable: {identifier!r}", result.stderr)

    def test_two_origins_under_one_source_name_are_refused(self):
        # completion-zsh takes the file name of completion-bash while keeping
        # its own shell, its own id and its own destination. Nothing collides
        # in the layout; what collides is what the builders prepare, and the
        # zsh destination would receive the bash bytes.
        def mutate(descriptor):
            self.entry(descriptor, "completion-zsh")["source"]["name"] = "rmux.bash"
        path = self.descriptor_with("origin-conflict", mutate)
        for name in ("unix-archive", "windows-archive", "debian", "rpm"):
            for command in (("source-names", "--variant", name, "--kind", "completion",
                             "--with-origin"),
                            ("source-names", "--variant", name, "--kind", "completion"),
                            ("stage-plan", "--variant", name),
                            ("manifest", "--variant", name),
                            ("install-plan", "--variant", name)):
                result = tool("--descriptor", str(path), *command)
                self.assertEqual(result.returncode, 2, f"{name} {command}: {result.stdout}")
                space = "sources" if name == "rpm" else "completions"
                self.assertIn(f"two sources prepared in the {space} of {name} are one file: "
                              "completion-zsh (completion zsh, rmux.bash) and completion-bash "
                              "(completion bash, rmux.bash)", result.stderr, f"{name} {command}")

    def test_one_file_declared_twice_from_one_place_is_still_listed_once(self):
        # The ConPTY runtime is staged at two destinations from one source.
        # That is one file from one place, and it stays legitimate.
        names = tool("source-names", "--variant", "windows-archive", "--kind", "conpty")
        self.assertEqual(names.returncode, 0, names.stderr)
        listed = names.stdout.strip().splitlines()
        self.assertEqual(sorted(listed), sorted(set(listed)))
        self.assertIn("conpty.dll", listed)
        descriptor = layout.load()
        placements = [placement["path"]
                      for entry, placement in layout.entries(descriptor, "windows-archive")
                      if entry["source"]["kind"] == "conpty"
                      and entry["source"].get("name") == "conpty.dll"]
        self.assertEqual(len(placements), 2, "the same file is staged at two destinations")

    def test_a_name_that_differs_only_by_case_is_the_same_file(self):
        # RMUX.BASH and rmux.bash are one file wherever the filesystem keeps
        # no case, and the same package is prepared on both kinds of
        # filesystem. The two origins would otherwise overwrite each other in
        # the preparation directory they share.
        def mutate(descriptor):
            self.entry(descriptor, "completion-zsh")["source"]["name"] = "RMUX.BASH"
        path = self.descriptor_with("case-alias", mutate)
        for name in ("unix-archive", "windows-archive", "debian", "rpm"):
            space = "sources" if name == "rpm" else "completions"
            for command in (("source-names", "--variant", name, "--kind", "completion",
                             "--with-origin"),
                            ("stage-plan", "--variant", name),
                            ("manifest", "--variant", name)):
                result = tool("--descriptor", str(path), *command)
                self.assertEqual(result.returncode, 2, f"{name} {command}: {result.stdout}")
                self.assertIn(f"two sources prepared in the {space} of {name} are one file",
                              result.stderr, f"{name} {command}")

    def test_one_name_in_two_preparation_directories_is_two_files(self):
        # A completion called rmux and the binary called rmux are prepared in
        # different directories by the archive builders, so neither hides the
        # other. The RPM builder flattens everything into its SOURCES, where
        # they really would be one file, and there it is refused.
        def mutate(descriptor):
            self.entry(descriptor, "completion-zsh")["source"]["name"] = "rmux"
        path = self.descriptor_with("name-of-a-binary", mutate)
        for name in ("unix-archive", "windows-archive", "debian"):
            result = tool("--descriptor", str(path), "source-names", "--variant", name,
                          "--kind", "completion", "--with-origin")
            self.assertEqual(result.returncode, 0, f"{name}: {result.stderr}")
            rows = [line.split("\t") for line in result.stdout.strip().splitlines()]
            self.assertIn(["zsh", "rmux"], rows, name)
            self.assertEqual(len(rows), 5, name)
        result = tool("--descriptor", str(path), "source-names", "--variant", "rpm",
                      "--kind", "completion", "--with-origin")
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("two sources prepared in the sources of rpm are one file: "
                      "completion-zsh (completion zsh, rmux) and cli (binary rmux, rmux)",
                      result.stderr)

    def test_two_repository_sources_sharing_a_last_segment_are_two_files(self):
        # The two licences move into directories of their own, keeping their
        # ids, their origins, their destinations and their modes. The builders
        # that read the repository where it lives are handed two distinct
        # paths, so this is a legitimate organisation; the RPM builder copies
        # both into SOURCES under the same last segment, where it is not.
        def mutate(descriptor):
            self.entry(descriptor, "license-apache")["source"]["path"] = "licenses/apache/LICENSE"
            self.entry(descriptor, "license-mit")["source"]["path"] = "licenses/mit/LICENSE"
        path = self.descriptor_with("repo-subdirectories", mutate)
        for name in ("unix-archive", "windows-archive", "debian"):
            for command in (("stage-plan", "--variant", name, "--repo", str(ROOT)),
                            ("manifest", "--variant", name),
                            ("install-plan", "--variant", name),
                            ("source-names", "--variant", name, "--kind", "repo")):
                result = tool("--descriptor", str(path), *command)
                self.assertEqual(result.returncode, 0, f"{name} {command}: {result.stderr}")
        # Both paths reach the consumer, neither hiding the other.
        plan = tool("--descriptor", str(path), "stage-plan", "--variant", "unix-archive",
                    "--repo", str(ROOT))
        sources = [line.split("\t")[0] for line in plan.stdout.strip().splitlines()]
        for expected in ("licenses/apache/LICENSE", "licenses/mit/LICENSE", "LICENSE"):
            self.assertTrue(any(source.endswith(expected) for source in sources),
                            f"{expected} is not handed to the builder")
        for command in (("stage-plan", "--variant", "rpm", "--repo", str(ROOT)),
                        ("stage-plan", "--variant", "rpm", "--staged-names",
                         "--repo", str(ROOT)),
                        ("manifest", "--variant", "rpm")):
            result = tool("--descriptor", str(path), *command)
            self.assertEqual(result.returncode, 2, f"rpm {command}: {result.stdout}")
            self.assertIn("two sources prepared in the sources of rpm are one file",
                          result.stderr, str(command))

    def test_one_repository_path_declared_by_two_entries_is_one_file(self):
        # The same file, declared twice, stays legitimate everywhere.
        def mutate(descriptor):
            self.entry(descriptor, "license-mit")["source"]["path"] = "LICENSE-APACHE"
        path = self.descriptor_with("same-repo-path", mutate)
        for name in ("unix-archive", "windows-archive", "debian", "rpm"):
            result = tool("--descriptor", str(path), "manifest", "--variant", name)
            self.assertEqual(result.returncode, 0, f"{name}: {result.stderr}")
        names = tool("--descriptor", str(path), "source-names", "--variant", "rpm",
                     "--kind", "repo")
        self.assertEqual(names.returncode, 0, names.stderr)
        listed = names.stdout.strip().splitlines()
        self.assertEqual(sorted(listed), sorted(set(listed)))

    def test_a_flattening_declaration_that_denies_the_builder_is_refused(self):
        # The RPM builder copies binaries, metadata, repository sources and
        # completions into one SOURCES directory. A declaration that leaves
        # that out, empties it, or names a directory that does not exist would
        # turn the guard off while the builder keeps copying both files over
        # each other.
        cases = {
            "absent": (None, "does not declare which of its preparation spaces are flattened"),
            "empty": ([], "prepares binary, completion, generated, repo in the same sources "
                          "and does not declare it flattened"),
            "misnamed": (["source"], "declares the space source flattened, but prepares "
                                     "nothing there"),
        }
        for label, (value, message) in cases.items():
            def mutate(descriptor, value=value):
                if value is None:
                    descriptor["variants"]["rpm"].pop("flattened_source_spaces", None)
                else:
                    descriptor["variants"]["rpm"]["flattened_source_spaces"] = value
            path = self.descriptor_with(f"flattening-{label}", mutate)
            for command in (("manifest", "--variant", "rpm"),
                            ("install-plan", "--variant", "rpm"),
                            ("stage-plan", "--variant", "rpm"),
                            ("source-names", "--variant", "rpm", "--kind", "repo"),
                            ("check-tree", "--variant", "rpm", "--root", str(self.root)),
                            ("rpm-spec-install", "--binaries-dir", "/binaries")):
                result = tool("--descriptor", str(path), *command)
                self.assertEqual(result.returncode, 2, f"{label} {command}: {result.stdout}")
                self.assertIn(message, result.stderr, f"{label} {command}")

    def test_detaching_a_kind_on_paper_does_not_move_it_out_of_the_rpm_sources(self):
        # The RPM builder copies binaries, metadata, repository sources and
        # completions into one SOURCES directory. Declaring one kind elsewhere
        # moves nothing: stage-plan still projects both to the same staged
        # name and the spec still installs both from there.
        cases = {
            "licences": ({"license-apache": ("path", "licenses/apache/LICENSE"),
                          "license-mit": ("path", "licenses/mit/LICENSE")},
                         ("repo", "repo")),
            "completion": ({"completion-zsh": ("name", "rmux")},
                           ("completion", "completions")),
        }
        for label, (entries, detached) in cases.items():
            def mutate(descriptor, entries=entries, detached=detached):
                for identifier, (field, value) in entries.items():
                    self.entry(descriptor, identifier)["source"][field] = value
                descriptor["variants"]["rpm"]["source_spaces"][detached[0]] = detached[1]
            path = self.descriptor_with(f"detached-{label}", mutate)
            for command in (("manifest", "--variant", "rpm"),
                            ("install-plan", "--variant", "rpm"),
                            ("stage-plan", "--variant", "rpm", "--repo", str(ROOT)),
                            ("stage-plan", "--variant", "rpm", "--staged-names",
                             "--repo", str(ROOT)),
                            ("source-names", "--variant", "rpm", "--kind", "completion"),
                            ("check-tree", "--variant", "rpm", "--root", str(self.root)),
                            ("rpm-spec-install", "--binaries-dir", "/binaries")):
                result = tool("--descriptor", str(path), *command)
                self.assertEqual(result.returncode, 2, f"{label} {command}: {result.stdout}")
                self.assertIn("the rpm builder prepares every source in one directory",
                              result.stderr, f"{label} {command}")
            # The three variants whose builders really keep the kinds apart
            # still admit the same organisation.
            for name in ("unix-archive", "windows-archive", "debian"):
                for command in (("manifest", "--variant", name),
                                ("install-plan", "--variant", name),
                                ("stage-plan", "--variant", name, "--repo", str(ROOT))):
                    result = tool("--descriptor", str(path), *command)
                    self.assertEqual(result.returncode, 0, f"{label} {name}: {result.stderr}")

    def test_a_variant_that_keeps_its_kinds_apart_may_not_declare_flattening(self):
        def mutate(descriptor):
            descriptor["variants"]["debian"]["flattened_source_spaces"] = ["completions"]
        path = self.descriptor_with("debian-flattening", mutate)
        result = tool("--descriptor", str(path), "manifest", "--variant", "debian")
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("the debian builder prepares each kind of source in its own directory, "
                      "but this variant declares completions flattened", result.stderr)

    def test_a_preparation_directory_that_is_not_a_name_is_refused(self):
        # A value that is not a string used to reach a set and raise instead of
        # being answered.
        def mutate(descriptor):
            descriptor["variants"]["rpm"]["source_spaces"]["repo"] = ["sources"]
        path = self.descriptor_with("space-not-a-name", mutate)
        result = tool("--descriptor", str(path), "manifest", "--variant", "rpm")
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("the rpm variant does not name a directory for its repo sources",
                      result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    def test_the_rpm_spec_projection_is_held_to_the_same_admission(self):
        # It reads the same flattened sources as the others, so a descriptor
        # they refuse cannot be projected into a spec file either.
        def mutate(descriptor):
            self.entry(descriptor, "license-apache")["source"]["path"] = "licenses/apache/LICENSE"
            self.entry(descriptor, "license-mit")["source"]["path"] = "licenses/mit/LICENSE"
        path = self.descriptor_with("rpm-spec-collision", mutate)
        result = tool("--descriptor", str(path), "rpm-spec-install", "--binaries-dir", "/binaries")
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("two sources prepared in the sources of rpm are one file", result.stderr)
        nominal = tool("rpm-spec-install", "--binaries-dir", "/binaries")
        self.assertEqual(nominal.returncode, 0, nominal.stderr)

    def test_a_directory_shared_by_several_kinds_must_be_declared_flattened(self):
        # The rule the declaration is held to, stated on a variant that does
        # not flatten today: give two kinds one directory and it must say so.
        def mutate(descriptor):
            descriptor["variants"]["debian"]["source_spaces"]["repo"] = "completions"
        path = self.descriptor_with("shared-space", mutate)
        result = tool("--descriptor", str(path), "manifest", "--variant", "debian")
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("prepares completion, repo in the same completions and does not declare "
                      "it flattened", result.stderr)

    def test_every_variant_says_which_of_its_spaces_are_flattened(self):
        descriptor = layout.load()
        flattened = {name: settings["flattened_source_spaces"]
                     for name, settings in descriptor["variants"].items()}
        self.assertEqual(flattened, {"unix-archive": [], "windows-archive": [],
                                     "debian": [], "rpm": ["sources"]})

    def test_every_variant_says_where_each_kind_of_source_is_prepared(self):
        descriptor = layout.load()
        for name, settings in descriptor["variants"].items():
            kinds = {entry["source"]["kind"]
                     for entry, _ in layout.entries(descriptor, name)}
            self.assertEqual(set(settings["source_spaces"]), kinds, name)
        # A kind with nowhere declared is refused rather than assumed.
        def mutate(document):
            del document["variants"]["unix-archive"]["source_spaces"]["completion"]
        path = self.descriptor_with("no-space", mutate)
        result = tool("--descriptor", str(path), "stage-plan", "--variant", "unix-archive")
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("the unix-archive variant declares no preparation space for completion "
                      "sources", result.stderr)

    def test_a_unique_rename_is_still_accepted_on_every_variant(self):
        def mutate(descriptor):
            self.entry(descriptor, "completion-zsh")["source"]["name"] = "_rmux-renamed"
        path = self.descriptor_with("unique-rename", mutate)
        for name in ("unix-archive", "windows-archive", "debian", "rpm"):
            result = tool("--descriptor", str(path), "source-names", "--variant", name,
                          "--kind", "completion", "--with-origin")
            self.assertEqual(result.returncode, 0, result.stderr)
            rows = [line.split("\t") for line in result.stdout.strip().splitlines()]
            self.assertEqual(len(rows), 5, name)
            self.assertIn(["zsh", "_rmux-renamed"], rows, name)

    def test_the_descriptor_as_written_has_no_collision(self):
        for name in layout.load()["variants"]:
            layout.validate_variant(layout.load(), name)


class DownstreamConsumers(unittest.TestCase):
    """The real generators, run against a descriptor mutated on its own copy.

    Each case moves one thing in the descriptor - where a file sits, which
    file a source is read from, which command a package exposes - and reads
    what the generator then writes. A consumer that kept its own copy of the
    layout would keep emitting the old answer.
    """

    SUMS = (
        "1111111111111111111111111111111111111111111111111111111111111111"
        "  rmux-0.10.0-windows-x86_64.zip\n"
        "2222222222222222222222222222222222222222222222222222222222222222"
        "  rmux-0.10.0-macos-aarch64.tar.gz\n"
        "3333333333333333333333333333333333333333333333333333333333333333"
        "  rmux-0.10.0-macos-x86_64.tar.gz\n"
    )

    def setUp(self):
        self.root, retain = fixture_root(self.id())
        if retain is not None:
            self.addCleanup(retain)
        self.checksums = self.root / "SHA256SUMS"
        self.checksums.write_text(self.SUMS, encoding="utf-8")

    def scripts_with(self, mutate=None):
        """A copy of scripts/, with the descriptor mutated inside the copy."""
        scripts = self.root / "scripts"
        if not scripts.exists():
            shutil.copytree(ROOT / "scripts", scripts)
        descriptor_path = scripts / "packaging" / "rmux-package-layout.json"
        descriptor = json.loads(descriptor_path.read_text(encoding="utf-8"))
        if mutate is not None:
            mutate(descriptor)
        descriptor_path.write_text(json.dumps(descriptor, indent=2), encoding="utf-8")
        return scripts

    def entry(self, descriptor, identifier):
        for entry in descriptor["entries"]:
            if entry["id"] == identifier:
                return entry
        raise AssertionError(f"no entry {identifier}")

    def generate(self, scripts, name, *arguments):
        result = subprocess.run(["bash", str(scripts / name), "--version", "0.10.0",
                                 "--checksums", str(self.checksums), *arguments],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def test_the_formula_follows_a_moved_file(self):
        def mutate(descriptor):
            placement = self.entry(descriptor, "man-page")["variants"]["unix-archive"]
            placement["path"] = "share/man/man1/rmux-renamed.1"
            placement["install"] = "share/man/man1/rmux-renamed.1"
        formula = self.generate(self.scripts_with(mutate), "generate-homebrew-formula.sh")
        self.assertIn('man1.install "share/man/man1/rmux-renamed.1"', formula)
        self.assertNotIn('man1.install "share/man/man1/rmux.1"', formula)

    def test_the_formula_installs_the_helper_where_the_descriptor_puts_it(self):
        formula = self.generate(self.scripts_with(), "generate-homebrew-formula.sh")
        self.assertIn('(libexec/"rmux").install "libexec/rmux/rmux"', formula)

    def test_the_winget_manifest_follows_the_declared_alias_and_path(self):
        def mutate(descriptor):
            descriptor["downstream"]["winget"]["portable_command_alias"] = "rmux-next"
            self.entry(descriptor, "cli")["variants"]["windows-archive"]["path"] = "rmux-next.exe"
        (self.root / "winget").mkdir(parents=True, exist_ok=True)
        self.generate(self.scripts_with(mutate), "generate-winget-manifest.sh",
                      "--output", str(self.root / "winget" / "Helvesec.RMUX.yaml"))
        body = (self.root / "winget" / "Helvesec.RMUX.installer.yaml").read_text(encoding="utf-8")
        self.assertIn("PortableCommandAlias: rmux-next", body)
        self.assertIn("RelativeFilePath: rmux-0.10.0-windows-x86_64\\rmux-next.exe", body)
        self.assertNotIn("PortableCommandAlias: rmux\n", body)

    def test_the_chocolatey_scripts_install_and_remove_the_same_commands(self):
        scripts = self.scripts_with()
        self.generate(scripts, "generate-chocolatey-package.sh",
                      "--output-dir", str(self.root / "choco"))
        install = (self.root / "choco/tools/chocolateyInstall.ps1").read_text(encoding="utf-8")
        uninstall = (self.root / "choco/tools/chocolateyUninstall.ps1").read_text(encoding="utf-8")
        projected = tool("chocolatey-shims")
        commands = [line.split("\t") for line in projected.stdout.strip().splitlines()]
        self.assertEqual([command for command, _ in commands], ["rmux", "rmux-daemon"])
        for command, path in commands:
            self.assertIn(f"Name = '{command}'; Path = (Join-Path $installDir '{path}')", install)
            self.assertIn(f"'{command}'", uninstall)
        # The helper travels in the package and is never put on the PATH: it
        # carries no command here, and the install script marks it so that the
        # engine's own shim pass leaves it alone.
        private = tool("chocolatey-private")
        self.assertIn("helper", private.stdout)
        self.assertNotIn("Name = 'rmux-full'", install)
        self.assertNotIn("Install-BinFile -Name 'libexec", install)
        self.assertIn("'libexec\\rmux\\rmux.exe'", install)
        self.assertNotIn("helper", uninstall)

    def test_the_chocolatey_install_ignores_every_executable_it_carries(self):
        # Chocolatey shims every executable a package carries unless a
        # <file>.ignore sits beside it, and it names each command after the
        # file's basename. Marking all of them is what makes the explicit
        # Install-BinFile list the whole command surface.
        scripts = self.scripts_with()
        self.generate(scripts, "generate-chocolatey-package.sh",
                      "--output-dir", str(self.root / "choco-ignores"))
        install = (self.root / "choco-ignores/tools/chocolateyInstall.ps1").read_text(encoding="utf-8")
        ignores = tool("chocolatey-ignores")
        self.assertEqual(ignores.returncode, 0, ignores.stderr)
        paths = ignores.stdout.strip().splitlines()
        self.assertIn("libexec\\rmux\\rmux.exe", paths)
        for path in paths:
            self.assertIn(f"  '{path}'", install)
        self.assertIn('New-Item -ItemType File -Path "$target.ignore"', install)
        # Every packaged executable is marked, the shimmed ones included: their
        # command comes from the descriptor, not from their file name.
        windows = tool("paths", "--variant", "windows-archive")
        carried = [line.split("\t")[0] for line in windows.stdout.strip().splitlines()
                   if line.lower().endswith(".exe")]
        self.assertEqual(sorted(paths), sorted(path.replace("/", "\\") for path in carried))
        for command, shimmed in (line.split("\t")
                                 for line in tool("chocolatey-shims").stdout.strip().splitlines()):
            self.assertIn(shimmed.replace("/", "\\"), paths)

    def test_a_command_alias_alone_adds_no_command_named_after_its_file(self):
        # Only the public name changes here: the daemon keeps its file name, so
        # nothing but the ignore marker stops Chocolatey's automatic pass from
        # adding rmux-daemon beside the declared rmuxd.
        def mutate(descriptor):
            descriptor["downstream"]["chocolatey"]["shims"][1]["command"] = "rmuxd"
        scripts = self.scripts_with(mutate)
        descriptor = scripts / "packaging" / "rmux-package-layout.json"
        self.generate(scripts, "generate-chocolatey-package.sh",
                      "--output-dir", str(self.root / "choco-alias"))
        install = (self.root / "choco-alias/tools/chocolateyInstall.ps1").read_text(encoding="utf-8")
        uninstall = (self.root / "choco-alias/tools/chocolateyUninstall.ps1").read_text(encoding="utf-8")
        ignores = tool("--descriptor", str(descriptor), "chocolatey-ignores")
        self.assertEqual(ignores.returncode, 0, ignores.stderr)
        paths = ignores.stdout.strip().splitlines()
        self.assertIn("rmux-daemon.exe", paths)
        self.assertIn("rmux.exe", paths)
        self.assertIn("  'rmux-daemon.exe'", install)
        # The package declares two commands and the file name is not one of them.
        self.assertIn("Name = 'rmuxd'; Path = (Join-Path $installDir 'rmux-daemon.exe')", install)
        self.assertIn("Name = 'rmux'; Path = (Join-Path $installDir 'rmux.exe')", install)
        self.assertEqual(install.count("[pscustomobject]@{ Name = "), 2)
        self.assertNotIn("Name = 'rmux-daemon'", install)
        self.assertIn("@('rmux', 'rmuxd')", uninstall)

    def test_a_renamed_executable_alone_keeps_its_declared_command(self):
        # The neighbour of the alias: only the file name moves, the command is
        # the one the descriptor already declared.
        def mutate(descriptor):
            self.entry(descriptor, "daemon")["variants"]["windows-archive"]["path"] = "rmuxd.exe"
        scripts = self.scripts_with(mutate)
        descriptor = scripts / "packaging" / "rmux-package-layout.json"
        self.generate(scripts, "generate-chocolatey-package.sh",
                      "--output-dir", str(self.root / "choco-file"))
        install = (self.root / "choco-file/tools/chocolateyInstall.ps1").read_text(encoding="utf-8")
        uninstall = (self.root / "choco-file/tools/chocolateyUninstall.ps1").read_text(encoding="utf-8")
        ignores = tool("--descriptor", str(descriptor), "chocolatey-ignores")
        self.assertEqual(ignores.returncode, 0, ignores.stderr)
        paths = ignores.stdout.strip().splitlines()
        self.assertIn("rmuxd.exe", paths)
        self.assertNotIn("rmux-daemon.exe", paths)
        self.assertIn("  'rmuxd.exe'", install)
        self.assertIn("Name = 'rmux-daemon'; Path = (Join-Path $installDir 'rmuxd.exe')", install)
        self.assertEqual(install.count("[pscustomobject]@{ Name = "), 2)
        self.assertIn("@('rmux', 'rmux-daemon')", uninstall)

    def test_a_packaged_executable_that_is_neither_shimmed_nor_private_is_refused(self):
        def mutate(descriptor):
            private = descriptor["downstream"]["chocolatey"]["private_entries"]
            private.remove("conpty-openconsole")
        scripts = self.scripts_with(mutate)
        result = subprocess.run(["bash", str(scripts / "generate-chocolatey-package.sh"),
                                 "--version", "0.10.0", "--checksums", str(self.checksums),
                                 "--output-dir", str(self.root / "choco-unclassified")],
                                capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("chocolatey neither shims nor declares private: conpty-openconsole",
                      result.stderr)

    def test_the_chocolatey_scripts_follow_a_renamed_command_and_executable(self):
        def mutate(descriptor):
            descriptor["downstream"]["chocolatey"]["shims"][1]["command"] = "rmuxd"
            self.entry(descriptor, "daemon")["variants"]["windows-archive"]["path"] = "rmuxd.exe"
        scripts = self.scripts_with(mutate)
        self.generate(scripts, "generate-chocolatey-package.sh",
                      "--output-dir", str(self.root / "choco-renamed"))
        install = (self.root / "choco-renamed/tools/chocolateyInstall.ps1").read_text(encoding="utf-8")
        uninstall = (self.root / "choco-renamed/tools/chocolateyUninstall.ps1").read_text(encoding="utf-8")
        self.assertIn("Name = 'rmuxd'; Path = (Join-Path $installDir 'rmuxd.exe')", install)
        self.assertIn("'rmuxd'", uninstall)
        self.assertNotIn("rmux-daemon", install)
        self.assertNotIn("rmux-daemon", uninstall)

    def test_a_shim_that_is_not_an_executable_is_refused(self):
        def mutate(descriptor):
            descriptor["downstream"]["chocolatey"]["shims"][0]["entry"] = "readme"
        scripts = self.scripts_with(mutate)
        result = subprocess.run(["bash", str(scripts / "generate-chocolatey-package.sh"),
                                 "--version", "0.10.0", "--checksums", str(self.checksums),
                                 "--output-dir", str(self.root / "choco-bad")],
                                capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("chocolatey shims a file that is not an executable: readme",
                      result.stderr)

    def test_an_entry_the_variant_does_not_carry_is_refused(self):
        def mutate(descriptor):
            descriptor["downstream"]["homebrew"]["install_map"][0]["entry"] = "conpty-dll"
        scripts = self.scripts_with(mutate)
        result = subprocess.run(["bash", str(scripts / "packaging" / "layout"),
                                 "homebrew-install"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("the unix-archive variant has no entry conpty-dll", result.stderr)


class PayloadProvenance(unittest.TestCase):
    """A payload the assembling repository did not build, bound to real files.

    The assembly metadata is true about the assembly; it says nothing about
    where the packaged binaries came from. The projection is what binds a
    declared payload source to the files a tree really carries, so these cases
    check that it accepts only a declaration the tree supports.
    """

    def setUp(self):
        self.root, retain = fixture_root(self.id())
        if retain is not None:
            self.addCleanup(retain)
        self.tree = self.root / "unix-archive"
        self.digests = {}
        for role, path, payload in (("cli", "bin/rmux", b"full-cli\n"),
                                    ("helper", "libexec/rmux/rmux", b"full-cli\n"),
                                    ("daemon", "bin/rmux-daemon", b"daemon\n")):
            target = self.tree / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(payload)
            self.digests[role] = hashlib.sha256(payload).hexdigest()

    def declaration(self, **changes):
        declared = {
            "source": "7f4d38e5ed0af845baee3ec585d232e5e94a20d0",
            "tree": "53a249cc1573948c0bfe282061de836ca5b2bb1c",
            "configuration": "x86_64-unknown-linux-gnu debug default,web",
            "files": [{"role": role, "sha256": digest}
                      for role, digest in self.digests.items()],
        }
        declared.update(changes)
        path = self.root / "declaration.json"
        path.write_text(json.dumps(declared), encoding="utf-8")
        return path

    def project(self, declaration, *extra):
        return tool("payload-provenance", "--variant", "unix-archive",
                    "--package-root", str(self.tree),
                    "--declaration", str(declaration), *extra)

    def test_a_declaration_the_tree_supports_is_bound_to_its_files(self):
        result = self.project(self.declaration())
        self.assertEqual(result.returncode, 0, result.stderr)
        projected = json.loads(result.stdout)
        self.assertEqual(projected["source"], "7f4d38e5ed0af845baee3ec585d232e5e94a20d0")
        self.assertEqual(projected["tree"], "53a249cc1573948c0bfe282061de836ca5b2bb1c")
        # descriptor order, which is what the projection documents
        self.assertEqual([file["path"] for file in projected["files"]],
                         ["bin/rmux", "libexec/rmux/rmux", "bin/rmux-daemon"])
        for file in projected["files"]:
            self.assertEqual(file["sha256"], self.digests[file["role"]])

    def test_two_slots_may_carry_the_same_image(self):
        projected = json.loads(self.project(self.declaration()).stdout)
        digests = {file["role"]: file["sha256"] for file in projected["files"]}
        self.assertEqual(digests["cli"], digests["helper"])

    def test_a_digest_the_file_does_not_have_is_refused(self):
        wrong = [{"role": role, "sha256": ("0" * 64 if role == "daemon" else digest)}
                 for role, digest in self.digests.items()]
        result = self.project(self.declaration(files=wrong))
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("bin/rmux-daemon is", result.stderr)
        self.assertIn("declared " + "0" * 64, result.stderr)

    def test_an_undeclared_binary_role_is_refused(self):
        partial = [{"role": role, "sha256": digest}
                   for role, digest in self.digests.items() if role != "helper"]
        result = self.project(self.declaration(files=partial))
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("undeclared binary role: helper", result.stderr)

    def test_a_role_the_variant_does_not_carry_is_refused(self):
        extra = [{"role": role, "sha256": digest} for role, digest in self.digests.items()]
        extra.append({"role": "conpty-dll", "sha256": "1" * 64})
        result = self.project(self.declaration(files=extra))
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("unknown binary role: conpty-dll", result.stderr)

    def test_a_missing_packaged_binary_is_refused(self):
        (self.tree / "bin/rmux").unlink()
        result = self.project(self.declaration())
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("missing packaged binary: bin/rmux", result.stderr)

    def test_an_empty_or_malformed_declaration_is_refused(self):
        for changes, expected in (({"source": ""}, "source must be a non-empty string"),
                                  ({"files": []}, "files must be a non-empty list")):
            result = self.project(self.declaration(**changes))
            self.assertEqual(result.returncode, 2, result.stdout)
            self.assertIn(expected, result.stderr)

    def test_the_embeddable_field_carries_the_projection(self):
        result = self.project(self.declaration(), "--field")
        self.assertEqual(result.returncode, 0, result.stderr)
        field = result.stdout.rstrip("\n")
        self.assertTrue(field.startswith('  "payload_provenance": {'), field[:40])
        self.assertTrue(field.endswith("},"), field[-10:])
        embedded = json.loads("{" + field.rstrip(",") + "}")
        self.assertEqual(embedded["payload_provenance"]["files"][0]["role"], "cli")


class PayloadProvenanceTopologyAndValidation(unittest.TestCase):
    """The two topologies the command names, and the validator it inherits.

    A package and a prefix are different trees of the same entries, and an
    identity its sibling projections refuse must not receive a positive
    provenance here. The Windows entries are the portable counter-example for
    a package path that differs from its installed destination; the files
    built here are inert bytes and nothing is ever executed.
    """

    WINDOWS = {"cli": ("rmux.exe", "bin/rmux.exe"),
               "helper": ("libexec/rmux/rmux.exe", "libexec/rmux/rmux.exe"),
               "daemon": ("rmux-daemon.exe", "bin/rmux-daemon.exe")}

    def setUp(self):
        self.root, retain = fixture_root(self.id())
        if retain is not None:
            self.addCleanup(retain)

    def tree_with(self, chooser, payloads):
        """One tree, each binary role written at the path the chooser picks."""
        root = self.root / "tree"
        digests = {}
        for role, paths in self.WINDOWS.items():
            target = root / chooser(paths)
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(payloads[role])
            digests[role] = hashlib.sha256(payloads[role]).hexdigest()
        return root, digests

    def declaration(self, digests, **changes):
        declared = {"source": "7f4d38e5ed0af845baee3ec585d232e5e94a20d0",
                    "tree": "53a249cc1573948c0bfe282061de836ca5b2bb1c",
                    "configuration": "inert test bytes, no build claimed",
                    "files": [{"role": role, "sha256": digest}
                              for role, digest in digests.items()]}
        declared.update(changes)
        path = self.root / f"declaration-{len(list(self.root.glob('declaration-*')))}.json"
        path.write_text(json.dumps(declared), encoding="utf-8")
        return path

    def tool_with_descriptor(self, mutate):
        """The same tool, reading a descriptor mutated in a copy of scripts/."""
        scripts = self.root / "scripts"
        if not scripts.exists():
            shutil.copytree(ROOT / "scripts", scripts)
        descriptor_path = scripts / "packaging" / "rmux-package-layout.json"
        descriptor = json.loads(descriptor_path.read_text(encoding="utf-8"))
        mutate(descriptor)
        descriptor_path.write_text(json.dumps(descriptor, indent=2), encoding="utf-8")
        return scripts / "packaging" / "layout.py"

    def run_tool(self, tool, *arguments):
        return subprocess.run([sys.executable, str(tool), *arguments],
                              capture_output=True, text=True)

    # ---- the tree is named, never guessed ------------------------------
    def test_an_installed_prefix_is_bound_at_its_destinations(self):
        payloads = {"cli": b"cli\n", "helper": b"helper\n", "daemon": b"daemon\n"}
        prefix, digests = self.tree_with(lambda paths: paths[1], payloads)
        result = tool("payload-provenance", "--variant", "windows-archive",
                      "--prefix", str(prefix), "--declaration", str(self.declaration(digests)))
        self.assertEqual(result.returncode, 0, result.stderr)
        projected = json.loads(result.stdout)
        self.assertEqual({file["role"]: file["path"] for file in projected["files"]},
                         {"cli": "bin/rmux.exe", "helper": "libexec/rmux/rmux.exe",
                          "daemon": "bin/rmux-daemon.exe"})

    def test_a_prefix_is_not_bound_at_the_package_paths(self):
        payloads = {"cli": b"cli\n", "helper": b"helper\n", "daemon": b"daemon\n"}
        prefix, digests = self.tree_with(lambda paths: paths[1], payloads)
        result = tool("payload-provenance", "--variant", "windows-archive",
                      "--package-root", str(prefix), "--declaration", str(self.declaration(digests)))
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("missing packaged binary: rmux.exe", result.stderr)

    def test_stray_copies_at_the_package_paths_are_not_what_a_prefix_verifies(self):
        installed = {"cli": b"installed-cli\n", "helper": b"installed-helper\n",
                     "daemon": b"installed-daemon\n"}
        prefix, digests = self.tree_with(lambda paths: paths[1], installed)
        stray = {"cli": b"stray-cli\n", "daemon": b"stray-daemon\n"}
        stray_digests = {}
        for role, payload in stray.items():
            target = prefix / self.WINDOWS[role][0]
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(payload)
            stray_digests[role] = hashlib.sha256(payload).hexdigest()
        accepted = tool("payload-provenance", "--variant", "windows-archive", "--prefix",
                        str(prefix), "--declaration", str(self.declaration(digests)))
        self.assertEqual(accepted.returncode, 0, accepted.stderr)
        self.assertEqual(json.loads(accepted.stdout)["files"][0]["sha256"], digests["cli"])
        # the same prefix, declared with the digests of the stray copies
        wrong = dict(digests)
        wrong.update(stray_digests)
        refused = tool("payload-provenance", "--variant", "windows-archive", "--prefix",
                       str(prefix), "--declaration", str(self.declaration(wrong)))
        self.assertEqual(refused.returncode, 2, refused.stdout)
        self.assertIn("bin/rmux.exe is", refused.stderr)

    def test_exactly_one_tree_must_be_named(self):
        payloads = {"cli": b"cli\n", "helper": b"helper\n", "daemon": b"daemon\n"}
        prefix, digests = self.tree_with(lambda paths: paths[1], payloads)
        declaration = str(self.declaration(digests))
        neither = tool("payload-provenance", "--variant", "windows-archive",
                       "--declaration", declaration)
        both = tool("payload-provenance", "--variant", "windows-archive", "--prefix", str(prefix),
                    "--package-root", str(prefix), "--declaration", declaration)
        for result in (neither, both):
            self.assertEqual(result.returncode, 2, result.stdout)
            self.assertIn("name exactly one tree", result.stderr)

    def test_a_role_the_variant_does_not_install_cannot_be_bound_in_a_prefix(self):
        def drop_install(descriptor):
            for entry in descriptor["entries"]:
                if entry["id"] == "daemon":
                    del entry["variants"]["windows-archive"]["install"]
        tool_path = self.tool_with_descriptor(drop_install)
        payloads = {"cli": b"cli\n", "helper": b"helper\n", "daemon": b"daemon\n"}
        prefix, digests = self.tree_with(lambda paths: paths[1], payloads)
        result = self.run_tool(tool_path, "payload-provenance", "--variant", "windows-archive",
                               "--prefix", str(prefix), "--declaration", str(self.declaration(digests)))
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("daemon is not installed by windows-archive", result.stderr)

    # ---- the validator the sibling projections apply --------------------
    def test_a_descriptor_its_siblings_refuse_cannot_receive_a_provenance(self):
        def duplicate_cli(descriptor):
            for entry in list(descriptor["entries"]):
                if entry["id"] == "cli":
                    descriptor["entries"].append(json.loads(json.dumps(entry)))
                    break
        tool_path = self.tool_with_descriptor(duplicate_cli)
        payloads = {"cli": b"cli\n", "helper": b"helper\n", "daemon": b"daemon\n"}
        package, digests = self.tree_with(lambda paths: paths[0], payloads)
        declaration = str(self.declaration(digests))
        for extra in ((), ("--field",)):
            result = self.run_tool(tool_path, "payload-provenance", "--variant", "windows-archive",
                                   "--package-root", str(package), "--declaration", declaration, *extra)
            self.assertEqual(result.returncode, 2, result.stdout)
            self.assertIn("two entries share the id cli", result.stderr)
        # the same duplicated descriptor is refused by a sibling projection
        sibling = self.run_tool(tool_path, "manifest", "--variant", "windows-archive")
        self.assertEqual(sibling.returncode, 2, sibling.stdout)

    def test_a_role_that_is_not_a_string_is_refused_by_name(self):
        payloads = {"cli": b"cli\n", "helper": b"helper\n", "daemon": b"daemon\n"}
        package, digests = self.tree_with(lambda paths: paths[0], payloads)
        for role in ([], {}, 7):
            declaration = self.declaration(
                digests, files=[{"role": role, "sha256": "0" * 64}])
            result = tool("payload-provenance", "--variant", "windows-archive",
                          "--package-root", str(package), "--declaration", str(declaration))
            self.assertEqual(result.returncode, 2, result.stdout)
            self.assertIn("a role must be a string", result.stderr)
            self.assertNotIn("Traceback", result.stderr)


if __name__ == "__main__":
    unittest.main()
