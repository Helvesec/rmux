#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const FIXTURE: &str = r#"
import copy
import hashlib
import importlib.util
import json
import subprocess
import sys
from pathlib import Path

repo, root = map(Path, sys.argv[1:])
script = repo / "scripts/release/candidate-manifest.py"
source = subprocess.run(
    ["git", "rev-parse", "HEAD"], cwd=repo, check=True,
    capture_output=True, text=True,
).stdout.strip()

def write(name, value):
    path = root / name
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
    return path

def file_hash(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def semantic_hash(value):
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest()

def job(identifier, name):
    return {
        "id": identifier, "name": name, "conclusion": "success",
        "labels": ["ubuntu-latest"], "runner_id": identifier,
        "runner_name": f"GitHub Actions {identifier}",
        "runner_group_id": 0, "runner_group_name": "GitHub Actions",
    }

policy_path = root / "policy.json"
subprocess.run([
    sys.executable, repo / "scripts/release/policy-root.py",
    "--source-sha", source, "--output", policy_path,
], cwd=repo, check=True)
policy = json.loads(policy_path.read_text())
contract = next(
    item for item in policy["records"]
    if item["path"] == ".github/release/candidate-contract.json"
)

def proof(kind, run_id, started, verified, jobs):
    value = {
        "schema_version": 1, "kind": kind, "repository_id": 1239918790,
        "run_id": run_id, "run_attempt": 1, "source_git_sha": source,
        "run_started_at": started, "verified_at": verified,
        "contract_sha256": contract["sha256"],
        "contract_blob_oid": contract["blob_oid"],
        "test_fixture": False, "jobs": jobs,
    }
    value["proof_sha256"] = semantic_hash(value)
    return value

fast = proof("fast", 42, "2026-07-19T00:00:00Z", "2026-07-19T00:11:00Z", [job(1, "Fast gate")])
candidate_jobs = [job(100 + n, f"Candidate proof job {n:02}") for n in range(49)]
candidate_jobs.append(job(200, "Release candidate gate"))
candidate = proof(
    "candidate", 77, "2026-07-19T00:10:00Z",
    "2026-07-19T01:00:00Z", candidate_jobs,
)
fast_path = write("fast.json", fast)
candidate_path = write("candidate.json", candidate)
intent_path = write("intent.json", {
    "schema_version": 1, "repository_id": 1239918790,
    "source_git_sha": source, "fast_run_id": 42,
    "release_intent_id": "shadow:manifest:test",
    "planned_release_ref": "v0.9.0", "release_kind": "shadow",
    "release_version": "0.9.0", "package_version": "0.9.0",
    "is_prerelease": False, "candidate_run_attempt": 1,
})

platforms = [
    ("linux-x86_64", "x86_64-unknown-linux-gnu", "ubuntu-22.04", "Linux", "X64", ["archive", "checksums", "crate-package-set", "debian", "rpm", "snap-amd64", "wasm-byte-set", "wasm-provenance"]),
    ("linux-aarch64", "aarch64-unknown-linux-gnu", "ubuntu-22.04-arm", "Linux", "ARM64", ["archive", "checksums", "debian", "rpm", "snap-arm64"]),
    ("macos-x86_64", "x86_64-apple-darwin", "macos-15-intel", "macOS", "X64", ["archive", "checksums"]),
    ("macos-aarch64", "aarch64-apple-darwin", "macos-15", "macOS", "ARM64", ["archive", "checksums"]),
    ("windows-x86_64", "x86_64-pc-windows-msvc", "windows-latest", "Windows", "X64", ["archive", "checksums", "chocolatey-package"]),
]
records, bindings, assets, provenances, canonical = {}, {}, [], [], []
for number, (key, target, image, os_name, arch, roles) in enumerate(platforms, 1):
    files = [
        {"path": f"{index:02}-{role}.bin", "role": role, "size": number,
         "sha256": f"{number:064x}"}
        for index, role in enumerate(roles, 1)
    ]
    runner = {"image": image, "os": os_name, "arch": arch, "environment": "github-hosted"}
    toolchain = {
        "requested": "1.96.1", "release": "1.96.1", "host": target,
        "commit_hash": source, "rustc_verbose_sha256": f"{number:064x}",
    }
    record = {
        "schema_version": 1, "repository_id": 1239918790,
        "source_git_sha": source, "fast_run_id": 42,
        "candidate_run_id": 77, "candidate_run_attempt": 1,
        "release_intent_id": "shadow:manifest:test",
        "planned_release_ref": "v0.9.0", "release_kind": "shadow",
        "platform": {"key": key, "target_triple": target,
                     "archive_format": "zip" if key.startswith("windows") else "tar.gz"},
        "runner": runner, "toolchain": toolchain,
        "build_policy": {
            "cargo_incremental": False, "cargo_locked": True,
            "fresh_target": True, "object_cache_restored": False,
            "publication_authority": False,
        },
        "created_at": "2026-07-19T00:20:00Z", "files": files,
    }
    records[key] = write(f"{key}-record.json", record)
    record_sha = file_hash(records[key])
    asset = {
        "role": "canonical-assets", "platform_key": key,
        "artifact_id": 300 + number,
        "name": f"rmux-canonical-{key}-{source}",
        "archive_digest": f"sha256:{number:064x}", "size_in_bytes": 1000 + number,
    }
    provenance = {
        "role": "canonical-provenance", "platform_key": key,
        "artifact_id": 400 + number,
        "name": f"rmux-canonical-provenance-{key}-{source}",
        "archive_digest": f"sha256:{number + 8:064x}", "size_in_bytes": 2000 + number,
    }
    attestation = {
        "attestation_id": f"attestation-{key}",
        "bundle_file": "build-provenance.sigstore.json",
        "bundle_sha256": f"{number + 12:064x}",
    }
    binding = {
        "schema_version": 1, "repository_id": 1239918790,
        "source_git_sha": source, "candidate_run_id": 77, "platform_key": key,
        "assets": {"artifact_id": asset["artifact_id"],
                   "artifact_name": asset["name"],
                   "artifact_digest": asset["archive_digest"]},
        "build_record_sha256": record_sha, "attestation": attestation,
    }
    bindings[key] = write(f"{key}-binding.json", binding)
    canonical.append({
        "platform_key": key, "assets_artifact": asset,
        "provenance_artifact": provenance, "build_record_sha256": record_sha,
        "attestation_id": attestation["attestation_id"],
        "attestation_bundle_sha256": attestation["bundle_sha256"],
        "runner": runner, "toolchain": toolchain, "files": files,
    })
    assets.append(asset)
    provenances.append(provenance)

fast_artifact = {
    "role": "fast-proof", "platform_key": None, "artifact_id": 250,
    "name": f"rmux-fast-proof-{source}",
    "archive_digest": f"sha256:{15:064x}", "size_in_bytes": 900,
}
metadata_path = write("metadata.json", {
    "schema_version": 1, "status": "verified-for-shadow-sealing",
    "repository_id": 1239918790, "source_git_sha": source,
    "fast_run_id": 42, "candidate_run_id": 77, "candidate_run_attempt": 1,
    "release_intent_id": "shadow:manifest:test",
    "planned_release_ref": "v0.9.0", "release_kind": "shadow",
    "resolution_sha256": f"{16:064x}",
    "fast_evidence": {"proof_sha256": fast["proof_sha256"], "nextest_artifact": {}},
    "source_artifacts": [fast_artifact, *assets, *provenances],
    "canonical_platforms": canonical,
})

def arguments(command, destination):
    values = [
        sys.executable, script, command,
        "--candidate-proof", candidate_path, "--fast-proof", fast_path,
        "--candidate-intent", intent_path, "--policy-root", policy_path,
        "--canonical-artifacts", metadata_path,
    ]
    for key, *_ in platforms:
        values += ["--build-record", f"{key}={records[key]}"]
    for key, *_ in platforms:
        values += ["--artifact-binding", f"{key}={bindings[key]}"]
    values += ["--output" if command == "create" else "--manifest", destination]
    return [str(value) for value in values]

def invoke(command, destination):
    return subprocess.run(arguments(command, destination), cwd=repo, capture_output=True, text=True)

def reject(name, expected):
    result = invoke("create", root / name)
    assert result.returncode != 0, f"mutation {name} was accepted"
    assert expected in result.stderr, result.stderr

first, second = root / "manifest.json", root / "manifest-2.json"
assert invoke("create", first).returncode == 0
assert invoke("create", second).returncode == 0
assert first.read_bytes() == second.read_bytes()
assert invoke("verify", first).returncode == 0
manifest = json.loads(first.read_text())
assert manifest["expires_at"] == "2026-07-21T00:00:00Z"
assert len(manifest["jobs"]) == 50 and len(manifest["artifacts"]) == 5
rendered = json.dumps(manifest)
assert "manifest_sha256" not in rendered and "candidate-manifest" not in rendered
if importlib.util.find_spec("jsonschema"):
    import jsonschema
    schema = json.loads((repo / ".github/release/schemas/candidate-manifest.schema.json").read_text())
    jsonschema.Draft202012Validator(schema).validate(manifest)

original_candidate = copy.deepcopy(candidate)
candidate["kind"] = "qualification"
candidate["proof_sha256"] = semantic_hash({k: v for k, v in candidate.items() if k != "proof_sha256"})
write("candidate.json", candidate)
reject("bad-kind.json", "identity changed")
candidate = copy.deepcopy(original_candidate)
candidate["jobs"][-1]["conclusion"] = "skipped"
candidate["proof_sha256"] = semantic_hash({k: v for k, v in candidate.items() if k != "proof_sha256"})
write("candidate.json", candidate)
reject("bad-gate.json", "successful final gate")
candidate = copy.deepcopy(original_candidate)
write("candidate.json", candidate)
original_fast = copy.deepcopy(fast)
fast["verified_at"] = "2026-07-19T00:05:00Z"
fast["proof_sha256"] = semantic_hash({k: v for k, v in fast.items() if k != "proof_sha256"})
write("fast.json", fast)
reject("bad-time.json", "timestamps are not ordered")
fast = original_fast
write("fast.json", fast)

key = platforms[0][0]
record = json.loads(records[key].read_text())
original_record = copy.deepcopy(record)
record["runner"]["image"] = "self-hosted"
write(f"{key}-record.json", record)
reject("bad-runner.json", "runner identity mismatch")
record = copy.deepcopy(original_record)
record["platform"]["target_triple"] = "aarch64-unknown-linux-gnu"
write(f"{key}-record.json", record)
reject("bad-target.json", "target triple mismatch")
write(f"{key}-record.json", original_record)
binding = json.loads(bindings[key].read_text())
original_binding = copy.deepcopy(binding)
binding["assets"]["artifact_digest"] = f"sha256:{99:064x}"
write(f"{key}-binding.json", binding)
reject("bad-binding.json", "exact asset artifact")
write(f"{key}-binding.json", original_binding)
metadata = json.loads(metadata_path.read_text())
metadata["source_artifacts"][1]["archive_digest"] = f"sha256:{98:064x}"
metadata["canonical_platforms"][0]["assets_artifact"]["archive_digest"] = f"sha256:{98:064x}"
write("metadata.json", metadata)
reject("bad-digest.json", "exact asset artifact")
"#;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn candidate_manifest_round_trip_schema_and_adversarial_contract() {
    let manifest = include_str!("../scripts/release/candidate-manifest.py");
    let evidence = include_str!("../scripts/release/candidate_manifest_evidence.py");
    assert!(manifest.lines().count() < 600);
    assert!(evidence.lines().count() < 600);
    assert!(manifest.contains("from candidate_manifest_evidence import ("));

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rmux-candidate-manifest-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&root).expect("create candidate manifest fixture");
    let script = root.join("fixture.py");
    fs::write(&script, FIXTURE).expect("write candidate manifest fixture program");
    let output = Command::new("python3")
        .args([
            script.as_os_str(),
            repo_root().as_os_str(),
            root.as_os_str(),
        ])
        .output()
        .expect("run candidate manifest fixture");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(root).expect("remove candidate manifest fixture");
}
