#!/usr/bin/env python3
"""Generate, check or query source-backed Web compatibility; never probes a service."""
from __future__ import annotations

import argparse
from dataclasses import asdict
import hashlib
from itertools import product
import json
from pathlib import Path
import sys
from typing import Any

from toml_reader import loads as read_toml
from rust_regex_validation import validate_regex, verify_engine
from web_compatibility_model import (
    ArtifactKind, Catalogue, NetworkKind, require,
)

ROOT = Path(__file__).resolve().parents[1]
SOURCE = "scripts/web-compatibility.json"
PUBLISHED = ".github/release/web-compatibility-matrix.json"
PRESETS = "crates/rmux-server/tunnels"


def validate_preset(preset: dict, expected_name: str) -> dict:
    """Check TunnelPreset's fields and regexes using the daemon's Rust engine."""
    required = {"name", "program", "url_pattern"}
    optional = {"args", "ready_pattern", "url_source", "ready_timeout_secs", "install_hint"}
    require(required <= preset.keys() and preset.keys() <= required | optional,
            f"invalid provider preset fields: {expected_name}")
    for field in required | {"ready_pattern", "install_hint"}:
        if field in preset:
            require(isinstance(preset[field], str),
                    f"invalid provider preset {field}: {expected_name}")
    name = preset["name"]
    require(name == expected_name and bool(name)
            and all(char.isascii() and (char.isalnum() or char in "-_") for char in name),
            f"preset name differs or is invalid: {expected_name}")
    # Rust str::trim uses Unicode White_Space, unlike Python's extra C0 separators.
    whitespace = " \t\n\r\v\f\u0085\u00a0\u1680\u2000\u2001\u2002\u2003\u2004\u2005\u2006\u2007\u2008\u2009\u200a\u2028\u2029\u202f\u205f\u3000"
    require(bool(preset["program"].strip(whitespace)) and "\0" not in preset["program"],
            f"invalid provider preset program: {expected_name}")
    args = preset.get("args", [])
    require(isinstance(args, list)
            and all(isinstance(arg, str) and "\0" not in arg for arg in args),
            f"invalid provider preset args: {expected_name}")
    timeout = preset.get("ready_timeout_secs", 30)
    require(type(timeout) is int and 1 <= timeout <= 300,
            f"invalid provider preset ready_timeout_secs: {expected_name}")
    source = preset.get("url_source", "both")
    require(isinstance(source, str) and source in {"both", "stderr", "stdout"},
            f"invalid provider preset url_source: {expected_name}")
    for field in ("url_pattern", "ready_pattern"):
        if field in preset:
            validate_regex(preset[field], field, expected_name)
    return {**preset, "args": args, "ready_timeout_secs": timeout, "url_source": source}


def text(root: Path, path: str) -> str:
    target = root / path
    require(target.resolve().is_relative_to(root.resolve()) and target.is_file(),
            f"support file absent or outside source: {path}")
    return target.read_text(encoding="utf-8")


def digest(root: Path, path: str) -> str:
    text(root, path)
    return hashlib.sha256((root / path).read_bytes()).hexdigest()


def verify_source(root: Path, catalogue: Catalogue) -> dict[str, dict]:
    """Bind declared combinations to current runtime entry paths and manifests."""
    evidence = {item.id: item for item in catalogue.support}
    for item in catalogue.support:
        require(item.contains in text(root, item.path), f"support anchor absent: {item.id}")
    protocol = "crates/rmux-server/src/web/protocol/mod.rs"
    crypto = "crates/rmux-server/src/web/crypto.rs"
    require(f"WEB_SHARE_PROTOCOL_VERSION: u16 = {catalogue.protocol_version};" in text(root, protocol),
            "daemon protocol version is not implemented")
    require(f'E2EE_CAPABILITY: &str = "{catalogue.capability}";' in text(root, crypto),
            "daemon capability is not implemented")
    package = read_toml(text(root, "Cargo.toml"))
    server = read_toml(text(root, "crates/rmux-server/Cargo.toml"))
    wasm = read_toml(text(root, "crates/rmux-web-crypto/Cargo.toml"))
    verify_engine(text(root, "Cargo.lock"))
    require(package["features"]["default"] == ["web"]
            and package["features"]["web"] == ["rmux-server/web"]
            and package["dependencies"]["rmux-server"]["default-features"] is False
            and "tiny-cli" in package["features"]
            and server["features"]["default"] == ["web"]
            and "dep:rmux-web-crypto" in server["features"]["web"]
            and wasm["features"]["wasm"] == ["dep:wasm-bindgen"],
            "Web artifact feature support changed")

    def backed(row: Any, required: set[str]) -> None:
        require(required <= set(row.support), f"missing implementation support: {row.id}")

    for row in catalogue.frontends:
        backed(row, {"protocol", "capability", "browser-crypto"})
        require(row.protocol_version == catalogue.protocol_version,
                f"frontend protocol not implemented: {row.id}")
    for row in catalogue.daemons:
        backed(row, {"protocol", "capability", "web-feature"})
        require(row.protocol_version == catalogue.protocol_version,
                f"daemon protocol not implemented: {row.id}")
    artifact_support = {
        ArtifactKind.NATIVE: {"web-feature"},
        ArtifactKind.DELEGATED: {"web-feature", "tiny-helper"},
        ArtifactKind.DISABLED: {"web-disabled"},
        ArtifactKind.CRYPTO: {"browser-crypto"},
    }
    require({row.kind for row in catalogue.artifacts} == set(ArtifactKind),
            "missing real artifact kind")
    for row in catalogue.artifacts:
        backed(row, artifact_support[row.kind])
    require({row.kind for row in catalogue.networks} == set(NetworkKind),
            "missing real network kind")
    providers = {}
    for row in catalogue.networks:
        backed(row, {NetworkKind.LOOPBACK: {"loopback"},
                     NetworkKind.EXTERNAL: {"external-url"},
                     NetworkKind.PROVIDER: {"provider-runner", "preset-loader"}}[row.kind])
        if row.kind is not NetworkKind.PROVIDER:
            continue
        require(row.preset == f"{PRESETS}/{row.id}.toml", f"orphan provider preset: {row.id}")
        preset = validate_preset(read_toml(text(root, row.preset)), row.id)
        providers[row.id] = {"id": row.id, "preset": row.preset,
                             "program": preset["program"], "args": preset["args"],
                             "install_hint": preset.get("install_hint"),
                             "requirements": list(row.requirements),
                             "support": sorted({row.preset, *(evidence[key].path for key in row.support)})}
    actual = {path.name for path in (root / PRESETS).glob("*.toml")}
    require(actual == {f"{name}.toml" for name in providers},
            "provider catalogue differs from shipped presets (missing or orphan provider)")
    require('Path::new("tunnels")' in text(root, "crates/rmux-server/build.rs")
            and "tunnel_presets.rs" in text(root, "crates/rmux-server/build.rs"),
            "shipped preset embedding changed")
    return providers


def generate(root: Path = ROOT) -> dict:
    catalogue = Catalogue.load(root / SOURCE)
    providers = verify_source(root, catalogue)
    paths = {item.id: item.path for item in catalogue.support}

    def support(*rows: Any) -> list[str]:
        return sorted({paths[key] for row in rows for key in row.support})

    pairs = []
    for frontend, daemon in product(catalogue.frontends, catalogue.daemons):
        pairs.append({"frontend": frontend.id, "daemon": daemon.id, "status": "supported",
                      "protocol_version": catalogue.protocol_version,
                      "capability": catalogue.capability, "support": support(frontend, daemon),
                      "requirements": list(frontend.requirements + daemon.requirements)})
    cells = []
    for artifact, network in product(catalogue.artifacts, catalogue.networks):
        enabled = artifact.kind in {ArtifactKind.NATIVE, ArtifactKind.DELEGATED}
        reason = "" if enabled else (
            "Web feature is absent; no Web listener or tunnel runner is available."
            if artifact.kind is ArtifactKind.DISABLED else
            "WASM is a browser crypto library, not a daemon or tunnel runner.")
        references = support(artifact, network)
        requirements = list(artifact.requirements + network.requirements)
        if network.kind is NetworkKind.PROVIDER:
            provider = providers[network.id]
            references = sorted({*references, provider["preset"]})
            requirements.append(f"External executable required: {provider['program']}.")
            if provider["install_hint"]:
                requirements.append(provider["install_hint"])
        cells.append({"artifact": artifact.id, "network": network.id,
                      "status": "supported" if enabled else "unsupported", "reason": reason,
                      "support": references, "requirements": requirements})
    evidence_paths = {*paths.values(), "Cargo.toml", "Cargo.lock", "crates/rmux-server/Cargo.toml",
                      "crates/rmux-web-crypto/Cargo.toml", "crates/rmux-server/build.rs",
                      *(provider["preset"] for provider in providers.values())}
    return {
        "schema_version": 1,
        "source": {"path": SOURCE, "sha256": digest(root, SOURCE)},
        "scope": "Implemented source compatibility only; native qualification, live frontend identity and external service availability are separate evidence. Unknown identifiers are refused.",
        "frontends": [asdict(row) for row in catalogue.frontends],
        "daemons": [asdict(row) for row in catalogue.daemons],
        "artifacts": [asdict(row) for row in catalogue.artifacts],
        "networks": [asdict(row) for row in catalogue.networks],
        "providers": list(providers.values()),
        "frontend_daemon": pairs,
        "artifact_network": cells,
        "implementation": [{"path": path, "sha256": digest(root, path)}
                           for path in sorted(evidence_paths)],
    }


def render(matrix: dict) -> str:
    # One row per line keeps the real Cartesian product reviewable.
    lines = ["{"]
    for key, value in matrix.items():
        prefix = f"  {json.dumps(key)}: "
        if isinstance(value, list):
            lines.append(prefix + "[")
            lines.extend("    " + json.dumps(row, ensure_ascii=False, sort_keys=True) +
                         ("," if index + 1 < len(value) else "")
                         for index, row in enumerate(value))
            lines.append("  ],")
        else:
            lines.append(prefix + json.dumps(value, ensure_ascii=False, sort_keys=True) + ",")
    lines[-1] = lines[-1].removesuffix(",")
    return "\n".join([*lines, "}", ""])


def validate_published(root: Path = ROOT) -> dict:
    expected = generate(root)
    require(text(root, PUBLISHED) == render(expected),
            "published Web compatibility matrix is stale or incomplete; regenerate it")
    return expected


def lookup(matrix: dict, table: str, **selection: str) -> dict:
    keys = {"frontend_daemon": {"frontend", "daemon"},
            "artifact_network": {"artifact", "network"}}
    require(table in keys and set(selection) == keys[table], "invalid compatibility query")
    matches = [row for row in matrix[table] if all(row[key] == value for key, value in selection.items())]
    require(len(matches) == 1, f"unknown or absent compatibility combination: {selection}")
    row = matches[0]
    if row["status"] != "supported":
        raise ValueError(f"unsupported compatibility combination: {row['reason']}")
    return row


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("generate", "check", "query"))
    parser.add_argument("--root", type=Path, default=ROOT)
    for name in ("frontend", "daemon", "artifact", "network"):
        parser.add_argument("--" + name)
    args = parser.parse_args()
    try:
        if args.command == "generate":
            (args.root / PUBLISHED).write_text(render(generate(args.root)), encoding="utf-8")
        else:
            matrix = validate_published(args.root)
            if args.command == "query":
                selection = {key: getattr(args, key) for key in ("frontend", "daemon", "artifact", "network")
                             if getattr(args, key) is not None}
                table = "frontend_daemon" if "frontend" in selection else "artifact_network"
                print(json.dumps(lookup(matrix, table, **selection), ensure_ascii=False))
                return 0
        print("web-compatibility-ok")
        return 0
    except (ValueError, OSError, KeyError, TypeError) as error:
        print(f"Web compatibility: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
