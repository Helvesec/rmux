"""Read byte-exact historical evidence through a bounded local Git blob locator."""
import hashlib
import json
from pathlib import Path
import re
import subprocess

SCHEMA = "rmux-git-provenance/1"
MAX_BYTES = 4 * 1024 * 1024


def archived_bytes(locator, repository):
    """Read one allowlisted, size- and digest-bound local blob."""
    if (not isinstance(locator, dict) or locator.get("schema") != SCHEMA
            or set(locator) != {"schema", "git_blob", "bytes", "sha256"}
            or not isinstance(locator["git_blob"], str)
            or not re.fullmatch("[0-9a-f]{40}", locator["git_blob"])
            or not isinstance(locator["sha256"], str)
            or not re.fullmatch("[0-9a-f]{64}", locator["sha256"])
            or type(locator["bytes"]) is not int
            or not 0 < locator["bytes"] <= MAX_BYTES):
        raise ValueError("invalid historical Git locator")
    try:
        size = subprocess.run(
            ["git", "-C", str(repository), "cat-file", "-s", locator["git_blob"]],
            capture_output=True, timeout=10, check=False,
        )
        if size.returncode != 0 or size.stdout.strip() != str(locator["bytes"]).encode():
            raise ValueError("historical Git size differs or is unavailable")
        result = subprocess.run(
            ["git", "-C", str(repository), "cat-file", "blob", locator["git_blob"]],
            capture_output=True, timeout=10, check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ValueError("historical Git object unavailable") from error
    if (result.returncode != 0 or len(result.stdout) != locator["bytes"]
            or hashlib.sha256(result.stdout).hexdigest() != locator["sha256"]):
        raise ValueError("historical Git bytes differ or are unavailable")
    return result.stdout


def public_projection(value):
    """Redact personal home roots while retaining the historical locator tail."""
    if isinstance(value, str):
        value = re.sub(r"(?:/Users|/home)/[^/]+/Projects/", "historical-projects/", value)
        return re.sub(r"(?:/Users|/home)/[^/]+/", "historical-home/", value)
    if isinstance(value, list):
        return [public_projection(item) for item in value]
    if isinstance(value, dict):
        return {key: public_projection(item) for key, item in value.items()}
    return value


def decode(raw, repository, *, current=False, epoch=None):
    """Return verified historical bytes, or the D155 current population on opt-in."""
    if epoch is not None:
        if current:
            raise ValueError("population epoch and current selection are exclusive")
        # Verify the supplied archive/projection before selecting another epoch.
        # A named epoch never bypasses the full HEAD chain or its sealed proof.
        original = decode(raw, repository)
        import pin_revisions
        if hashlib.sha256(original).hexdigest() != pin_revisions.ARCHIVE_SHA256:
            raise ValueError("population epoch has no verified archive")
        resolved = pin_revisions.at_epoch(json.loads(original), repository, epoch)
        if resolved == json.loads(original):
            return original
        return json.dumps(resolved, sort_keys=True, separators=(",", ":")).encode()
    try:
        value = json.loads(raw)
    except (ValueError, UnicodeDecodeError):
        if current:
            raise ValueError("current population has no verified archive")
        return raw
    if not isinstance(value, dict):
        if current:
            raise ValueError("current population has no verified archive")
        return raw
    if value.get("schema") == SCHEMA:
        original = archived_bytes(value, repository)
        if current:
            import pin_revisions
            if value["sha256"] != pin_revisions.ARCHIVE_SHA256:
                raise ValueError("current population has no verified archive")
            pin_revisions.resolve(public_projection(json.loads(original)),
                                  json.loads(original), repository, current=True)
        return original
    if (isinstance(value.get("historical_source"), dict)
            and value["historical_source"].get("schema") == SCHEMA):
        original = archived_bytes(value["historical_source"], repository)
        projection = {key: item for key, item in value.items() if key != "historical_source"}
        # D155 applies only to the digest-bound R5-B population. Other archives
        # retain the exact comparison, without a general revision escape hatch.
        import pin_revisions
        if value["historical_source"]["sha256"] == pin_revisions.ARCHIVE_SHA256:
            resolved = pin_revisions.resolve(projection, json.loads(original), repository, current=current)
            if current and resolved != json.loads(original):
                return json.dumps(resolved, sort_keys=True, separators=(",", ":")).encode()
        elif projection != public_projection(json.loads(original)):
            raise ValueError("public historical projection differs")
        elif current:
            raise ValueError("current population has no verified archive")
        return original
    if current:
        import pin_revisions
        if hashlib.sha256(raw).hexdigest() != pin_revisions.ARCHIVE_SHA256:
            raise ValueError("current population has no verified archive")
        pin_revisions.resolve(public_projection(value), value, repository, current=True)
    return raw


def read(path, repository, *, current=False, epoch=None):
    """Read a record; current=True requires the latest declared population pins."""
    return decode(Path(path).read_bytes(), repository, current=current, epoch=epoch)
