"""Portable publication locators for the unchanged, independently sealed R2 proof."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

from provenance_archive import SCHEMA as ARCHIVE_SCHEMA, archived_bytes

ATTRIBUTION = "R2-D155-PIN-SCOPE-INDEPENDENT-PROOF-223"
DIRECTORY = Path("benches/perf/qualification/pin-revision-proofs") / ATTRIBUTION
ORIGINALS = Path(".rmux-audit/pin-proof-originals")
CHAIN = DIRECTORY / "publications.json"
ORIGIN = "52464c10c163cde491cb9f6d223baec4f1a3889b"
SCHEMA = "rmux-r2-proof-publications/1"
SOURCES = {
    "PROOF.json": {"git_blob": "a2ed116d9db37916b957539f4c53a07bc9f5da2d", "bytes": 19716,
                   "sha256": "d4c0da9884cd019d4d6d35c258c8f74d726b399cadd5f7725febae50f7f72fee"},
    "PREUVE.md": {"git_blob": "72bcf138bef9ad128b7a437e2ae984e1497ec396", "bytes": 10719,
                  "sha256": "b6a92e42d68c71a15f42c073e65df7443ec2d3c79eb3478d26825e3e68474301"},
    "REVUE.md": {"git_blob": "ca86c222c65d9a631c8bb02528ec1b1f9ba88918", "bytes": 1326,
                 "sha256": "92f297ffcf9d696776f47a1e4b4bf87f0c42e1235f3ae19cf122b2fde63208da"},
}


def encode(value):
    return (json.dumps(value, indent=2, ensure_ascii=False) + "\n").encode()


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def locator(name):
    return {"schema": ARCHIVE_SCHEMA, **SOURCES[name]}


def publication(name):
    """Publish coordinates, never a rewritten or encoded copy of sealed evidence."""
    if name == "PROOF.json":
        return encode(locator(name))
    return ("# Archived independent R2 evidence\n\n"
            "This is an S1 publication locator under D161, not a new R2 verdict.\n"
            "The original sealed document remains byte-exact in Git history and private audit.\n\n"
            f"Source: `{ORIGIN}:{(DIRECTORY / name).as_posix()}`.\n"
            "Verify the source bytes with this size- and SHA-256-bound locator:\n\n"
            "```json\n" + encode(locator(name)).decode() + "```\n\n"
            "Publication chain: [publications.json](publications.json).\n").encode()


def document():
    row = {"id": "D161-INT331-portable-publication", "decision": "D161",
           "source_attribution": ATTRIBUTION, "publisher": "S1",
           "source_commit": ORIGIN, "previous_sha256": SOURCES["PROOF.json"]["sha256"],
           "artifacts": [{"path": (DIRECTORY / name).as_posix(), "source": locator(name),
                          "publication_sha256": digest(publication(name))} for name in SOURCES]}
    row["sha256"] = digest(json.dumps(row, sort_keys=True, separators=(",", ":")).encode())
    return {"schema": SCHEMA, "entries": [row]}


def _unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate R2 publication field")
        result[key] = value
    return result


def _git(repository, *args):
    try:
        result = subprocess.run(["git", "-C", str(repository), *args], capture_output=True,
                                check=False, timeout=10)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ValueError("R2 publication history unavailable") from error
    if result.returncode:
        raise ValueError("R2 publication history unavailable")
    return result.stdout


def _local(repository, path, max_bytes=4 * 1024 * 1024):
    root = Path(repository).resolve()
    target = root / path
    if not target.resolve().is_relative_to(root):
        raise ValueError("R2 publication escapes repository")
    try:
        if not 0 < target.stat().st_size <= max_bytes:
            raise ValueError("invalid R2 publication size")
        return target.read_bytes()
    except OSError as error:
        raise ValueError("R2 publication unreadable") from error


def _history(repository):
    return _git(repository, "log", "--full-history", "--format=%H", "-17", "HEAD",
                "--", CHAIN.as_posix()).splitlines()


def validate(repository):
    expected = document()
    raw = _local(repository, CHAIN, 65536)
    try:
        actual = json.loads(raw, object_pairs_hook=_unique)
    except (ValueError, UnicodeDecodeError) as error:
        raise ValueError("invalid R2 publication chain") from error
    if actual != expected:
        raise ValueError("R2 publication chain altered or removed")
    history = _history(repository)
    if len(history) > 16:
        raise ValueError("R2 publication history exceeds bounded D161 scope")
    for commit in history:
        prior = json.loads(_git(repository, "show", commit.decode() + ":" + CHAIN.as_posix()),
                           object_pairs_hook=_unique)
        if prior != expected:
            raise ValueError("R2 publication history rewritten or removed")
    # Bind each original to the introducing commit as well as its blob and SHA.
    for name, source in SOURCES.items():
        blob = _git(repository, "rev-parse", ORIGIN + ":" + (DIRECTORY / name).as_posix()).strip()
        if blob.decode() != source["git_blob"]:
            raise ValueError("R2 publication source commit differs")
        archived_bytes(locator(name), repository)
        if _local(repository, DIRECTORY / name) != publication(name):
            raise ValueError("R2 publication differs from declared locator")
    return expected


def proof_bytes(repository):
    """Resolve publication to authentic evidence; keep old/synthetic direct reads."""
    chain = Path(repository) / CHAIN
    if chain.exists() or chain.is_symlink():
        validate(repository)
        return archived_bytes(locator("PROOF.json"), repository)
    if _history(repository):
        raise ValueError("R2 publication chain removed")
    return _local(repository, DIRECTORY / "PROOF.json")


def write(repository):
    """Preserve originals before publishing; repeated generation is byte-idempotent."""
    repository = Path(repository)
    if (repository / CHAIN).exists():
        validate(repository)
        return
    if _history(repository):
        raise ValueError("R2 publication chain removed")
    originals = {name: archived_bytes(locator(name), repository) for name in SOURCES}
    for name, raw in originals.items():
        if _local(repository, DIRECTORY / name) != raw:
            raise ValueError("R2 original differs before publication")
    private = repository / ORIGINALS / ATTRIBUTION
    private.mkdir(parents=True, mode=0o700, exist_ok=True)
    for name, raw in originals.items():
        target = private / name
        if target.exists() and target.read_bytes() != raw:
            raise ValueError("private R2 original differs")
        target.write_bytes(raw)
        target.chmod(0o600)
    (private / "SHA256SUMS").write_text("".join(
        f"{digest(raw)}  {name}\n" for name, raw in originals.items()))
    for name in SOURCES:
        (repository / DIRECTORY / name).write_bytes(publication(name))
    (repository / CHAIN).write_bytes(encode(document()))
    validate(repository)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help="preserve and publish exact R2 locators")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    write(root) if args.write else validate(root)


if __name__ == "__main__":
    main()
