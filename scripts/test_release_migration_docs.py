"""Local acceptance check for the 0.11 release notes and destructive migration."""

from pathlib import Path
import os
import re
import shlex
import shutil
import subprocess
import tempfile
import textwrap
import unittest

from toml_reader import loads


ROOT = Path(__file__).resolve().parent.parent
REFERENCE = "76e4cdbdb6afe33ade10533ed8cd9d001f7de83d"
NOTES = ROOT / "docs/release/0.11.0.md"
GUIDE = ROOT / "docs/migration/0.10-to-0.11.md"


class ReleaseMigrationDocs(unittest.TestCase):
    def test_local_source_evidence_and_retained_archive_paths(self):
        # Documentary checks only: neither package execution nor native proof.
        with self.subTest(document="notes evidence"):
            text = NOTES.read_text(encoding="utf-8")
            for target in (
                "Cargo.toml", "crates/rmux-proto/src/envelope.rs",
                "crates/rmux-core/Cargo.toml", "crates/rmux-core/tests/kitty_fallback.rs",
                "docs/benchmarks.md",
            ):
                self.assertIn(
                    "](../../" + target + ")", text
                )
            self.assertNotIn("/blob/v0.11.0/", text)
            self.assertIn("](../../docs/migration/0.10-to-0.11.md)", text)
            prose = " ".join(text.split())
            self.assertIn("campaign artifact outside the qualified source content", prose)
            self.assertIn("qualification run and package manifest", prose)

        guide = GUIDE.read_text(encoding="utf-8")
        # The retained v0.10.0 archive puts the Unix CLI under bin/ and the
        # Windows CLI at its root, next to rmux-daemon.exe. Neither example
        # may redirect saves to a new client or a private helper.
        for assignment, expected in (
            (r"(?m)^RMUX010=(.+)$", "/path/to/retained-0.10.0/bin/rmux"),
            (r"(?m)^\$rmux010 = '([^']+)'$", r"C:\tools\retained-0.10.0\rmux.exe"),
        ):
            with self.subTest(assignment=assignment):
                match = re.search(assignment, guide)
                self.assertIsNotNone(match)
                self.assertEqual(match.group(1), expected)

    def test_sdk_install_commands_use_the_checkout_in_every_readme(self):
        for path in [ROOT / "README.md", *sorted((ROOT / "docs/i18n").glob("README.*.md"))]:
            with self.subTest(document=path.relative_to(ROOT)):
                commands = re.findall(r"^cargo add [^\n]*rmux-sdk[^\n]*$", path.read_text(encoding="utf-8"), re.M)
                self.assertEqual([shlex.split(line) for line in commands],
                                 [["cargo", "add", "--path", "/path/to/rmux/crates/rmux-sdk"]])

    def test_sdk_install_examples_select_the_matching_checkout(self):
        guides = {
            "crates/rmux-sdk/README.md": ("rmux-sdk",),
            "docs/scripting-sdk.md": ("rmux-sdk",),
            "crates/ratatui-rmux/README.md": ("ratatui-rmux", "rmux-sdk"),
        }
        checkout = r"C:\src\review\rmux"
        for guide, crates in guides.items():
            text = (ROOT / guide).read_text(encoding="utf-8")
            block = text.split("```toml\n", 1)[1].split("```", 1)[0]
            for crate in crates:
                with self.subTest(document=guide, crate=crate):
                    example = loads(block.replace("/path/to/rmux", checkout))
                    dependency = example["dependencies"][crate]
                    self.assertIsInstance(dependency, dict, "the candidate must be usable before publication")
                    manifest = (ROOT / "crates" / crate / "Cargo.toml").read_text(encoding="utf-8")
                    self.assertEqual(dependency["version"], loads(manifest)["package"]["version"])
                    self.assertEqual(dependency["path"], f"{checkout}/crates/{crate}")

    def test_uninstall_example_names_the_matching_windows_archive(self):
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        example = next(line for line in readme.splitlines() if " -Uninstall " in line)
        version = loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
        self.assertRegex(example, rf"-PreviousPackageRoot '[^']*\\rmux-{re.escape(version)}-windows-x86_64'")

    @unittest.skipUnless(os.name == "posix", "Unix shell and permission bits required")
    def test_unix_saves_are_private_and_keep_each_inventory(self):
        self.check_backup_example("sh")

    @unittest.skipUnless(shutil.which("pwsh") or shutil.which("powershell"), "PowerShell required")
    def test_powershell_saves_keep_each_inventory(self):
        self.check_backup_example("powershell")

    def check_backup_example(self, language):
        block = GUIDE.read_text(encoding="utf-8").split(f"```{language}\n", 1)[1].split("```", 1)[0]
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            env = dict(os.environ, HOME=directory, USERPROFILE=directory)
            # Emit identifiable content through the real backup commands and redirects.
            if language == "sh":
                block = block.replace("/path/to/retained-0.10.0/bin/rmux", "mock_rmux")
                block = 'umask 022\nmock_rmux() { echo "$MARKER $*"; }\n' + block
                command = ["sh", "-c"]
            else:
                block = block.replace(r"C:\tools\retained-0.10.0\rmux.exe", "Mock-Rmux")
                block = 'function Mock-Rmux { "$env:MARKER $args"; $global:LASTEXITCODE = 0 }\n' + block
                shell = shutil.which("pwsh") or shutil.which("powershell")
                command = [shell, "-NoProfile", "-NonInteractive", "-Command"]

            endpoints = ("first", "second", "first")
            for marker, endpoint in enumerate(endpoints):
                script = block.replace("-L work", "-L " + endpoint)
                result = subprocess.run(command + [script], cwd=home, env=dict(env, MARKER=str(marker)),
                                        capture_output=True, text=True, timeout=15)
                self.assertEqual(result.returncode, 0, result.stderr)

            for filename, action in (("sessions.txt", "list-sessions"), ("windows.txt", "list-windows"),
                                     ("panes.txt", "list-panes"), ("work-0-0.txt", "capture-pane")):
                paths = list(home.rglob(filename))
                with self.subTest(language=language, file=filename):
                    actual = [p.read_text(encoding="utf-8-sig").split()[:4] for p in paths]
                    expected = [[str(i), "-L", endpoint, action] for i, endpoint in enumerate(endpoints)]
                    self.assertCountEqual(actual, expected)
                if language == "sh":
                    for path in paths:
                        with self.subTest(permissions=path):
                            self.assertEqual(path.parent.stat().st_mode & 0o077, 0)
                            self.assertEqual(path.stat().st_mode & 0o077, 0)

    def test_migration_references_the_versioned_notes_not_private_commits(self):
        guide = GUIDE.read_text(encoding="utf-8")
        self.assertIn("../release/0.11.0.md#candidate-reference", guide)
        self.assertNotRegex(guide, r"\b[0-9a-f]{40}\b")
        for version in ("Ratatui 0.29", "Ratatui 0.30", "`ratatui-rmux` 0.10"):
            self.assertIn(version, NOTES.read_text(encoding="utf-8"))

    def test_published_notes_do_not_embed_obsolete_local_candidates(self):
        text = NOTES.read_text(encoding="utf-8")
        self.assertNotIn("Draft", text)
        self.assertNotRegex(text, r"[0-9a-f]{40}")
        self.assertIn("The release publisher appends the exact source SHA", text)
        self.assertIn("source inspection, not native qualification", text)

    def test_release_notes_links_resolve_before_tagging(self):
        documents = sorted((ROOT / "docs/release").glob("*.md"))
        self.assertTrue(documents, "versioned release notes must exist")
        for path in documents:
            text = path.read_text(encoding="utf-8")
            inline = re.findall(r"!?\[[^\]\n]*\]\(\s*(<[^>\n]*>|[^\s)]*)", text)
            references = re.findall(r"^ {0,3}\[[^\]\n]+\]:\s*(<[^>\n]*>|\S+)",
                                    text, re.M)
            for destination in inline + references:
                link = destination.removeprefix("<").removesuffix(">")
                with self.subTest(document=path.name, link=link):
                    if link.startswith("../../") and link in inline:
                        target = (path.parent / link.partition("#")[0]).resolve()
                        self.assertTrue(target.is_relative_to(ROOT) and target.is_file(), link)
                        continue
                    self.assertTrue(link.startswith("#") or
                                    re.match(r"[A-Za-z][A-Za-z0-9+.-]*:", link),
                                    f"release link has no scheme: {link}")
                    repository = "https://github.com/Helvesec/rmux/blob/"
                    if link.startswith(repository):
                        version = repository + "v" + path.stem + "/"
                        self.assertTrue(link.startswith(version),
                                        f"release link must use its version tag: {link}")
                        target = ROOT / link.removeprefix(version).split("#", 1)[0]
                        self.assertTrue(target.is_file(), f"broken release link: {link}")

    def test_publication_reads_versioned_notes_and_binds_the_candidate(self):
        workflow = (ROOT / ".github/workflows/release-github-publish.yml").read_text()
        self.assertIn("ref: ${{ inputs.expected_source_sha }}", workflow)
        publish = workflow.split("      - id: publish\n", 1)[1]
        self.assertIn("NOTES_SOURCE_SHA: ${{ inputs.expected_source_sha }}", publish)
        self.assertIn('--notes-file "$root/release-notes.md"', publish)
        # Execute only local preparation, stopping before the publisher command.
        prepare = textwrap.dedent(publish.split("        run: |\n", 1)[1]
                                  .split("          python3 scripts/release/", 1)[0])
        notes = NOTES.read_bytes()
        for release, run, sha, document, success in (
            ("v0.11.0", "4242", "a" * 40, True, True),
            ("v0.11.0-rc.1", "5252", "b" * 40, True, True),
            ("v0.12.0", "4242", "a" * 40, True, False),
            ("v0.11.0", "4242", "a" * 40, False, False),
        ):
            with self.subTest(release=release, document=document):
                with tempfile.TemporaryDirectory() as directory:
                    fixture = Path(directory)
                    root = fixture / "rmux-publication"
                    for folder, filename in (
                        ("authorization", "promotion-authorization-predicate.json"),
                        ("authorization", "SHA256SUMS.sigstore.json"),
                        ("envelope", "promotion-authorization-envelope.json"),
                    ):
                        target = root / folder / filename
                        target.parent.mkdir(parents=True, exist_ok=True)
                        target.write_text("{}")
                    (root / "release-assets").mkdir()
                    if document:
                        target = fixture / "docs/release/0.11.0.md"
                        target.parent.mkdir(parents=True)
                        target.write_bytes(notes)
                    values = {"inputs.release_ref": release,
                              "inputs.candidate_run_id": run,
                              "inputs.expected_source_sha": sha,
                              "github.server_url": "https://github.com",
                              "github.repository": "Helvesec/rmux"}
                    script = prepare
                    for key, value in values.items():
                        script = script.replace("${{ " + key + " }}", value)
                    self.assertNotIn("${{", script)
                    result = subprocess.run(
                        ["bash", "-c", script], cwd=fixture,
                        env={**os.environ, "RUNNER_TEMP": directory, "NOTES_SOURCE_SHA": sha},
                        capture_output=True, text=True, timeout=10,
                    )
                    if not success:
                        self.assertNotEqual(result.returncode, 0)
                        continue
                    self.assertEqual(result.returncode, 0, result.stderr)
                    published = (root / "release-notes.md").read_bytes()
                    prefix = f"](https://github.com/Helvesec/rmux/blob/{sha}/".encode()
                    rendered = notes.replace(b"](../../", prefix)
                    self.assertTrue(published.startswith(rendered))
                    self.assertIn(prefix + b"docs/migration/0.10-to-0.11.md)", published)
                    self.assertNotIn(b"](../../", published)
                    footer = published[len(rendered):].decode()
                    for value in (release, sha,
                                  f"https://github.com/Helvesec/rmux/actions/runs/{run}"):
                        self.assertIn(value, footer)
                    self.assertNotIn(REFERENCE, footer)

    def test_release_and_migration_explain_the_wire_cut_and_recovery_limits(self):
        for path in (NOTES, GUIDE):
            self.assertTrue(path.is_file(), f"missing release document: {path}")
        for path in (NOTES, GUIDE):
            with self.subTest(document=path.relative_to(ROOT)):
                text = path.read_text(encoding="utf-8")
                prose = " ".join(text.split())
                for required in (
                    "0.10.0 (wire 8)", "0.11.0 (wire 9)",
                    "restart the daemon", "Open sessions are lost",
                    "running processes", "not a process checkpoint",
                    "Windows 10 is not qualified",
                ):
                    self.assertIn(required, prose)
                links = re.findall(r"\[[^\]]+\]\(([^)]+)\)", text)
                self.assertTrue(links, "release documents must link their evidence")
                for link in links:
                    if re.match(r"[a-z]+://", link):
                        continue
                    target, _, anchor = link.partition("#")
                    destination = (path.parent / target).resolve() if target else path
                    self.assertTrue(destination.is_relative_to(ROOT))
                    self.assertTrue(destination.is_file(), f"broken link: {link}")
                    if anchor and destination.suffix == ".md":
                        headings = re.findall(r"^#+ (.+)$", destination.read_text(), re.M)
                        slugs = {re.sub(r"[^\w -]", "", h.lower()).replace(" ", "-")
                                 for h in headings}
                        self.assertIn(anchor, slugs, f"broken anchor: {link}")

        notes = " ".join(NOTES.read_text(encoding="utf-8").split())
        for required in (
            "exact source SHA", "qualification run", "No performance claim",
            "ad hoc", "not notarized", "unsigned", "Authenticode",
            "Kitty Keyboard", "disabled by default", "floating panes",
            "experimental", "loopback", "IME", "SIXEL", "SemVer 1.0",
            "WinGet", "Chocolatey", "homebrew-core", "crates.io",
            "#92", "#213", "#211", "#214", "#218", "#219",
            "| Windows 10 |", "PREUVES-WINDOWS.md", "variant B",
            "#259", "#261", "#254",
            "## Deferred scope", "## Candidate reference",
        ):
            self.assertIn(required, notes)
        for issue in (92, 211, 213, 214, 218, 219):
            self.assertNotIn(f"| #{issue} |", notes)
        guide = GUIDE.read_text(encoding="utf-8")
        positions = [guide.index(section) for section in (
            "## 1. Save", "## 2. Stop", "## 3. Install", "## 4. Recreate",
            "## Roll back",
        )]
        self.assertEqual(positions, sorted(positions))
        for required in (
            "capture-pane -p -S -", "list-sessions", "list-windows", "list-panes",
            "kill-server", "new-session", "-L", "-S", "0.10.0",
            "configuration", "resurrect", "continuum", "not guaranteed",
            "“uses incompatible protocol 8”",
            "../release/0.11.0.md#candidate-reference",
        ):
            self.assertIn(required, guide)


if __name__ == "__main__":
    unittest.main()
