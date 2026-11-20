#!/usr/bin/env python3
"""Check release prose and exact document/form allowlists using Git and stdlib."""
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
POLICY = ".github/release/docs-allowlist.json"
RELEASE_REF = "v0.11.0"
RELEASE_URL = f"https://github.com/Helvesec/rmux/releases/tag/{RELEASE_REF}"


def git(*args):
    return subprocess.check_output(["git", "-C", str(ROOT), *args])


def require_object(ref, kind, label):
    if not isinstance(ref, str) or not re.fullmatch(r"[0-9a-f]{40}", ref):
        raise ValueError(f"{POLICY}: {label} needs an immutable {kind} ref")
    if git("cat-file", "-t", ref).strip().decode() != kind:
        raise ValueError(f"{POLICY}: {label} must name an existing {kind}")


def publication_url(policy):
    """Bind prose to archived receipt identity; not a live/attestation verifier."""
    release = policy["release"]
    fields = {"status", "url", "source_ref", "receipt_ref", "receipt_path"}
    if not isinstance(release, dict) or set(release) != fields:
        raise ValueError(f"{POLICY}: release phase fields are incomplete or unknown")
    if release["status"] == "pending":
        if any(release[field] is not None for field in fields - {"status"}):
            raise ValueError(f"{POLICY}: pending release URL and evidence must be null")
        return None
    if release["status"] != "published" or release["url"] != RELEASE_URL:
        raise ValueError(f"{POLICY}: release phase or versioned URL is invalid")
    source, ref, path = (release[field] for field in ("source_ref", "receipt_ref", "receipt_path"))
    require_object(source, "commit", "release source")
    require_object(ref, "commit", "publication receipt")
    if (not isinstance(path, str) or not path or path.startswith("/")
            or any(part in ("", ".", "..") for part in path.split("/"))):
        raise ValueError(f"{POLICY}: publication receipt needs a repository file path")
    binding = f"{ref}:{path}"
    if git("cat-file", "-t", binding).strip() != b"blob":
        raise ValueError(f"{POLICY}: publication receipt must bind a file at an exact commit")
    receipt = json.loads(git("show", binding))
    if not isinstance(receipt, dict):
        raise ValueError(f"{POLICY}: publication receipt must be an object")
    schema = json.loads((ROOT / ".github/release/schemas/"
                         "publication-receipt-predicate.schema.json").read_text(encoding="utf-8"))
    missing = set(schema["required"]) - receipt.keys()
    extra = receipt.keys() - schema["properties"].keys()
    if missing or extra:
        raise ValueError(f"{POLICY}: publication receipt predicate keys differ: "
                         f"missing={sorted(missing)}, extra={sorted(extra)}")
    identity = {"schema_version": 1,
                "predicate_type": "https://rmux.io/attestations/release-publication-receipt/v1",
                "status": "downstream-authorized", "downstream_authority": True,
                "repository_id": 1239918790, "source_git_sha": source}
    if any(type(receipt.get(key)) is not type(value) or receipt[key] != value
           for key, value in identity.items()):
        raise ValueError(f"{POLICY}: publication receipt identity or authority differs")
    published, candidate, producer = (receipt.get(key) for key in ("release", "candidate", "receipt"))
    if not all(isinstance(value, dict) for value in (published, candidate, producer)):
        raise ValueError(f"{POLICY}: publication receipt source evidence is incomplete")
    if (published.get("ref") != RELEASE_REF or published.get("kind") != "stable"
            or published.get("immutable") is not True
            or candidate.get("source_git_sha") != source
            or producer.get("workflow_id") != 316435347
            or producer.get("workflow_path") != ".github/workflows/release-receipt.yml"
            or type(producer.get("run_attempt")) is not int or producer["run_attempt"] != 1
            or any(type(value) is not int or value <= 0
                   for value in (published.get("id"), producer.get("run_id")))):
        raise ValueError(f"{POLICY}: publication receipt release or producer differs")
    assets = receipt.get("assets")
    if (not isinstance(assets, list) or not assets
            or type(receipt.get("asset_count")) is not int or receipt["asset_count"] != len(assets)):
        raise ValueError(f"{POLICY}: publication receipt assets are incomplete")
    tag = published.get("tag_object_sha")
    require_object(tag, "tag", "release tag")
    tag_header = git("cat-file", "tag", tag).split(b"\n\n", 1)[0].splitlines()
    if tag_header[:3] != [f"object {source}".encode(), b"type commit", f"tag {RELEASE_REF}".encode()]:
        raise ValueError(f"{POLICY}: publication receipt tag and source differ")
    return release["url"]


def claim_errors(text, path):
    errors = []
    text = re.sub(r"<[^>]*>", " ", text)
    # A soft Markdown line wrap does not end the rendered claim.
    text = re.sub(r"(?<=\S)[ \t]*\n[ \t]*(?=\S)", " ", text)
    for line in re.split(r"[.!?\n]", text):
        plain = line
        windows = re.finditer(r"Windows\s*10(?:(?!Windows\s*10)[^.!?\n]){0,100}?"
                             r"\b(qualified|supported|verified|tested)\b", plain, re.I)
        qualified = any(not re.search(r"\b(not|unqualified|unverified|unsupported)\b",
                                     match.group(), re.I) for match in windows)
        speed = re.search(r"\b(\d+(?:\.\d+)?x\s+faster|faster\s+than\s+tmux|"
                          r"fastest|blazing\s+fast|lightning\s+fast|low\s+latency|"
                          r"(?:engineered|optimized)[^.!?\n]{0,30}for\s+speed)\b", plain, re.I)
        if qualified or speed:
            errors.append(f"{path}: unqualified release claim: {plain.strip()}")
    return errors


def check():
    readme = (ROOT / "README.md").read_text(encoding="utf-8")
    errors = claim_errors(readme, "README.md")
    policy = json.loads((ROOT / POLICY).read_text(encoding="utf-8"))
    if policy["version"] != 1:
        raise ValueError(f"{POLICY}: unsupported allowlist version")
    declarations = json.loads((ROOT / "scripts/source-inventory/declarations.json").read_text(encoding="utf-8"))
    allowed = {p for paths in declarations["files"].values() for p in paths}
    tracked = set(git("ls-files", "-z").decode().rstrip("\0").split("\0"))
    forms = set(policy["issue_forms"])
    for path in sorted(tracked):
        if ((path.startswith("docs/") or path.endswith((".md", ".html", ".1")))
                and path not in allowed):
            errors.append(f"{path}: document absent from source-inventory allowlist")
        if path.startswith(".github/ISSUE_TEMPLATE/") and path not in forms:
            errors.append(f"{path}: issue form absent from release allowlist")
        if path.startswith("docs/benchmarks/"):
            errors.append(f"{path}: benchmark results belong in campaign artifacts")
    for path in sorted(forms):
        if path not in tracked:
            errors.append(f"{path}: allowlisted issue form missing")
            continue
        form = json.loads((ROOT / path).read_text(encoding="utf-8"))
        if path.endswith("/config.yml"):
            if form.get("blank_issues_enabled") is not False:
                errors.append(f"{path}: blank issue reports must use the forms")
        else:
            fields = {row.get("id"): row for row in form["body"]}
            for field in ("version", "platform", "reproduction"):
                if not fields.get(field, {}).get("validations", {}).get("required"):
                    errors.append(f"{path}: required {field} field missing")
    required = ("rmux-0.11.0", "Windows 10 is not qualified", "Speed is not qualified",
                "Floating panes are experimental", "Web Share is experimental",
                "Kitty Keyboard is disabled by default", "homebrew-core", "Scoop",
                "WinGet", "Chocolatey", "submission pending")
    for value in required:
        if value not in readme:
            errors.append(f"README.md: required release disclosure missing: {value}")
    release_url = publication_url(policy)
    published_urls = set()
    for name, guide in policy["external_guides"].items():
        ref, source = guide["source_ref"], guide["source_path"]
        # A supplied digest asserts checkout bytes, independently of the historical ref.
        if "source_sha256" in guide:
            source_sha = guide["source_sha256"]
            source_path = ROOT / source
            if not isinstance(source_sha, str) or not re.fullmatch(r"[0-9a-f]{64}", source_sha):
                errors.append(f"{POLICY}: {name} needs an exact source digest")
            elif (Path(source).is_absolute() or ".." in Path(source).parts
                  or source_path.resolve() != source_path.absolute()
                  or not source_path.is_file() or source not in tracked
                  or hashlib.sha256(source_path.read_bytes()).hexdigest() != source_sha):
                errors.append(f"{POLICY}: {name} source snapshot differs")
        if not re.fullmatch(r"[0-9a-f]{40}", ref):
            errors.append(f"{POLICY}: {name} needs an immutable source ref")
            continue
        if (git("cat-file", "-t", ref).strip() != b"commit"
                or git("cat-file", "-t", f"{ref}:{source}").strip() != b"blob"):
            errors.append(f"{POLICY}: {name} must bind a file at an exact commit")
        source_binding = f"{ref}:{source}"
        if source_binding not in readme:
            errors.append(f"README.md: {name} immutable guide source link missing")
        if guide["status"] == "pending":
            if guide["url"] is not None:
                errors.append(f"{POLICY}: {name} pending URL must be null")
        elif (guide["status"] != "published" or not isinstance(guide["url"], str)
              or not guide["url"].startswith("https://rmux.io/docs/") or ref not in guide["url"]):
            errors.append(f"{POLICY}: {name} guide URL must be versioned by source SHA")
        else:
            published_urls.add(guide["url"])
    for path in ("README.md", "docs/README.md"):
        text = (ROOT / path).read_text(encoding="utf-8")
        for url in re.findall(r'(?:https?://[^\s"<>`()]*)?/releases/tag/v0\.11\.0[^\s"<>`()]*', text):
            if url != release_url:
                errors.append(f"{path}: release URL requires matching published receipt evidence")
        for url in re.findall(r'https://rmux\.io/docs/[^\s"<>)]*', text):
            if url not in published_urls:
                errors.append(f"{path}: replace floating hosted guides with immutable sources or pending")
    if re.search(r"<table|\d+(?:\.\d+)?x faster|\d+\.\d+ ms", (ROOT / "docs/benchmarks.md").read_text(encoding="utf-8")):
        errors.append("docs/benchmarks.md: measured tables belong in campaign artifacts")
    return errors


def main():
    try:
        errors = check()
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        errors = claim_errors((ROOT / "README.md").read_text(encoding="utf-8"), "README.md") + [str(error)]
    for error in errors:
        print(f"release-docs: {error}", file=sys.stderr)
    if errors:
        return 1
    print("release-docs-allowlist=ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
