"""Verify that share.rmux.io serves one exact repository commit and WASM."""

from __future__ import annotations

import hashlib
import json
import re
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

MANIFEST_URL = "https://share.rmux.io/.well-known/rmux-web-share.json"
PUBLIC_ORIGIN = "https://share.rmux.io"
REPOSITORY_URL = "https://github.com/Helvesec/rmux-web-share"
ASSET_PATH = re.compile(r"/_astro/[A-Za-z0-9._-]+\.wasm")


def expected_wasm_hash(provenance_path: Path, source_sha: str, version: str) -> str:
    try:
        value = json.loads(provenance_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("web share provenance is invalid") from error
    if not isinstance(value, dict):
        raise ValueError("web share provenance must be an object")
    source = value.get("source")
    artifacts = value.get("artifacts")
    digest = (
        artifacts.get("rmux_web_crypto_wasm_bg.wasm")
        if isinstance(artifacts, dict)
        else None
    )
    if (
        value.get("version") != version
        or not isinstance(source, dict)
        or source.get("source_commit") != source_sha
        or not isinstance(digest, str)
        or not digest.startswith("sha256:")
        or len(digest) != 71
    ):
        raise ValueError("web share provenance identity differs")
    raw = digest.removeprefix("sha256:")
    if any(character not in "0123456789abcdef" for character in raw):
        raise ValueError("web share provenance WASM digest is invalid")
    return raw


def validate_manifest(
    value: Any, *, commit_sha: str, wasm_sha256: str
) -> tuple[str, int]:
    if (
        not isinstance(value, dict)
        or value.get("schema_version") != 1
        or value.get("project") != "rmux-web-share"
        or value.get("public_origin") != PUBLIC_ORIGIN
        or value.get("repository") != REPOSITORY_URL
        or value.get("commit_sha1") != commit_sha
        or value.get("commit_url") != f"{REPOSITORY_URL}/commit/{commit_sha}"
    ):
        raise ValueError("public Web Share manifest does not name the exact commit")
    assets = value.get("assets")
    if not isinstance(assets, list):
        raise ValueError("public Web Share manifest has no asset inventory")
    matches = [
        item
        for item in assets
        if isinstance(item, dict)
        and isinstance(item.get("path"), str)
        and ASSET_PATH.fullmatch(item["path"]) is not None
    ]
    if len(matches) != 1:
        raise ValueError("public Web Share manifest has no unique WASM asset")
    asset = matches[0]
    if asset.get("sha256") != wasm_sha256 or type(asset.get("bytes")) is not int:
        raise ValueError("public Web Share manifest WASM identity differs")
    if asset["bytes"] <= 0:
        raise ValueError("public Web Share manifest reports an empty WASM asset")
    return asset["path"], asset["bytes"]


def fetch(url: str, limit: int, expected_content_type: str) -> bytes:
    request = urllib.request.Request(
        url,
        headers={
            "Accept": "application/json" if url == MANIFEST_URL else "application/wasm",
            "Cache-Control": "no-cache",
            "User-Agent": "rmux-release-writer/1 (security@rmux.io)",
        },
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        final = urlsplit(response.geturl())
        content_type = response.headers.get_content_type()
        if (
            final.scheme != "https"
            or final.hostname != "share.rmux.io"
            or final.port is not None
            or final.username is not None
            or final.password is not None
            or content_type != expected_content_type
        ):
            raise ValueError("public Web Share response origin or media type differs")
        data = response.read(limit + 1)
    if len(data) > limit:
        raise ValueError("public Web Share response exceeds the release limit")
    return data


def wait_for_live(
    *, provenance_path: Path, source_sha: str, version: str, commit_sha: str
) -> str:
    wasm_sha256 = expected_wasm_hash(provenance_path, source_sha, version)
    last_error = "deployment has not appeared"
    for attempt in range(61):
        try:
            raw = fetch(MANIFEST_URL, 4 * 1024 * 1024, "application/json")
            manifest = json.loads(raw)
            asset_path, expected_size = validate_manifest(
                manifest, commit_sha=commit_sha, wasm_sha256=wasm_sha256
            )
            wasm = fetch(
                f"{PUBLIC_ORIGIN}{asset_path}",
                16 * 1024 * 1024,
                "application/wasm",
            )
            if (
                len(wasm) != expected_size
                or hashlib.sha256(wasm).hexdigest() != wasm_sha256
            ):
                raise ValueError("public Web Share WASM bytes differ")
            return MANIFEST_URL
        except (
            json.JSONDecodeError,
            UnicodeDecodeError,
            urllib.error.HTTPError,
            urllib.error.URLError,
            ValueError,
        ) as error:
            last_error = str(error)
        if attempt < 60:
            time.sleep(5)
    raise ValueError(f"Web Share deployment did not become exact: {last_error}")
