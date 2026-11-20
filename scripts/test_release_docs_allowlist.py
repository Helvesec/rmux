"""Release documentation must reject unqualified claims at the existing gate."""
import hashlib
import json
import os
import runpy
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


class ReleaseDocsAllowlist(unittest.TestCase):
    def source_section(self, name):
        text = (ROOT / "scripts" / name).read_text()
        function = re.search(r"^run_step\(\) \{\n.*?^\}", text, re.M | re.S)
        condition = (r'if \[ "\$skip_source_gates" -eq 0 \]; then' if name == "gate-unix-fast.sh"
                     else r"if section_enabled static; then")
        sections = re.findall(r"^" + condition + r"\n.*?^fi$", text, re.M | re.S)
        sections = [section for section in sections if "scripts/check-worktree-hygiene.sh" in section]
        self.assertIsNotNone(function, name)
        self.assertEqual(len(sections), 1, name)
        return function[0] + "\nskip_source_gates=0\nevidence_mode=review\n" + \
            "section_enabled() { return 0; }\n" + sections[0]

    def source_gate(self, repo, name):
        return subprocess.run(["bash", "-euc", self.source_section(name)], cwd=repo,
                              env=dict(os.environ, RMUX_PYTHON=sys.executable),
                              capture_output=True, text=True)

    def test_frozen_analysis_and_neighbor_document_invocation(self):
        contract = runpy.run_path(str(ROOT / "tests/hygiene_neutrality_spec.py"))
        for name, expected in contract["FROZEN_GATES"].items():
            with self.subTest(gate=name):
                self.assertEqual(hashlib.sha256((ROOT / "scripts" / name).read_bytes()).hexdigest(),
                                 expected)
        for name in ("gate-unix-fast.sh", "release-review-gate.sh"):
            with self.subTest(entry=name):
                self.assertIn("scripts/release_docs_allowlist.py", self.source_section(name))

    def test_neighbor_entries_execute_closed_document_checks(self):
        self.check_neighbor_entries(checkout_digests=False)

    def test_neighbor_entries_execute_closed_document_checks_with_checkout_digests(self):
        self.check_neighbor_entries(checkout_digests=True)

    def check_neighbor_entries(self, *, checkout_digests):
        audit = ROOT / ".rmux-audit" / "s3-113"
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="docs-neighbors-", dir=audit) as directory:
            repo = Path(directory)
            subprocess.run(["git", "clone", "--shared", "--quiet", "--no-checkout",
                            str(ROOT), str(repo)], check=True)
            policy = json.loads((ROOT / ".github/release/docs-allowlist.json").read_text())
            if checkout_digests:
                for guide in policy["external_guides"].values():
                    guide["source_sha256"] = hashlib.sha256(
                        (ROOT / guide["source_path"]).read_bytes()).hexdigest()
            paths = ["README.md", "docs/README.md", "docs/benchmarks.md",
                     "scripts/check-worktree-hygiene.sh", "scripts/release_docs_allowlist.py",
                     "scripts/source-inventory/declarations.json", ".github/release/docs-allowlist.json"]
            sources = [guide["source_path"] for guide in policy["external_guides"].values()]
            for path in paths + policy["issue_forms"] + sources:
                target = repo / path
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(ROOT / path, target)
            (repo / ".github/release/docs-allowlist.json").write_text(json.dumps(policy))
            for name in ("no-network-in-runtime.sh", "check-platform-neutrality.sh",
                         "no-debug-assert-side-effects.sh"):
                target = repo / "scripts" / name
                target.write_text("#!/bin/sh\nexit 0\n")
                target.chmod(0o755)
            subprocess.run(["git", "-C", str(repo), "add", "."], check=True)
            cases = [("docs/unlisted.md", "unlisted\n", "document absent from"),
                     (policy["issue_forms"][0], None, "allowlisted issue form missing")]
            release_url = "https://github.com/Helvesec/rmux/releases/tag/v0.11.0"
            for path in ("README.md", "docs/README.md"):
                cases.append((path, (ROOT / path).read_text() + f"\n[Release]({release_url})\n",
                              "published receipt evidence"))
            for field, value in (("status", "unknown"), ("url", release_url)):
                changed = json.loads(json.dumps(policy))
                changed["release"][field] = value
                cases.append((".github/release/docs-allowlist.json", json.dumps(changed),
                              "release"))
            for ref, diagnostic in (("main", "immutable source ref"),
                                    ("f" * 40, "returned non-zero exit status")):
                changed = json.loads(json.dumps(policy))
                changed["external_guides"]["unix_installation"]["source_ref"] = ref
                cases.append((".github/release/docs-allowlist.json", json.dumps(changed), diagnostic))
            for name, guide in policy["external_guides"].items():
                if checkout_digests:
                    for digest, diagnostic in ((None, "needs an exact source digest"),
                                               ("", "needs an exact source digest"),
                                               ("0" * 64, "source snapshot differs")):
                        changed = json.loads(json.dumps(policy))
                        changed["external_guides"][name]["source_sha256"] = digest
                        cases.append((".github/release/docs-allowlist.json", json.dumps(changed),
                                      f"{name} {diagnostic}"))
                    cases.append((guide["source_path"],
                                  (ROOT / guide["source_path"]).read_text() + "\n# changed\n",
                                  f"{name} source snapshot differs"))
                    cases.append((guide["source_path"], None, f"{name} source snapshot differs"))
                changed = json.loads(json.dumps(policy))
                del changed["external_guides"][name]["source_ref"]
                cases.append((".github/release/docs-allowlist.json", json.dumps(changed), "source_ref"))
            for entry in ("gate-unix-fast.sh", "release-review-gate.sh"):
                with self.subTest(entry=entry):
                    green = self.source_gate(repo, entry)
                    self.assertEqual(green.returncode, 0, green.stdout + green.stderr)
                    self.assertIn("release-docs-allowlist=ok", green.stdout)
                    if not checkout_digests:
                        for name, guide in policy["external_guides"].items():
                            target = repo / guide["source_path"]
                            original = target.read_bytes() if target.exists() else None
                            for state in ("changed", "missing"):
                                with self.subTest(guide=name, current_source=state):
                                    if state == "changed":
                                        target.write_bytes((ROOT / guide["source_path"]).read_bytes()
                                                           + b"\n# current checkout only\n")
                                    else:
                                        target.unlink()
                                    accepted = self.source_gate(repo, entry)
                                    self.assertEqual(accepted.returncode, 0,
                                                     accepted.stdout + accepted.stderr)
                            if original is not None:
                                target.write_bytes(original)
                    for path, content, diagnostic in cases:
                        with self.subTest(path=path, diagnostic=diagnostic):
                            target = repo / path
                            original = target.read_bytes() if target.exists() else None
                            if content is None:
                                subprocess.run(["git", "-C", str(repo), "rm", "-q", "--cached", path], check=True)
                            else:
                                target.write_text(content)
                                subprocess.run(["git", "-C", str(repo), "add", path], check=True)
                            red = self.source_gate(repo, entry)
                            self.assertNotEqual(red.returncode, 0, red.stdout + red.stderr)
                            self.assertIn(diagnostic, red.stderr)
                            if original is None:
                                subprocess.run(["git", "-C", str(repo), "rm", "-q", "--cached", path], check=True)
                                target.unlink()
                            else:
                                target.write_bytes(original)
                                subprocess.run(["git", "-C", str(repo), "add", path], check=True)

    def test_document_gate_checkouts_recover_frozen_sources(self):
        policy = json.loads((ROOT / ".github/release/docs-allowlist.json").read_text())
        source = policy["external_guides"]["unix_installation"]["source_ref"]
        checkouts = (("ci.yml", "linux-source-gates"),
                     ("ci.yml", "release-review-sections"),
                     ("release-candidate-delta.yml", "release-review-sections"),
                     ("release.yml", "source-gates"))
        audit = ROOT / ".rmux-audit" / "s3-103"
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="shallow-", dir=audit) as directory:
            repo = Path(directory) / "repo"
            subprocess.run(["git", "clone", "--quiet", "--depth=1", "--no-local",
                            "--single-branch", "--no-tags", ROOT.as_uri(), str(repo)], check=True)

            def git(*args):
                return subprocess.run(["git", "-C", str(repo), *args],
                                      capture_output=True, text=True)

            def gate():
                return subprocess.run([sys.executable, "scripts/release_docs_allowlist.py"],
                                      cwd=repo, capture_output=True, text=True)

            head = git("rev-parse", "HEAD").stdout.strip()
            self.assertEqual(git("rev-parse", "--is-shallow-repository").stdout.strip(), "true")
            self.assertNotEqual(git("cat-file", "-t", source).returncode, 0)
            red = gate()
            self.assertEqual(red.returncode, 1, red.stdout + red.stderr)
            self.assertIn(source, red.stderr)

            depths = {}
            for workflow, job in checkouts:
                text = (ROOT / ".github/workflows" / workflow).read_text()
                section = re.search(r"^  " + re.escape(job) +
                                    r":\n(.*?)(?=^  [\w-]+:|\Z)", text, re.M | re.S)
                self.assertIsNotNone(section, (workflow, job))
                checkout = re.search(r"^      - uses: actions/checkout@[^\n]+\n"
                                     r"(.*?)(?=^      - |\Z)", section[1], re.M | re.S)
                self.assertIsNotNone(checkout, (workflow, job))
                depth = re.search(r"^          fetch-depth: (\d+)\s*$", checkout[1], re.M)
                depths[f"{workflow}/{job}"] = int(depth[1]) if depth else 1
            self.assertEqual(depths, dict.fromkeys(depths, 0), red.stderr)

            # actions/checkout fetch-depth: 0 recovers history even in an existing shallow clone.
            recovered = git("fetch", "--quiet", "--unshallow", "origin")
            self.assertEqual(recovered.returncode, 0, recovered.stderr)
            self.assertEqual(git("rev-parse", "HEAD").stdout.strip(), head)
            self.assertEqual(git("cat-file", "-t", source).stdout.strip(), "commit")
            green = gate()
            self.assertEqual(green.returncode, 0, green.stdout + green.stderr)
            self.assertIn("release-docs-allowlist=ok", green.stdout)

    def test_existing_hygiene_gate_rejects_unqualified_release_claims(self):
        audit = ROOT / ".rmux-audit" / "s3-113"
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="docs-entry-", dir=audit) as directory:
            repo = Path(directory)
            (repo / "scripts").mkdir()
            shutil.copy2(ROOT / "scripts/check-worktree-hygiene.sh", repo / "scripts")
            helper = ROOT / "scripts/release_docs_allowlist.py"
            if helper.exists():
                shutil.copy2(helper, repo / "scripts")
            for name in ("no-network-in-runtime.sh", "check-platform-neutrality.sh",
                         "no-debug-assert-side-effects.sh"):
                stub = repo / "scripts" / name
                stub.write_text("#!/bin/sh\nexit 0\n")
                stub.chmod(0o755)
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            for claim in ("Windows 10 is fully qualified.", "RMUX is 10x faster than tmux."):
                with self.subTest(claim=claim):
                    (repo / "README.md").write_text(claim + "\n", encoding="utf-8")
                    subprocess.run(["git", "-C", str(repo), "add", "README.md", "scripts"], check=True)
                    for entry in ("gate-unix-fast.sh", "release-review-gate.sh"):
                        with self.subTest(entry=entry):
                            result = self.source_gate(repo, entry)
                            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                            self.assertIn("README.md", result.stderr)
                            self.assertIn("unqualified release claim", result.stderr)

    def test_wrapped_markdown_claims_rejected_at_ordinary_entries(self):
        audit = ROOT / ".rmux-audit" / "s3-251"
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="wrapped-claims-", dir=audit) as directory:
            repo = Path(directory)
            subprocess.run(["git", "clone", "--shared", "--quiet", str(ROOT), str(repo)],
                           check=True)
            readme = repo / "README.md"
            original = readme.read_text()
            for entry in ("gate-unix-fast.sh", "release-review-gate.sh"):
                with self.subTest(entry=entry):
                    green = self.source_gate(repo, entry)
                    self.assertEqual(green.returncode, 0, green.stdout + green.stderr)
                    for claim in ("Windows 10 is fully\nqualified.",
                                  "RMUX is faster\nthan tmux."):
                        with self.subTest(claim=claim):
                            readme.write_text(original + "\n\n" + claim + "\n")
                            try:
                                red = self.source_gate(repo, entry)
                                self.assertNotEqual(red.returncode, 0, red.stdout + red.stderr)
                                self.assertIn("README.md: unqualified release claim", red.stderr)
                            finally:
                                readme.write_text(original)
                    self.assertEqual(self.source_gate(repo, entry).returncode, 0)

    def test_second_windows_claim_survives_prior_bullet_negation(self):
        # Both qualification paths use only interpreted source checks in this
        # synthetic fixture; no build or native qualification is performed.
        audit = ROOT / ".rmux-audit" / "s3-252"
        audit.mkdir(parents=True, exist_ok=True)
        bridge = '''import json, os, runpy, sys
from pathlib import Path
from unittest.mock import patch
root = Path.cwd()
sys.path.insert(0, str(root / "scripts"))
import candidate_identity, qualification_campaign as campaign
real_output = campaign.subprocess.check_output
real_campaign = campaign.run_campaign
def metadata(command, **kwargs):
    if command[:2] == ["cargo", "metadata"]:
        return json.dumps({"workspace_members": ["fixture"], "packages": [
            {"id": "fixture", "name": "fixture", "features": {}}]})
    return real_output(command, **kwargs)
def source_checks(root, manifest, evidence, **kwargs):
    selected = dict(manifest,
        qualification_id="Q0",
        scenarios=[s for s in manifest["scenarios"] if s["command"] in (
            ["bash", "scripts/check-worktree-hygiene.sh"],
            ["python", "scripts/release_docs_allowlist.py"])],
        cells=[dict(c, id=c["id"] + "-" + evidence.name)
               for c in manifest["cells"] if c["native_os"] == campaign.native_os()])
    result = real_campaign(root, selected, evidence, synthetic=True)
    for receipt in json.loads((evidence / "receipts.json").read_text()):
        index = receipt["scenario_id"]
        print("source-check=" + index)
    for stream in evidence.glob("*.stdout"):
        print(stream.read_text(), end="")
    for stream in evidence.glob("*.stderr"):
        print(stream.read_text(), end="", file=sys.stderr)
    return result
script = sys.argv.pop(1)
sys.path.insert(0, str(Path(script).resolve().parent))
with patch.object(candidate_identity, "source_identity"), \\
     patch.object(campaign.subprocess, "check_output", side_effect=metadata), \\
     patch.object(campaign, "run_campaign", side_effect=source_checks):
    runpy.run_path(script, run_name="__main__")
'''
        entries = ("gate-unix-fast.sh", "release-review-gate.sh",
                   "--qualification-dir", "--run-qualification")
        for entry in entries:
            with self.subTest(entry=entry), tempfile.TemporaryDirectory(dir=audit) as directory:
                repo = Path(directory)
                subprocess.run(["git", "clone", "--shared", "--quiet", str(ROOT), str(repo)],
                               check=True)
                bin_dir = repo / ".rmux-audit/bin"
                bin_dir.mkdir(parents=True)
                interpreter = bin_dir / "python3"
                interpreter.write_text("#!" + sys.executable + "\n" + bridge)
                interpreter.chmod(0o755)
                env = dict(os.environ, PATH=str(bin_dir) + os.pathsep + os.environ["PATH"])

                def gate(name):
                    if entry in ("gate-unix-fast.sh", "release-review-gate.sh"):
                        return self.source_gate(repo, entry)
                    evidence = repo / ".rmux-audit" / name
                    command = (["bash", "scripts/release-review-gate.sh", entry, str(evidence)]
                               if entry == "--qualification-dir" else
                               [str(interpreter), "scripts/validation/contracts.py", "--root", str(repo),
                                entry, str(evidence)])
                    return subprocess.run(command, cwd=repo, env=env, capture_output=True, text=True)

                readme = repo / "README.md"
                original = readme.read_text()
                for name, suffix in (("control", ""),
                                     ("negation", "- Windows 10 is not a release target")):
                    with self.subTest(control=name):
                        readme.write_text(original + "\n\n" + suffix + "\n")
                        green = gate(name)
                        self.assertEqual(green.returncode, 0, green.stdout + green.stderr)
                        self.assertIn("release-docs-allowlist=ok", green.stdout)
                claims = ("- Windows 10 is not a release target\n"
                          "- Windows 10 is fully qualified.",
                          "- wInDoWs 10 is not a release target\n"
                          "- WINDOWS 10 is fully qualified.",
                          "Windows 10 is fully\nqualified.", "RMUX is faster\nthan tmux.")
                try:
                    for index, claim in enumerate(claims):
                        with self.subTest(claim=claim):
                            readme.write_text(original + "\n\n" + claim + "\n")
                            red = gate(f"claim-{index}")
                            self.assertNotEqual(red.returncode, 0, red.stdout + red.stderr)
                            self.assertIn("README.md: unqualified release claim", red.stderr)
                finally:
                    readme.write_text(original)
                restored = gate("restored")
                self.assertEqual(restored.returncode, 0, restored.stdout + restored.stderr)

    def test_qualification_entries_reject_unqualified_readme_claim(self):
        # Exercise both public dispatch paths and the real recipe/runner. Only
        # the source checks run in this synthetic fixture: no Cargo/native credit.
        audit = ROOT / ".rmux-audit" / "s3-114"
        audit.mkdir(parents=True, exist_ok=True)
        bridge = '''import json, os, runpy, sys
from pathlib import Path
from unittest.mock import patch
root = Path.cwd()
sys.path.insert(0, str(root / "scripts"))
import candidate_identity, qualification_campaign as campaign
real_output = campaign.subprocess.check_output
real_campaign = campaign.run_campaign
def metadata(command, **kwargs):
    if command[:2] == ["cargo", "metadata"]:
        return json.dumps({"workspace_members": ["fixture"], "packages": [
            {"id": "fixture", "name": "fixture", "features": {}}]})
    return real_output(command, **kwargs)
def source_checks(root, manifest, evidence, **kwargs):
    selected = dict(manifest,
        qualification_id="Q1" if evidence.name == "claim" else "Q0",
        scenarios=[s for s in manifest["scenarios"] if s["command"] in (
            ["bash", "scripts/check-worktree-hygiene.sh"],
            ["python", "scripts/release_docs_allowlist.py"])],
        cells=[c for c in manifest["cells"] if c["native_os"] == campaign.native_os()])
    result = real_campaign(root, selected, evidence, synthetic=True)
    for receipt in json.loads((evidence / "receipts.json").read_text()):
        index = receipt["scenario_id"]
        print("source-check=" + index)
    for stream in evidence.glob("*.stdout"):
        print(stream.read_text(), end="")
    for stream in evidence.glob("*.stderr"):
        print(stream.read_text(), end="", file=sys.stderr)
    return result
script = sys.argv.pop(1)
sys.path.insert(0, str(Path(script).resolve().parent))
with patch.object(candidate_identity, "source_identity"), \\
     patch.object(campaign.subprocess, "check_output", side_effect=metadata), \\
     patch.object(campaign, "run_campaign", side_effect=source_checks):
    runpy.run_path(script, run_name="__main__")
'''
        for entry in ("--qualification-dir", "--run-qualification"):
            with self.subTest(entry=entry), tempfile.TemporaryDirectory(dir=audit) as directory:
                repo = Path(directory)
                subprocess.run(["git", "clone", "--shared", "--quiet", "--no-checkout",
                                str(ROOT), str(repo)], check=True)
                subprocess.run(["git", "-C", str(repo), "checkout", "HEAD", "--", "scripts",
                                "README.md", "docs/README.md", "docs/benchmarks.md", ".github"],
                               check=True)
                shutil.copy2(ROOT / "scripts/qualification_recipe.py", repo / "scripts")
                bin_dir = repo / ".rmux-audit/bin"
                bin_dir.mkdir(parents=True)
                interpreter = bin_dir / "python3"
                interpreter.write_text("#!" + sys.executable + "\n" + bridge)
                interpreter.chmod(0o755)
                env = dict(os.environ, PATH=str(bin_dir) + os.pathsep + os.environ["PATH"])

                def gate(name):
                    evidence = repo / ".rmux-audit" / name
                    command = (["bash", "scripts/release-review-gate.sh", entry, str(evidence)]
                               if entry == "--qualification-dir" else
                               [str(interpreter), "scripts/validation/contracts.py", "--root", str(repo),
                                entry, str(evidence)])
                    return subprocess.run(command, cwd=repo, env=env, capture_output=True, text=True)

                green = gate("control")
                self.assertEqual(green.returncode, 0, green.stdout + green.stderr)
                self.assertIn("worktree-hygiene=ok", green.stdout)
                with (repo / "README.md").open("a") as readme:
                    readme.write("\nRMUX is 10x faster than tmux.\n")
                red = gate("claim")
                self.assertNotEqual(red.returncode, 0, red.stdout + red.stderr)
                self.assertIn("README.md: unqualified release claim", red.stderr)
                self.assertIn("RMUX is 10x faster than tmux", red.stderr)


if __name__ == "__main__":
    unittest.main()
