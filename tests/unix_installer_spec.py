"""The Unix archive installer, against archives built here file by file.

The installer is run as the packaged installer really is: a copy of the script
inside an archive root, with a prefix of its own. The payloads are sentinels,
not RMUX; the dispatcher is a shell script that answers like the CLI so the
transaction reaches its layout verification. What each case asserts is the
refusal or the installed result, and that a refused archive leaves the prefix
exactly as it was.
"""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tarfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts" / "packaging"))

import layout  # noqa: E402
INSTALLER = ROOT / "scripts" / "install-unix-archive.sh"
TOOL = ROOT / "scripts" / "packaging" / "layout.py"

# The Unix archive, transcribed from the descriptor by hand: every path the
# package carries and the mode it carries it with.
ARCHIVE = {
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

DISPATCHER = b"""#!/bin/sh
printf 'usage: rmux [command]\\n'
exit 1
"""


def write(path, contents, mode):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(contents)
    os.chmod(path, int(mode, 8))


# The packaged Unix installer is a bash script that stages, backs up and
# replaces files by POSIX mode. A host without those modes cannot run these
# cases, so they are skipped there rather than weakened everywhere.
@unittest.skipUnless(os.name == "posix", "this host does not run the Unix archive installer")
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


class InstallerCase(unittest.TestCase):
    installer = INSTALLER

    def setUp(self):
        self.root, retain = fixture_root(self.id())
        if retain is not None:
            self.addCleanup(retain)

    def manifest(self):
        result = subprocess.run([sys.executable, str(TOOL), "manifest",
                                 "--variant", "unix-archive"],
                                capture_output=True, text=True, check=True)
        return result.stdout

    def archive(self, label, *, manifest=None, extra=None, skip=(), metadata=None):
        archive = self.root / label
        for path, mode in ARCHIVE.items():
            if path in skip:
                continue
            if path == "install.sh":
                continue
            if path == "share/rmux/package-layout.tsv":
                continue
            if path == "share/rmux/artifact-metadata.json":
                body = (metadata if metadata is not None
                        else b'{"package_layout": "rmux-package-v3"}\n')
                write(archive / path, body, mode)
                continue
            body = DISPATCHER if path == "bin/rmux" else f"payload of {path}\n".encode()
            write(archive / path, body, mode)
        shutil.copyfile(self.installer, archive / "install.sh")
        os.chmod(archive / "install.sh", 0o755)
        if "share/rmux/package-layout.tsv" not in skip:
            write(archive / "share/rmux/package-layout.tsv",
                  (manifest if manifest is not None else self.manifest()).encode(), "0644")
        for path, body in (extra or {}).items():
            write(archive / path, body, "0644")
        return archive

    def install(self, archive, prefix, *extra_arguments, environment=None):
        return subprocess.run([str(archive / "install.sh"), "--prefix", str(prefix),
                               *extra_arguments],
                              capture_output=True, text=True,
                              env=dict(os.environ, **(environment or {})))

    def seeded_prefix(self, label):
        """A prefix that already holds a dispatcher nobody may replace silently."""
        prefix = self.root / label
        write(prefix / "bin/rmux", b"#!/bin/sh\nprintf 'usage: rmux previous\\n'\n", "0755")
        return prefix


class ReleaseProvenance(InstallerCase):
    def test_release_archive_requires_an_exact_clean_source_commit(self):
        commit = "1234567890abcdef1234567890abcdef12345678"
        metadata = dict(artifact_kind="unix-package-binary", package_layout="rmux-package-v3",
                        git_commit=commit, configuration="release", release_artifact=True,
                        git_dirty=False, skip_build=False)
        for label, changes, expected_commit, accepted in (
                ("valid", {}, None, True),
                ("invalid-commit", {"git_commit": None}, None, False),
                ("dirty", {"git_dirty": True}, None, False),
                ("skipped", {"skip_build": True}, None, False),
                ("pinned", {}, commit, True),
                ("other-commit", {}, "0" * 40, False)):
            with self.subTest(label=label):
                tree = self.archive("rmux-" + label)
                value = dict(metadata, **changes)
                for field, path in (("binary", "bin/rmux"),
                                    ("helper_binary", "libexec/rmux/rmux"),
                                    ("daemon_binary", "bin/rmux-daemon")):
                    value[field + "_sha256"] = hashlib.sha256((tree / path).read_bytes()).hexdigest()
                (tree / "share/rmux/artifact-metadata.json").write_text(json.dumps(value, indent=2) + "\n")
                files = sorted(p for p in tree.rglob("*") if p.is_file() and p.name != "SHA256SUMS.txt")
                (tree / "SHA256SUMS.txt").write_text("".join(
                    f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.relative_to(tree)}\n" for p in files))
                archive = self.root / (tree.name + ".tar.gz")
                with tarfile.open(archive, "w:gz") as output:
                    output.add(tree, arcname=tree.name)
                (self.root / "SHA256SUMS.txt").write_text(
                    f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n")
                args = [str(ROOT / "scripts/verify-package.sh"), str(archive), "--require-release-artifact"]
                if expected_commit is not None:
                    args.extend(("--expected-commit", expected_commit))
                result = subprocess.run(args, capture_output=True, text=True)
                self.assertEqual(result.returncode == 0, accepted, result.stdout + result.stderr)
                if not accepted:
                    self.assertIn("source provenance", result.stderr)


class Membership(InstallerCase):
    def test_an_extra_file_named_like_a_declared_suffix_is_refused(self):
        # "bin/rmux" is declared; a file called "rmux" at the archive root is
        # not, and used to be accepted because the declaration was matched as
        # a suffix instead of a complete entry.
        archive = self.archive("suffix", extra={"rmux": b"undeclared\n"})
        prefix = self.seeded_prefix("suffix-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("archive carries an undeclared file: rmux", result.stderr)
        self.assertEqual((prefix / "bin/rmux").read_bytes(),
                         b"#!/bin/sh\nprintf 'usage: rmux previous\\n'\n")
        self.assertFalse((prefix / "bin/rmux-daemon").exists())

    def test_an_ordinary_extra_file_is_refused(self):
        archive = self.archive("extra", extra={"share/rmux/leftover.tmp": b"extra\n"})
        prefix = self.seeded_prefix("extra-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("archive carries an undeclared file: share/rmux/leftover.tmp",
                      result.stderr)
        self.assertFalse((prefix / "libexec/rmux/rmux").exists())

    def test_a_path_declared_twice_is_refused(self):
        rows = self.manifest().splitlines()
        # The repeated row carries an id of its own, so what is refused here
        # is the repeated path and not the repeated identity.
        repeat = rows[0].split("\t")
        repeat[4] = "helper-again"
        duplicated = "\n".join(rows + ["\t".join(repeat)]) + "\n"
        repeated = rows[0].split("\t")[0]
        archive = self.archive("repeat", manifest=duplicated)
        prefix = self.seeded_prefix("repeat-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(f"layout manifest declares {repeated} twice", result.stderr)
        self.assertFalse((prefix / "bin/rmux-daemon").exists())
        self.assertEqual((prefix / "bin/rmux").read_bytes(),
                         b"#!/bin/sh\nprintf 'usage: rmux previous\\n'\n")

    def test_a_faithful_archive_installs(self):
        archive = self.archive("good")
        prefix = self.root / "good-prefix"
        result = self.install(archive, prefix)
        self.assertEqual(result.returncode, 0, result.stderr)
        for destination in ("bin/rmux", "bin/rmux-daemon", "libexec/rmux/rmux",
                            "share/man/man1/rmux.1"):
            self.assertTrue((prefix / destination).is_file(), destination)


class Formats(InstallerCase):
    """Legacy package, current package, and a current package that is malformed."""

    LEGACY = b'{"package_layout": "rmux-package-v2"}\n'
    CURRENT = b'{"package_layout": "rmux-package-v3"}\n'

    def test_a_package_built_before_the_manifest_still_installs(self):
        # rmux-package-v2 is the format the base produced: no layout manifest,
        # three executables and whatever it carries under share/.
        archive = self.archive("legacy", skip={"share/rmux/package-layout.tsv"},
                               metadata=self.LEGACY)
        prefix = self.root / "legacy-prefix"
        result = self.install(archive, prefix)
        self.assertEqual(result.returncode, 0, result.stderr)
        for destination in ("bin/rmux", "bin/rmux-daemon", "libexec/rmux/rmux"):
            self.assertTrue((prefix / destination).is_file(), destination)
            self.assertTrue(os.access(prefix / destination, os.X_OK), destination)
        for asset in ("share/man/man1/rmux.1",
                      "share/zsh/site-functions/_rmux",
                      "share/rmux/artifact-metadata.json"):
            self.assertEqual((prefix / asset).read_bytes(),
                             (archive / asset).read_bytes(), asset)

    def test_a_current_package_without_its_manifest_is_refused(self):
        archive = self.archive("no-manifest", skip={"share/rmux/package-layout.tsv"},
                               metadata=self.CURRENT)
        prefix = self.seeded_prefix("no-manifest-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("archive declares rmux-package-v3 but is missing its layout "
                      "manifest: share/rmux/package-layout.tsv", result.stderr)
        self.assertFalse((prefix / "bin/rmux-daemon").exists())

    def test_a_legacy_package_carrying_a_manifest_is_refused(self):
        archive = self.archive("legacy-with-manifest", metadata=self.LEGACY)
        prefix = self.seeded_prefix("legacy-with-manifest-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("archive declares rmux-package-v2 but carries a layout manifest",
                      result.stderr)
        self.assertFalse((prefix / "bin/rmux-daemon").exists())

    def test_an_unknown_format_is_refused_by_name(self):
        archive = self.archive("unknown", metadata=b'{"package_layout": "rmux-package-v9"}\n')
        prefix = self.seeded_prefix("unknown-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("unsupported package layout: rmux-package-v9 "
                      "(supported: rmux-package-v3, rmux-package-v2)", result.stderr)
        self.assertFalse((prefix / "bin/rmux-daemon").exists())

    def test_a_package_carrying_a_manifest_but_no_declared_format_is_refused(self):
        # Neither of the two formats: it carries the current format's manifest
        # and says nothing, so it is not taken for the older layout.
        archive = self.archive("silent", metadata=b'{"schema": 1}\n')
        prefix = self.seeded_prefix("silent-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("archive carries a layout manifest but declares no package layout",
                      result.stderr)
        self.assertFalse((prefix / "bin/rmux-daemon").exists())

    def test_a_package_without_metadata_or_manifest_installs_as_the_older_layout(self):
        # The shape 956d566 produced and installed: three executables and
        # whatever it carries under share/, with no declaration of any kind.
        archive = self.archive("undeclared",
                               skip={"share/rmux/package-layout.tsv",
                                     "share/rmux/artifact-metadata.json"})
        prefix = self.root / "undeclared-prefix"
        result = self.install(archive, prefix)
        self.assertEqual(result.returncode, 0, result.stderr)
        for destination in ("bin/rmux", "bin/rmux-daemon", "libexec/rmux/rmux"):
            self.assertTrue((prefix / destination).is_file(), destination)
        for asset in ("share/man/man1/rmux.1", "share/zsh/site-functions/_rmux"):
            self.assertEqual((prefix / asset).read_bytes(),
                             (archive / asset).read_bytes(), asset)

    def test_an_archive_without_metadata_that_is_not_the_older_layout_is_refused(self):
        archive = self.archive("not-legacy",
                               skip={"share/rmux/package-layout.tsv",
                                     "share/rmux/artifact-metadata.json",
                                     "libexec/rmux/rmux"})
        prefix = self.seeded_prefix("not-legacy-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("archive declares no package layout and does not carry the "
                      "rmux-package-v2 layout: libexec/rmux/rmux is missing", result.stderr)
        self.assertFalse((prefix / "bin/rmux-daemon").exists())


class Collisions(InstallerCase):
    """Manifests whose destinations would install onto each other."""

    def aimed_at_the_dispatcher(self):
        """The manual page, with only its destination column changed."""
        rows = []
        for line in self.manifest().splitlines():
            fields = line.split("\t")
            if fields[0] == "share/man/man1/rmux.1":
                fields[2] = "bin/rmux"
            rows.append("\t".join(fields))
        return "\n".join(rows) + "\n"

    def test_an_asset_aimed_at_the_dispatcher_is_refused_before_any_mutation(self):
        archive = self.archive("aimed", manifest=self.aimed_at_the_dispatcher())
        prefix = self.seeded_prefix("aimed-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("layout manifest installs two entries to bin/rmux", result.stderr)
        # The dispatcher that was there is the dispatcher that is still there,
        # and nothing else was installed alongside it.
        self.assertEqual((prefix / "bin/rmux").read_bytes(),
                         b"#!/bin/sh\nprintf 'usage: rmux previous\\n'\n")
        self.assertFalse((prefix / "bin/rmux-daemon").exists())
        self.assertFalse((prefix / "libexec/rmux/rmux").exists())
        self.assertFalse((prefix / "share/man/man1/rmux.1").exists())

    def test_two_assets_sharing_a_destination_are_refused(self):
        rows = []
        for line in self.manifest().splitlines():
            fields = line.split("\t")
            if fields[0] == "share/elvish/lib/rmux.elv":
                fields[2] = "share/man/man1/rmux.1"
            rows.append("\t".join(fields))
        archive = self.archive("two-assets", manifest="\n".join(rows) + "\n")
        prefix = self.seeded_prefix("two-assets-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("layout manifest installs two entries to share/man/man1/rmux.1",
                      result.stderr)
        self.assertFalse((prefix / "share/man/man1/rmux.1").exists())

    def test_an_unknown_role_is_refused(self):
        rows = []
        for line in self.manifest().splitlines():
            fields = line.split("\t")
            if fields[0] == "bin/rmux":
                fields[5] = "dispatcher"
            rows.append("\t".join(fields))
        archive = self.archive("role", manifest="\n".join(rows) + "\n")
        prefix = self.seeded_prefix("role-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("unknown layout manifest role: dispatcher", result.stderr)
        self.assertFalse((prefix / "bin/rmux-daemon").exists())

    def test_a_manifest_line_without_its_columns_is_refused(self):
        rows = [line.rsplit("\t", 2)[0] if line.startswith("bin/rmux\t") else line
                for line in self.manifest().splitlines()]
        archive = self.archive("short", manifest="\n".join(rows) + "\n")
        prefix = self.seeded_prefix("short-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("invalid layout manifest line", result.stderr)
        self.assertFalse((prefix / "bin/rmux-daemon").exists())

    def test_an_asset_keeps_its_own_destination_when_the_manifest_is_sound(self):
        archive = self.archive("sound")
        prefix = self.root / "sound-prefix"
        result = self.install(archive, prefix)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((prefix / "share/man/man1/rmux.1").read_bytes(),
                         (archive / "share/man/man1/rmux.1").read_bytes())
        self.assertEqual((prefix / "bin/rmux").read_bytes(), DISPATCHER)


class EquivalentPaths(InstallerCase):
    """Destinations that name one file under more than one spelling."""

    def aimed(self, spelling):
        rows = []
        for line in self.manifest().splitlines():
            fields = line.split("\t")
            if fields[0] == "share/man/man1/rmux.1":
                fields[2] = spelling
            rows.append("\t".join(fields))
        return "\n".join(rows) + "\n"

    def refused(self, label, spelling):
        archive = self.archive(label, manifest=self.aimed(spelling))
        prefix = self.seeded_prefix(f"{label}-prefix")
        before = sorted(p.relative_to(prefix).as_posix() for p in prefix.rglob("*") if p.is_file())
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        # Nothing was staged, replaced or published, and the dispatcher that
        # was there is byte for byte the one still there.
        self.assertEqual((prefix / "bin/rmux").read_bytes(),
                         b"#!/bin/sh\nprintf 'usage: rmux previous\\n'\n")
        self.assertEqual(sorted(p.relative_to(prefix).as_posix()
                                for p in prefix.rglob("*") if p.is_file()), before)
        return result

    def test_a_dot_segment_reaching_the_dispatcher_is_refused(self):
        result = self.refused("dot", "bin/./rmux")
        self.assertIn("non-canonical layout manifest path: bin/./rmux", result.stderr)

    def test_a_repeated_separator_reaching_the_dispatcher_is_refused(self):
        result = self.refused("slashes", "bin//rmux")
        self.assertIn("non-canonical layout manifest path: bin//rmux", result.stderr)

    def test_a_trailing_separator_and_a_parent_segment_are_refused(self):
        self.assertIn("non-canonical layout manifest path: bin/rmux/",
                      self.refused("trailing", "bin/rmux/").stderr)
        self.assertIn("non-canonical layout manifest path: bin/../bin/rmux",
                      self.refused("parent", "bin/../bin/rmux").stderr)

    def test_a_case_collision_is_still_refused_and_a_canonical_plan_still_installs(self):
        archive = self.archive("case", manifest=self.aimed("bin/RMUX"))
        prefix = self.seeded_prefix("case-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("layout manifest installs two entries to bin/RMUX", result.stderr)
        archive = self.archive("canonical")
        prefix = self.root / "canonical-prefix"
        self.assertEqual(self.install(archive, prefix).returncode, 0)
        self.assertEqual((prefix / "share/man/man1/rmux.1").read_bytes(),
                         (archive / "share/man/man1/rmux.1").read_bytes())


class EntryIdentities(InstallerCase):
    """Manifest identities, and what a transaction stores under them."""

    def with_id(self, path, identifier, label):
        rows = []
        for line in self.manifest().splitlines():
            fields = line.split("\t")
            if fields[0] == path:
                fields[4] = identifier
            rows.append("\t".join(fields))
        return self.archive(label, manifest="\n".join(rows) + "\n")

    def seeded_executables(self, label):
        """A prefix whose three executables hold distinct, recognisable bytes."""
        prefix = self.root / label
        for destination, body in (("bin/rmux", b"#!/bin/sh\n# old dispatcher\n"),
                                  ("bin/rmux-daemon", b"#!/bin/sh\n# old daemon\n"),
                                  ("libexec/rmux/rmux", b"#!/bin/sh\n# old helper\n")):
            write(prefix / destination, body, "0755")
        return prefix

    def test_a_duplicated_entry_id_is_refused_before_any_mutation(self):
        # Sources, modes, destinations, roles and membership all stay
        # admissible; only the identity is repeated, and two executables would
        # then be stored under one name.
        archive = self.with_id("bin/rmux", "helper", "dup-id")
        prefix = self.seeded_executables("dup-id-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("layout manifest declares the entry id helper twice", result.stderr)
        self.assertEqual((prefix / "libexec/rmux/rmux").read_bytes(),
                         b"#!/bin/sh\n# old helper\n")
        self.assertEqual((prefix / "bin/rmux").read_bytes(), b"#!/bin/sh\n# old dispatcher\n")
        self.assertEqual((prefix / "bin/rmux-daemon").read_bytes(), b"#!/bin/sh\n# old daemon\n")

    def test_an_absent_entry_id_is_refused(self):
        # The columns are separated by tabs, and a shell collapses a run of
        # them, so an empty identity column reaches the installer as a line
        # with one column missing. It is refused there, by line, before any
        # mutation, which is what this case is about.
        archive = self.with_id("bin/rmux", "", "no-id")
        prefix = self.seeded_executables("no-id-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("invalid layout manifest line 3: bin/rmux", result.stderr)
        self.assertEqual((prefix / "bin/rmux").read_bytes(), b"#!/bin/sh\n# old dispatcher\n")

    def test_an_entry_id_that_could_not_name_a_file_is_refused(self):
        for identifier, label in (("../escape", "escape"), ("Helper", "upper"),
                                  ("-leading", "leading")):
            archive = self.with_id("bin/rmux", identifier, f"id-{label}")
            prefix = self.seeded_executables(f"id-{label}-prefix")
            result = self.install(archive, prefix)
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertIn(f"layout manifest entry id is not usable: {identifier}",
                          result.stderr)
            self.assertEqual((prefix / "bin/rmux").read_bytes(),
                             b"#!/bin/sh\n# old dispatcher\n")

    def test_each_destination_keeps_its_own_snapshot_when_a_failure_rolls_back(self):
        # The three old executables differ; after a failure injected once the
        # set has been replaced, each destination must come back with its own
        # previous bytes and mode, and the package assets must be gone.
        archive = self.archive("rollback")
        prefix = self.seeded_executables("rollback-prefix")
        write(prefix / "share/rmux/local-only", b"keep-local-asset\n", "0600")
        result = self.install(archive, prefix, environment={"RMUX_INSTALL_TEST_FAIL_AT": "after-verify"})
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertEqual((prefix / "bin/rmux").read_bytes(), b"#!/bin/sh\n# old dispatcher\n")
        self.assertEqual((prefix / "bin/rmux-daemon").read_bytes(), b"#!/bin/sh\n# old daemon\n")
        self.assertEqual((prefix / "libexec/rmux/rmux").read_bytes(), b"#!/bin/sh\n# old helper\n")
        for destination in ("bin/rmux", "bin/rmux-daemon", "libexec/rmux/rmux"):
            self.assertEqual(oct(os.stat(prefix / destination).st_mode & 0o7777), "0o755")
        self.assertFalse((prefix / "share/man/man1/rmux.1").exists())
        self.assertEqual((prefix / "share/rmux/local-only").read_bytes(), b"keep-local-asset\n")


class Declarations(InstallerCase):
    """What the metadata says, how it says it, and what it fails to say."""

    # The declaration of A's counterexample: valid JSON, the key and its value
    # on different lines. Read line by line, no line carries both.
    SPLIT_V3 = b'{\n  "package_layout":\n    "rmux-package-v3"\n}\n'
    SPLIT_UNKNOWN = b'{\n  "package_layout":\n    "rmux-package-v9"\n}\n'

    def test_a_declaration_split_across_lines_is_a_declaration(self):
        # Three executables, no manifest: the shape the legacy path admits.
        # What refuses it is the declaration it carries.
        archive = self.archive("split-v3", skip={"share/rmux/package-layout.tsv"},
                               metadata=self.SPLIT_V3)
        prefix = self.seeded_prefix("split-v3-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("archive declares rmux-package-v3 but is missing its layout "
                      "manifest: share/rmux/package-layout.tsv", result.stderr)
        self.assertEqual((prefix / "bin/rmux").read_bytes(),
                         b"#!/bin/sh\nprintf 'usage: rmux previous\\n'\n")
        self.assertFalse((prefix / "bin/rmux-daemon").exists())

    def test_an_unknown_identity_split_across_lines_is_refused(self):
        archive = self.archive("split-unknown", skip={"share/rmux/package-layout.tsv"},
                               metadata=self.SPLIT_UNKNOWN)
        prefix = self.seeded_prefix("split-unknown-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("unsupported package layout: rmux-package-v9", result.stderr)
        self.assertFalse((prefix / "bin/rmux-daemon").exists())

    def test_metadata_that_is_not_a_json_object_is_refused(self):
        archive = self.archive("not-json", skip={"share/rmux/package-layout.tsv"},
                               metadata=b'package_layout = rmux-package-v3\n')
        prefix = self.seeded_prefix("not-json-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("archive metadata is not a valid JSON object", result.stderr)
        self.assertFalse((prefix / "bin/rmux-daemon").exists())

    def test_a_declaration_this_installer_cannot_read_is_refused(self):
        for label, body in (("null", b'{"package_layout": null}\n'),
                            ("number", b'{"package_layout": 3}\n')):
            archive = self.archive(f"unreadable-{label}",
                                   skip={"share/rmux/package-layout.tsv"},
                                   metadata=body)
            prefix = self.seeded_prefix(f"unreadable-{label}-prefix")
            result = self.install(archive, prefix)
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertIn("archive metadata declares a package layout that is not a string",
                          result.stderr)
            self.assertFalse((prefix / "bin/rmux-daemon").exists())

    def test_two_declarations_are_refused(self):
        archive = self.archive("twice-declared", skip={"share/rmux/package-layout.tsv"},
                               metadata=b'{"package_layout": "rmux-package-v2",\n'
                                        b' "package_layout": "rmux-package-v3"}\n')
        prefix = self.seeded_prefix("twice-declared-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("archive metadata declares its package layout more than once", result.stderr)
        self.assertFalse((prefix / "bin/rmux-daemon").exists())

    def test_an_invalid_document_is_not_an_absence(self):
        # {not-json} has the outline of an object and nothing else. Reading it
        # as text finds no declaration, which is not the same as a document
        # that declares none.
        archive = self.archive("not-an-object", skip={"share/rmux/package-layout.tsv"},
                               metadata=b"{not-json}\n")
        prefix = self.seeded_prefix("not-an-object-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("archive metadata is not a valid JSON object", result.stderr)
        self.assertEqual((prefix / "bin/rmux").read_bytes(),
                         b"#!/bin/sh\nprintf 'usage: rmux previous\\n'\n")
        self.assertFalse((prefix / "bin/rmux-daemon").exists())

    def test_an_escaped_property_name_is_the_property(self):
        # package_\u006cayout is package_layout once the escape is decoded, so
        # this document declares the current format and has no manifest.
        archive = self.archive("escaped-key", skip={"share/rmux/package-layout.tsv"},
                               metadata=b'{"package_\\u006cayout":"rmux-package-v3"}\n')
        prefix = self.seeded_prefix("escaped-key-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("archive declares rmux-package-v3 but is missing its layout "
                      "manifest", result.stderr)
        self.assertFalse((prefix / "bin/rmux-daemon").exists())

    def test_the_property_name_as_a_value_declares_nothing(self):
        # A valid object whose only member holds that text as its value
        # declares no format, so the older layout is admitted on its own
        # conditions instead of being refused.
        archive = self.archive("name-as-value", skip={"share/rmux/package-layout.tsv"},
                               metadata=b'{"note":"package_layout"}\n')
        prefix = self.root / "name-as-value-prefix"
        result = self.install(archive, prefix)
        self.assertEqual(result.returncode, 0, result.stderr)
        for destination in ("bin/rmux", "bin/rmux-daemon", "libexec/rmux/rmux"):
            self.assertTrue((prefix / destination).is_file(), destination)

    def test_a_declaration_nested_in_another_object_is_not_the_declaration(self):
        archive = self.archive("nested", skip={"share/rmux/package-layout.tsv"},
                               metadata=b'{"nested": {"package_layout": "rmux-package-v3"},'
                                        b' "schema": 1}\n')
        prefix = self.root / "nested-prefix"
        result = self.install(archive, prefix)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((prefix / "bin/rmux").is_file())

    def test_a_number_with_a_leading_zero_is_not_a_document(self):
        # 01 is not a JSON number, so this file is not a JSON document, and a
        # document that cannot be read is not one that declares nothing.
        for label, body in (("top-level", b'{"schema":01}\n'),
                            ("nested", b'{"nested": {"bytes": 007}, "schema": 1}\n'),
                            ("in-an-array", b'{"sizes": [1, 02]}\n')):
            archive = self.archive(f"leading-zero-{label}",
                                   skip={"share/rmux/package-layout.tsv"}, metadata=body)
            prefix = self.seeded_prefix(f"leading-zero-{label}-prefix")
            result = self.install(archive, prefix)
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertIn("archive metadata is not a valid JSON object", result.stderr)
            self.assertEqual((prefix / "bin/rmux").read_bytes(),
                             b"#!/bin/sh\nprintf 'usage: rmux previous\\n'\n")
            self.assertFalse((prefix / "bin/rmux-daemon").exists())

    def test_a_number_that_is_valid_json_is_still_read(self):
        archive = self.archive("valid-numbers", skip={"share/rmux/package-layout.tsv"},
                               metadata=b'{"schema": 0, "bytes": 10, "ratio": -0.5,'
                                        b' "exp": 1e3, "layouts": [0, 12]}\n')
        prefix = self.root / "valid-numbers-prefix"
        result = self.install(archive, prefix)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((prefix / "bin/rmux").is_file())

    def test_a_declared_identity_keeps_what_it_ends_with(self):
        # The declared value is rmux-package-v2 followed by a line feed, which
        # is not the identity of the older layout. Trimming that line feed on
        # the way to the comparison would turn an unknown identity into the
        # one this installer admits.
        archive = self.archive("trailing-newline", skip={"share/rmux/package-layout.tsv"},
                               metadata=b'{"package_layout":"rmux-package-v2\\n"}\n')
        prefix = self.seeded_prefix("trailing-newline-prefix")
        result = self.install(archive, prefix)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("unsupported package layout", result.stderr)
        self.assertEqual((prefix / "bin/rmux").read_bytes(),
                         b"#!/bin/sh\nprintf 'usage: rmux previous\\n'\n")
        self.assertFalse((prefix / "bin/rmux-daemon").exists())

    def test_an_identity_with_other_whitespace_is_unknown_too(self):
        for label, body in (("leading-space", b'{"package_layout":" rmux-package-v2"}\n'),
                            ("tab", b'{"package_layout":"rmux-package-v2\\t"}\n'),
                            ("carriage-return", b'{"package_layout":"rmux-package-v2\\r"}\n')):
            archive = self.archive(f"whitespace-{label}",
                                   skip={"share/rmux/package-layout.tsv"}, metadata=body)
            prefix = self.seeded_prefix(f"whitespace-{label}-prefix")
            result = self.install(archive, prefix)
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertIn("unsupported package layout", result.stderr)
            self.assertFalse((prefix / "bin/rmux-daemon").exists())

    def test_an_empty_or_non_object_document_is_refused(self):
        # The same two shapes the Windows installer names, so both answer an
        # unreadable document the same way.
        for label, body in (("empty", b""), ("scalar", b'"rmux-package-v2"\n')):
            archive = self.archive(f"empty-{label}", skip={"share/rmux/package-layout.tsv"},
                                   metadata=body)
            prefix = self.seeded_prefix(f"empty-{label}-prefix")
            result = self.install(archive, prefix)
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertIn("archive metadata is not a valid JSON object", result.stderr)
            self.assertFalse((prefix / "bin/rmux-daemon").exists())

    def test_a_document_that_ends_badly_is_refused(self):
        for label, body in (("unterminated", b'{"package_layout": "rmux'),
                            ("trailing-comma", b'{"package_layout": "rmux-package-v2",}\n'),
                            ("trailing-content", b'{"package_layout": "rmux-package-v2"} x\n')):
            archive = self.archive(f"bad-{label}", skip={"share/rmux/package-layout.tsv"},
                                   metadata=body)
            prefix = self.seeded_prefix(f"bad-{label}-prefix")
            result = self.install(archive, prefix)
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertIn("archive metadata is not a valid JSON object", result.stderr)
            self.assertFalse((prefix / "bin/rmux-daemon").exists())

    def test_metadata_without_the_key_declares_nothing_and_still_installs(self):
        # A document that says nothing about the format is an absence, and the
        # legacy shape is admitted on its own conditions.
        archive = self.archive("no-key", skip={"share/rmux/package-layout.tsv"},
                               metadata=b'{"schema": 1, "rmux_version": "0.10.0"}\n')
        prefix = self.root / "no-key-prefix"
        result = self.install(archive, prefix)
        self.assertEqual(result.returncode, 0, result.stderr)
        for destination in ("bin/rmux", "bin/rmux-daemon", "libexec/rmux/rmux"):
            self.assertTrue((prefix / destination).is_file(), destination)

    def test_the_declared_positives_still_install(self):
        current = self.archive("current-ok")
        prefix = self.root / "current-ok-prefix"
        self.assertEqual(self.install(current, prefix).returncode, 0)
        self.assertTrue((prefix / "share/man/man1/rmux.1").is_file())

        legacy = self.archive("legacy-ok", skip={"share/rmux/package-layout.tsv"},
                              metadata=b'{"package_layout": "rmux-package-v2"}\n')
        prefix = self.root / "legacy-ok-prefix"
        self.assertEqual(self.install(legacy, prefix).returncode, 0)
        for destination in ("bin/rmux", "bin/rmux-daemon", "libexec/rmux/rmux"):
            self.assertTrue((prefix / destination).is_file(), destination)

    def test_an_entry_id_beginning_with_a_digit_is_admitted_like_elsewhere(self):
        # The descriptor and the Windows installer admit it; this one used to
        # refuse it. The three admissions now agree.
        rows = []
        for line in self.manifest().splitlines():
            fields = line.split("\t")
            if fields[0] == "share/man/man1/rmux.1":
                fields[4] = "7manual"
            rows.append("\t".join(fields))
        archive = self.archive("digit-id", manifest="\n".join(rows) + "\n")
        prefix = self.root / "digit-id-prefix"
        result = self.install(archive, prefix)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((prefix / "share/man/man1/rmux.1").is_file())
        self.assertEqual(layout.ENTRY_ID.match("7manual") is not None, True)


if __name__ == "__main__":
    unittest.main()
