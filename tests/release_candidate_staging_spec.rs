use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn temp_dir() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rmux-candidate-staging-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&root).expect("create staging fixture");
    root
}

const FIXTURE: &str = r#"
import hashlib
import json
import os
import pathlib
import subprocess
import sys

repo, root = map(pathlib.Path, sys.argv[1:])
script = repo / "scripts/release/stage-candidate-release.py"
source = "a" * 40
run_id = 77
downloads = root / "downloads"
downloads.mkdir()

def digest(raw):
    return hashlib.sha256(raw).hexdigest()

def artifact(role, platform, number):
    prefix = "rmux-canonical-provenance" if role == "canonical-provenance" else "rmux-canonical"
    name = f"{prefix}-{platform}-{source}"
    return {
        "role": role,
        "platform_key": platform,
        "artifact_id": number,
        "name": name,
        "archive_digest": "sha256:" + f"{number:064x}",
        "size_in_bytes": 1000 + number,
    }

platforms = [
    "linux-aarch64", "linux-x86_64", "macos-aarch64",
    "macos-x86_64", "windows-x86_64",
]
resolved = [{
    "role": "fast-proof", "platform_key": None, "artifact_id": 1,
    "name": f"rmux-fast-proof-{source}", "archive_digest": "sha256:" + "01" * 32,
    "size_in_bytes": 100,
}]
manifest_platforms = []
next_id = 10
for platform in platforms:
    assets = artifact("canonical-assets", platform, next_id)
    provenance = artifact("canonical-provenance", platform, next_id + 1)
    next_id += 2
    resolved.extend([assets, provenance])
    root_assets = downloads / assets["name"] / "assets"
    root_assets.mkdir(parents=True)
    roles = ["archive", "checksums"]
    if platform == "linux-x86_64":
        roles.extend(["crate-package-set", "debian", "rpm", "snap-amd64", "wasm-byte-set", "wasm-provenance"])
    elif platform == "linux-aarch64":
        roles.extend(["debian", "rpm", "snap-arm64"])
    elif platform == "windows-x86_64":
        roles.append("chocolatey-package")
    suffixes = {
        "archive": ".tar.gz", "checksums": ".txt", "chocolatey-package": ".nupkg",
        "crate-package-set": ".tar", "debian": ".deb", "rpm": ".rpm",
        "snap-amd64": ".snap", "snap-arm64": ".snap", "wasm-byte-set": ".tar",
        "wasm-provenance": ".json",
    }
    public_roles = {"archive", "debian", "rpm", "snap-amd64", "snap-arm64"}
    files = []
    for role in roles:
        suffix = suffixes[role]
        name = "SHA256SUMS.txt" if role == "checksums" else f"rmux-{platform}-{role}{suffix}"
        raw = f"{platform}:{role}\n".encode()
        (root_assets / name).write_bytes(raw)
        files.append({"path": name, "role": role, "size": len(raw), "sha256": digest(raw)})
    manifest_platforms.append({"platform_key": platform, "assets": assets, "provenance": provenance, "files": files})

manifest = {
    "schema_version": 1, "repository_id": 1239918790,
    "source_git_sha": source, "fast_run_id": 55,
    "candidate_run_id": run_id, "candidate_run_attempt": 1,
    "release_intent_id": "release:staging:test", "planned_release_ref": "v1.0.0",
    "release_kind": "stable", "release_version": "1.0.0", "is_prerelease": False,
    "release_policy": {"sha256": "b" * 64},
    "created_at": "2026-07-19T00:00:00Z", "expires_at": "2026-07-21T00:00:00Z",
    "artifacts": manifest_platforms,
}
resolution = {
    "schema_version": 1, "repository_id": 1239918790,
    "candidate_run": {"id": run_id, "attempt": 1, "head_sha": source, "status": "completed", "conclusion": "success"},
    "expected_artifact_count": 11, "artifacts": resolved,
}
manifest_path = root / "manifest.json"
resolution_path = root / "resolution.json"
manifest_path.write_text(json.dumps(manifest))
resolution_path.write_text(json.dumps(resolution))

def invoke(output, downloads_dir=downloads):
    return subprocess.run([
        sys.executable, script, "--manifest", manifest_path,
        "--resolution", resolution_path, "--downloads-dir", downloads_dir,
        "--candidate-run-id", str(run_id), "--source-sha", source,
        "--output", output,
    ], cwd=repo, capture_output=True, text=True)

staged = root / "staged"
accepted = invoke(staged)
assert accepted.returncode == 0, accepted.stderr
sums = (staged / "SHA256SUMS").read_text().splitlines()
assert len(sums) == 11
assert all("SHA256SUMS.txt" not in line for line in sums)
assert not any("crate-package-set" in line or "wasm" in line or ".nupkg" in line for line in sums)

if os.name != "nt":
    downloads_link = root / "downloads-link"
    downloads_link.symlink_to(downloads, target_is_directory=True)
    rejected = invoke(root / "symlink-downloads", downloads_link)
    assert rejected.returncode != 0
    assert "candidate downloads must be one real directory" in rejected.stderr

original = resolution["artifacts"][1]["archive_digest"]
resolution["artifacts"][1]["archive_digest"] = "sha256:" + "f" * 64
resolution_path.write_text(json.dumps(resolution))
rejected = invoke(root / "forged-resolution")
assert rejected.returncode != 0
assert "does not bind the live canonical-assets archive" in rejected.stderr
resolution["artifacts"][1]["archive_digest"] = original
resolution_path.write_text(json.dumps(resolution))

first = manifest_platforms[0]
asset = downloads / first["assets"]["name"] / "assets" / first["files"][0]["path"]
asset.write_bytes(b"mutated\n")
rejected = invoke(root / "mutated-bytes")
assert rejected.returncode != 0
assert "candidate asset bytes differ" in rejected.stderr
"#;

#[test]
fn staging_rebinds_live_archives_and_original_bytes() {
    let root = temp_dir();
    let output = Command::new("python3")
        .args(["-c", FIXTURE])
        .arg(repo_root())
        .arg(&root)
        .output()
        .expect("run staging fixture");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(root).expect("remove staging fixture");
}
