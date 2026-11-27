"""Versioned additions to the historical package layouts."""
from copy import deepcopy
import json
from pathlib import Path

PROFILE = Path(__file__).with_name("notice-layout.json")


def with_notices(base):
    profile = json.loads(PROFILE.read_text(encoding="utf-8"))
    if profile.get("schema") != 1:
        raise ValueError("unsupported notice layout profile")
    result = deepcopy(base)
    for name, identity in profile["identities"].items():
        result["variants"][name]["package_layout"] = identity
    by_id = {entry["id"]: entry for entry in result["entries"]}
    for addition in profile["entries"]:
        if addition["id"] in by_id:
            for name, placement in addition["variants"].items():
                by_id[addition["id"]]["variants"][name].update(placement)
        else:
            result["entries"].append(addition)
    return result


def for_package(base, root):
    root = Path(root)
    paths = [root / prefix / "artifact-metadata.json"
             for prefix in ("share/rmux", "usr/share/rmux")]
    existing = [path for path in paths if path.is_file()]
    if len(existing) != 1:
        raise ValueError("package must carry exactly one artifact metadata file")
    # Select only from metadata inside the extracted package boundary.
    existing[0].resolve().relative_to(root.resolve())
    metadata = json.loads(existing[0].read_text(encoding="utf-8-sig"))
    if not isinstance(metadata, dict):
        raise ValueError("package metadata must be an object")
    identity = metadata.get("package_layout")
    current = with_notices(base)
    if identity in {v["package_layout"] for v in current["variants"].values()}:
        return current
    historical = {identity for v in base["variants"].values()
                  for identity in [v["package_layout"], *v.get("legacy_package_layouts", [])]}
    if identity in historical:
        return base
    raise ValueError(f"unsupported package layout: {identity!r}")
