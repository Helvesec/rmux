"""D155: two bounded AST amendments, sealed evidence and append-only Git history."""
import copy
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re
import subprocess

from pin_proof_publication import proof_bytes

CHAIN = Path("benches/perf/qualification/pin-revisions.json")
PROOFS = CHAIN.parent / "pin-revision-proofs"
SCHEMA = "rmux-ast-pin-revisions/1"
ARCHIVE_SHA256 = "d2608970b6830d4d8e0688a3828916d380850a5bd41addede3104df4929041c5"
ATTRIBUTION = "R2-D155-PIN-SCOPE-INDEPENDENT-PROOF-223"
REQUIRED_CONDITIONS = {
    "exact_heads_clean", "pin_parent_is_corrected_source", "only_pin_manifest_file_changes",
    "only_authorized_ast_pin_changes", "corrected_source_equals_pin_source_bytes",
    "causal_population_bytes_unchanged", "changed_existing_definitions_only_target",
    "new_definitions_exactly_necessary_helper", "all_other_top_level_ast_unchanged",
    "preexisting_assertions_file_lines_unchanged", "old_ast_matches_old_pin",
    "new_ast_matches_revised_pin", "old_ast_matches_historical_source",
    "historical_source_digest_matches", "historical_archive_digest_size_blob_matches",
    "baseline_projection_matches_archive", "historical_locator_unchanged",
    "historical_old_pin_matches", "recipe_and_population_fields_unchanged", "guards_unchanged",
}
ALLOWED = {
    "relative_or_string": (
        "D147",
        "1cd07e5b657952cb14d37cfe960d60e4727cc61879e6a6abfa6866233e846847",
        "30f6f0796bfd0112990955ec7e622f37259f2d8bcc6e04c0c7c88c937029fea3",
    ),
    "screen_cleanup": (
        "D152",
        "8e89afd0378d93becffe1b197c4d3f3d85d2f361117f0bdc73e10bccc869a671",
        "93f2f644e5b135073197e9cec77622b85f152512d4806cc74e498a3f650c7ad4",
    ),
}


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":"),
                                     allow_nan=False).encode()).hexdigest()


def _unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate pin revision field")
        result[key] = value
    return result


def _document(raw):
    try:
        value = json.loads(raw, object_pairs_hook=_unique)
        if (not isinstance(value, dict)
                or set(value) != {"schema", "archive_sha256", "entries"}
                or value["schema"] != SCHEMA or value["archive_sha256"] != ARCHIVE_SHA256
                or not isinstance(value["entries"], list) or len(value["entries"]) > 2):
            raise ValueError("invalid pin revision document")
        return value
    except (TypeError, UnicodeDecodeError) as error:
        raise ValueError("invalid pin revision document") from error


def _git(repository, *arguments):
    try:
        result = subprocess.run(["git", "-C", str(repository), *arguments],
                                capture_output=True, timeout=10, check=False)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ValueError("pin revision Git history unavailable") from error
    if result.returncode:
        raise ValueError("pin revision Git history unavailable")
    return result.stdout


def _append_only(repository, entries):
    # A hash chain alone cannot detect a removed tail or a resealed old entry.
    # Every reachable committed version must be a prefix, including after merges.
    commits = _git(repository, "log", "--full-history", "--format=%H", "-65", "HEAD", "--", CHAIN.as_posix()).splitlines()
    if len(commits) > 64:
        raise ValueError("pin revision history exceeds bounded D155 scope")
    for commit in commits:
        raw = _git(repository, "show", commit.decode() + ":" + CHAIN.as_posix())
        prior = _document(raw)["entries"]
        if entries[:len(prior)] != prior:
            raise ValueError("pin revision history rewritten or removed")


def _proof(repository, proof, row):
    if not isinstance(proof, dict) or set(proof) != {"path", "sha256"}:
        raise ValueError("invalid R2 proof reference")
    if (not isinstance(proof["path"], str) or not isinstance(proof["sha256"], str)
            or not re.fullmatch("[0-9a-f]{64}", proof["sha256"])):
        raise ValueError("invalid R2 proof reference")
    path = Path(proof["path"])
    expected = PROOFS / ATTRIBUTION / "PROOF.json"
    if path != expected or proof["path"] != expected.as_posix():
        raise ValueError("R2 proof outside independent D155 attribution")
    target = Path(repository) / path
    if not target.resolve().is_relative_to(Path(repository).resolve()):
        raise ValueError("R2 proof escapes repository")
    try:
        if not 0 < target.stat().st_size <= 4 * 1024 * 1024:
            raise ValueError("invalid R2 proof size")
        raw = proof_bytes(repository)
    except (OSError, ValueError) as error:
        raise ValueError("R2 proof unreadable: " + str(error)) from error
    if hashlib.sha256(raw).hexdigest() != proof["sha256"]:
        raise ValueError("R2 proof sha256 differs")
    try:
        evidence = json.loads(raw, object_pairs_hook=_unique)
        revision = next(item for item in evidence["revisions"] if item["function"] == row["pin"])
        if (evidence["mandate"] != ATTRIBUTION or evidence["verdict"] != "PASS"
                or evidence["reviewer_role"] != "R2 independent scope proof"
                or set(evidence["conditions"]) != {"D147", "D152"}
                or any(value is not True for value in evidence["conditions"].values())
                or revision["decision"] != row["decision"]
                or revision["old_ast_sha256"] != row["old_ast_sha256"]
                or revision["new_ast_sha256"] != row["new_ast_sha256"]
                or not isinstance(revision["pin_commit"], str)
                or not re.fullmatch("[0-9a-f]{40}", revision["pin_commit"])
                or not REQUIRED_CONDITIONS <= revision["conditions"].keys()
                or (row["pin"] == "screen_cleanup"
                    and revision["conditions"].get("single_original_5s_budget") is not True)
                or any(value is not True for value in revision["conditions"].values())):
            raise ValueError("R2 proof does not authorize pin revision")
        issued = datetime.fromisoformat(evidence["created_utc"])
        recorded = datetime.strptime(row["recorded_at"], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
        if issued.tzinfo is None or issued > recorded:
            raise ValueError("pin revision predates its independent proof")
        # The sealed proof already carries each exact pin-changing commit. Keep
        # this binding instead of rewriting the append-only revision entries.
        return revision["pin_commit"]
    except (KeyError, TypeError, AttributeError, StopIteration) as error:
        raise ValueError("R2 proof attribution or pin scope missing") from error


def entries(repository):
    target = Path(repository) / CHAIN
    if not target.resolve().is_relative_to(Path(repository).resolve()):
        raise ValueError("pin revision chain escapes repository")
    try:
        raw = target.read_bytes()
    except OSError as error:
        raise ValueError("pin revision chain unreadable") from error
    if len(raw) > 65536:
        raise ValueError("pin revision chain too large")
    rows = _document(raw)["entries"]
    _append_only(repository, rows)
    previous = ARCHIVE_SHA256
    seen = set()
    last_date = datetime.min.replace(tzinfo=timezone.utc)
    for row in rows:
        fields = {"id", "pin", "old_ast_sha256", "new_ast_sha256", "decision", "proof",
                  "recorded_at", "attribution", "previous_sha256", "sha256"}
        if not isinstance(row, dict) or set(row) != fields or not isinstance(row["pin"], str):
            raise ValueError("invalid pin revision entry")
        pin = row["pin"]
        if pin not in ALLOWED or pin in seen:
            raise ValueError("unknown or repeated pin revision")
        decision, old, new = ALLOWED[pin]
        if (row["id"] != decision + "-" + pin or row["decision"] != decision
                or row["old_ast_sha256"] != old or row["new_ast_sha256"] != new
                or row["attribution"] != ATTRIBUTION):
            raise ValueError("pin revision outside D147/D152 authority")
        if (row["previous_sha256"] != previous
                or row["sha256"] != digest({k: v for k, v in row.items() if k != "sha256"})):
            raise ValueError("pin revision chain altered")
        try:
            date = datetime.strptime(row["recorded_at"], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
        except (TypeError, ValueError) as error:
            raise ValueError("invalid pin revision date") from error
        if (date <= last_date or date > datetime.now(timezone.utc)
                or row["recorded_at"] != date.strftime("%Y-%m-%dT%H:%M:%SZ")):
            raise ValueError("pin revision dates out of order or in future")
        _proof(repository, row["proof"], row)
        previous, last_date = row["sha256"], date
        seen.add(pin)
    return rows


def _snapshots(original, repository, commit):
    rows = entries(repository)
    # No path filter: include every parent, even a TREESAME merge's hidden side.
    # Object availability or an equivalent patch does not establish ancestry.
    reachable = set(_git(repository, "rev-list", "--full-history", commit).splitlines())
    state = copy.deepcopy(original)
    snapshots = [copy.deepcopy(state)]
    for row in rows:
        if _proof(repository, row["proof"], row).encode() not in reachable:
            continue
        pin = state["preserved_definitions"][row["pin"]]
        if pin["ast_sha256"] != row["old_ast_sha256"]:
            raise ValueError("pin revision does not extend historical AST")
        pin["ast_sha256"] = row["new_ast_sha256"]
        snapshots.append(copy.deepcopy(state))
    return snapshots


def at_epoch(original, repository, epoch):
    """Resolve a named, reachable full commit SHA through the sealed chain."""
    if not isinstance(epoch, str) or not re.fullmatch("[0-9a-f]{40}", epoch):
        raise ValueError("population epoch must be an exact commit SHA")
    reachable = set(_git(repository, "rev-list", "--full-history", "HEAD").splitlines())
    if epoch.encode() not in reachable:
        raise ValueError("population epoch is not in validated Git history")
    return _snapshots(original, repository, epoch)[-1]


def resolve(projection, original, repository, *, current=False):
    """Match a sealed historical state, or require the current activated state."""
    snapshots = _snapshots(original, repository, "HEAD")
    # Imported lazily to avoid an import cycle with the archive byte reader.
    from provenance_archive import public_projection
    candidates = snapshots[-1:] if current else snapshots
    if not any(projection == public_projection(snapshot) for snapshot in candidates):
        raise ValueError("public historical projection differs: undeclared or stale AST revision")
    return snapshots[-1]
