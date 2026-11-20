"""Local acceptance check for the 0.11 release notes and destructive migration."""

from pathlib import Path
import os
import re
import subprocess
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parent.parent
REFERENCE = "76e4cdbdb6afe33ade10533ed8cd9d001f7de83d"
LOCAL_CANDIDATE = "627c2bf9b78f07289b5adaa07e9d739f4f9c245d"
NOTES = ROOT / "docs/release/0.11.0.md"
GUIDE = ROOT / "docs/migration/0.10-to-0.11.md"


class ReleaseMigrationDocs(unittest.TestCase):
    def test_local_source_evidence_and_retained_archive_paths(self):
        # Documentary checks only: neither package execution nor native proof.
        reviewed_source = "247733ef5cd58bc4547b6379eecd61bf88547743"
        for path in (NOTES, GUIDE):
            with self.subTest(document=path.relative_to(ROOT)):
                text = " ".join(path.read_text(encoding="utf-8").split())
                self.assertIn(f"Reviewed source snapshot: `{reviewed_source}`", text)
                self.assertIn(
                    f"https://github.com/Helvesec/rmux/commit/{reviewed_source}", text
                )
                self.assertIn("source inspection, not native qualification", text)

        with self.subTest(document="notes evidence"):
            text = NOTES.read_text(encoding="utf-8")
            for target in (
                "Cargo.toml", "crates/rmux-proto/src/envelope.rs",
                "crates/rmux-core/Cargo.toml", "crates/rmux-core/tests/kitty_fallback.rs",
                "docs/benchmarks.md",
            ):
                self.assertIn(
                    "https://github.com/Helvesec/rmux/blob/v0.11.0/" + target, text
                )
            prose = " ".join(text.split())
            self.assertIn("campaign artifact outside the qualified source content", prose)
            self.assertIn("Final release content and the 1.0 contract remain pending", prose)

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

    def test_documents_identify_the_current_local_candidate_and_version(self):
        for path in (NOTES, GUIDE):
            with self.subTest(document=path.relative_to(ROOT)):
                text = path.read_text(encoding="utf-8")
                reference = re.search(
                    r"Draft — (?:documentary )?source reference:\s*`([0-9a-f]{40})`",
                    text,
                )
                self.assertIsNotNone(reference)
                self.assertEqual(reference.group(1), LOCAL_CANDIDATE)
                self.assertIn(
                    f"https://github.com/Helvesec/rmux/commit/{LOCAL_CANDIDATE}",
                    text,
                )
                self.assertIn("local source already declares 0.11.0", " ".join(text.split()))

    def test_release_notes_links_are_absolute_or_internal_anchors(self):
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
                        env={**os.environ, "RUNNER_TEMP": directory},
                        capture_output=True, text=True, timeout=10,
                    )
                    if not success:
                        self.assertNotEqual(result.returncode, 0)
                        continue
                    self.assertEqual(result.returncode, 0, result.stderr)
                    published = (root / "release-notes.md").read_bytes()
                    self.assertTrue(published.startswith(notes),
                                    "publisher discarded the versioned release notes")
                    footer = published[len(notes):].decode()
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
                    "Windows 10 is not qualified", REFERENCE,
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
            "Draft", "not the final release SHA", "No performance claim",
            "ad hoc", "not notarized", "unsigned", "Authenticode",
            "Kitty Keyboard", "disabled by default", "floating panes",
            "experimental", "loopback", "IME", "SIXEL", "SemVer 1.0",
            "WinGet", "Chocolatey", "homebrew-core", "crates.io",
            "#92", "#213", "#211", "#214", "#218B", "#219",
            "## Deferred scope", "## Candidate reference",
        ):
            self.assertIn(required, notes)
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
            "../release/0.11.0.md#candidate-reference",
        ):
            self.assertIn(required, guide)


if __name__ == "__main__":
    unittest.main()
