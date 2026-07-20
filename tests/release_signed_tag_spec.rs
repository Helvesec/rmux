#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Command, Output};
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
const SOURCE_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
#[cfg(unix)]
const MANIFEST_DIGEST: &str =
    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
#[cfg(unix)]
const MANIFEST_SHA: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
#[cfg(unix)]
const POLICY_ROOT: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

#[cfg(unix)]
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[cfg(unix)]
fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "rmux-signed-tag-{label}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("create fixture directory");
    path
}

#[cfg(unix)]
fn run(command: &mut Command) -> Output {
    command.output().expect("run fixture command")
}

#[cfg(unix)]
fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    let mut permissions = fs::metadata(path).expect("script metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make script executable");
}

#[cfg(unix)]
struct Fixture {
    root: PathBuf,
    repository: PathBuf,
    scripts: PathBuf,
    signing_key: PathBuf,
    source_sha: String,
}

#[cfg(unix)]
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[cfg(unix)]
fn activated_fixture(label: &str) -> Fixture {
    let root = temp_dir(label);
    let repository = root.join("repository");
    let scripts = repository.join("scripts/release");
    let policy_dir = repository.join(".github/release");
    fs::create_dir_all(&scripts).expect("create fixture scripts");
    fs::create_dir_all(&policy_dir).expect("create fixture policy");

    for name in [
        "release_evidence.py",
        "release_tag_policy.py",
        "release-tag-message.py",
        "signed-tag-proof.py",
        "verify-release-tag.py",
        "sign-and-push-release-tag.sh",
    ] {
        let source = repo_root().join("scripts/release").join(name);
        let destination = scripts.join(name);
        fs::copy(source, &destination).expect("copy release tag script");
        make_executable(&destination);
    }

    let signing_key = root.join("release-signing-key");
    let generated = run(Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&signing_key));
    assert!(generated.status.success(), "{}", stderr(&generated));
    let public_key = fs::read_to_string(signing_key.with_extension("pub"))
        .expect("read public key")
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    let fingerprint_output = run(Command::new("ssh-keygen")
        .args(["-l", "-f"])
        .arg(signing_key.with_extension("pub"))
        .args(["-E", "sha256"]));
    assert!(fingerprint_output.status.success());
    let fingerprint = String::from_utf8(fingerprint_output.stdout)
        .expect("fingerprint UTF-8")
        .split_whitespace()
        .nth(1)
        .expect("fingerprint field")
        .to_owned();
    let policy = serde_json::json!({
        "schema_version": 1,
        "status": "enabled",
        "repository": {"id": 1239918790, "full_name": "Helvesec/rmux"},
        "release_app": {
            "app_id": 4339867,
            "may_create_only": "refs/tags/v*",
            "force_updates_allowed": false
        },
        "tag_policy": {
            "signature_format": "ssh",
            "signature_namespace": "git",
            "ref_pattern": "^refs/tags/v[0-9]+\\.[0-9]+\\.[0-9]+(?:-rc\\.[0-9]+)?$",
            "required_private_key_secret": "RMUX_RELEASE_SSH_SIGNING_KEY",
            "enabled": true,
            "blocker": "",
            "allowed_signers": [{
                "principal": "rmux-release@rmux.io",
                "public_key": public_key,
                "fingerprint": fingerprint
            }]
        }
    });
    fs::write(
        policy_dir.join("release-signers.json"),
        serde_json::to_vec_pretty(&policy).expect("serialize signer policy"),
    )
    .expect("write signer policy");

    let initialized = run(Command::new("git").args(["init", "-q"]).arg(&repository));
    assert!(initialized.status.success(), "{}", stderr(&initialized));
    for args in [
        ["config", "user.name", "RMUX fixture"],
        ["config", "user.email", "fixture@rmux.invalid"],
    ] {
        let configured = run(Command::new("git").args(args).current_dir(&repository));
        assert!(configured.status.success(), "{}", stderr(&configured));
    }
    fs::write(repository.join("README"), "fixture\n").expect("write fixture file");
    let added = run(Command::new("git")
        .args(["add", "."])
        .current_dir(&repository));
    assert!(added.status.success());
    let committed = run(Command::new("git")
        .args(["commit", "-q", "-m", "fixture"])
        .current_dir(&repository));
    assert!(committed.status.success(), "{}", stderr(&committed));
    let resolved = run(Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&repository));
    assert!(resolved.status.success());
    let source_sha = String::from_utf8(resolved.stdout)
        .expect("source SHA UTF-8")
        .trim()
        .to_owned();
    Fixture {
        root,
        repository,
        scripts,
        signing_key,
        source_sha,
    }
}

#[cfg(unix)]
fn identity_arguments(source_sha: &str) -> Vec<String> {
    [
        ("--release-ref", "v0.9.1"),
        ("--release-intent-id", "release-20260719"),
        ("--release-kind", "stable"),
        ("--source-sha", source_sha),
        ("--candidate-run-id", "77"),
        ("--candidate-manifest-artifact-id", "88"),
        ("--candidate-manifest-artifact-digest", MANIFEST_DIGEST),
        ("--candidate-manifest-sha256", MANIFEST_SHA),
        ("--release-policy-root-sha256", POLICY_ROOT),
    ]
    .into_iter()
    .flat_map(|(key, value)| [key.to_owned(), value.to_owned()])
    .collect()
}

#[test]
#[cfg(unix)]
fn canonical_tag_message_rejects_duplicate_reordered_and_mismatched_trailers() {
    let fixture = activated_fixture("message");
    let script = fixture.scripts.join("release-tag-message.py");
    let message = fixture.root.join("message");
    let mut render_args = vec!["render".to_owned()];
    render_args.extend(identity_arguments(&fixture.source_sha));
    render_args.extend(["--output".to_owned(), message.display().to_string()]);
    let rendered = run(Command::new(&script).args(&render_args));
    assert!(rendered.status.success(), "{}", stderr(&rendered));
    let expected = format!(
        "RMUX release v0.9.1\n\n\
RMUX-Release-Intent-ID: release-20260719\n\
RMUX-Release-Kind: stable\n\
RMUX-Source-SHA: {}\n\
RMUX-Candidate-Run-ID: 77\n\
RMUX-Candidate-Manifest-Artifact-ID: 88\n\
RMUX-Candidate-Manifest-Artifact-Digest: {MANIFEST_DIGEST}\n\
RMUX-Candidate-Manifest-SHA256: {MANIFEST_SHA}\n\
RMUX-Release-Policy-Root-SHA256: {POLICY_ROOT}\n",
        fixture.source_sha
    );
    assert_eq!(
        fs::read_to_string(&message).expect("read message"),
        expected
    );

    let mut verify_args = vec!["verify".to_owned(), "--message".to_owned()];
    verify_args.push(message.display().to_string());
    verify_args.extend(identity_arguments(&fixture.source_sha));
    let verified = run(Command::new(&script).args(&verify_args));
    assert!(verified.status.success(), "{}", stderr(&verified));

    fs::write(&message, format!("{expected}RMUX-Candidate-Run-ID: 77\n"))
        .expect("write duplicate trailer");
    let duplicate = run(Command::new(&script).args(&verify_args));
    assert!(!duplicate.status.success());
    assert!(stderr(&duplicate).contains("canonical line count"));

    let reordered = expected.replace(
        "RMUX-Release-Intent-ID: release-20260719\nRMUX-Release-Kind: stable",
        "RMUX-Release-Kind: stable\nRMUX-Release-Intent-ID: release-20260719",
    );
    fs::write(&message, reordered).expect("write reordered trailers");
    let wrong_order = run(Command::new(&script).args(&verify_args));
    assert!(!wrong_order.status.success());
    assert!(stderr(&wrong_order).contains("expected unique canonical trailer"));

    let mut rc_args = render_args.clone();
    let kind_index = rc_args
        .iter()
        .position(|value| value == "--release-kind")
        .expect("kind argument");
    rc_args[kind_index + 1] = "rc".to_owned();
    let mismatch = run(Command::new(&script).args(&rc_args));
    assert!(!mismatch.status.success());
    assert!(stderr(&mismatch).contains("does not match v0.9.1"));

    let mut uppercase_args = render_args;
    let source_index = uppercase_args
        .iter()
        .position(|value| value == "--source-sha")
        .expect("source SHA argument");
    uppercase_args[source_index + 1] = fixture.source_sha.to_uppercase();
    let noncanonical = run(Command::new(&script).args(&uppercase_args));
    assert!(!noncanonical.status.success());
    assert!(stderr(&noncanonical).contains("lowercase full SHA-1"));
}

#[cfg(unix)]
fn create_signed_tag(fixture: &Fixture, message: &Path) {
    for args in [
        vec!["config", "gpg.format", "ssh"],
        vec![
            "config",
            "user.signingkey",
            fixture.signing_key.to_str().expect("key path"),
        ],
    ] {
        let output = run(Command::new("git")
            .args(args)
            .current_dir(&fixture.repository));
        assert!(output.status.success(), "{}", stderr(&output));
    }
    let signed = run(Command::new("git")
        .args(["tag", "--annotate", "--sign", "--file"])
        .arg(message)
        .args(["v0.9.1", &fixture.source_sha])
        .current_dir(&fixture.repository));
    assert!(signed.status.success(), "{}", stderr(&signed));
}

#[cfg(unix)]
fn write_github_tag_fixture(fixture: &Fixture, message: &Path) -> (PathBuf, PathBuf) {
    let resolved = run(Command::new("git")
        .args(["rev-parse", "refs/tags/v0.9.1^{tag}"])
        .current_dir(&fixture.repository));
    assert!(resolved.status.success(), "{}", stderr(&resolved));
    let tag_sha = String::from_utf8(resolved.stdout)
        .expect("tag SHA UTF-8")
        .trim()
        .to_owned();
    let raw = run(Command::new("git")
        .args(["cat-file", "tag", &tag_sha])
        .current_dir(&fixture.repository));
    assert!(raw.status.success(), "{}", stderr(&raw));
    let marker = b"-----BEGIN SSH SIGNATURE-----\n";
    let marker_index = raw
        .stdout
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("SSH signature marker");
    let payload =
        String::from_utf8(raw.stdout[..marker_index].to_vec()).expect("signature payload UTF-8");
    let signature =
        String::from_utf8(raw.stdout[marker_index..].to_vec()).expect("signature envelope UTF-8");
    let ref_json = fixture.root.join("ref.json");
    let tag_json = fixture.root.join("tag.json");
    fs::write(
        &ref_json,
        serde_json::to_vec(&serde_json::json!({
            "ref": "refs/tags/v0.9.1",
            "object": {"type": "tag", "sha": tag_sha, "url": "https://example.invalid/ref"}
        }))
        .expect("serialize ref JSON"),
    )
    .expect("write ref JSON");
    fs::write(
        &tag_json,
        serde_json::to_vec(&serde_json::json!({
            "sha": tag_sha,
            "tag": "v0.9.1",
            "message": fs::read_to_string(message).expect("read canonical message"),
            "object": {
                "type": "commit",
                "sha": fixture.source_sha,
                "url": "https://example.invalid/commit"
            },
            "verification": {
                "verified": true,
                "reason": "valid",
                "payload": payload,
                "signature": signature
            }
        }))
        .expect("serialize tag JSON"),
    )
    .expect("write tag JSON");
    (ref_json, tag_json)
}

#[test]
#[cfg(unix)]
fn verifier_requires_exact_allowlisted_ssh_signature_and_identity() {
    let fixture = activated_fixture("verification");
    let message_script = fixture.scripts.join("release-tag-message.py");
    let verifier = fixture.scripts.join("verify-release-tag.py");
    let message = fixture.root.join("message");
    let mut render_args = vec!["render".to_owned()];
    render_args.extend(identity_arguments(&fixture.source_sha));
    render_args.extend(["--output".to_owned(), message.display().to_string()]);
    let rendered = run(Command::new(message_script).args(render_args));
    assert!(rendered.status.success(), "{}", stderr(&rendered));
    create_signed_tag(&fixture, &message);

    let mut verify_args = vec![
        "local".to_owned(),
        "--repository".to_owned(),
        fixture.repository.display().to_string(),
    ];
    verify_args.extend(identity_arguments(&fixture.source_sha));
    let valid = run(Command::new(&verifier).args(&verify_args));
    assert!(valid.status.success(), "{}", stderr(&valid));
    let result: serde_json::Value =
        serde_json::from_slice(&valid.stdout).expect("verification JSON");
    assert_eq!(result["signature_format"], "ssh");
    assert_eq!(result["signer_principal"], "rmux-release@rmux.io");
    assert!(result["key_fingerprint"]
        .as_str()
        .expect("key fingerprint")
        .starts_with("SHA256:"));
    assert_eq!(result["source_git_sha"], fixture.source_sha);

    let run_index = verify_args
        .iter()
        .position(|value| value == "--candidate-run-id")
        .expect("candidate run argument");
    let original = verify_args[run_index + 1].clone();
    verify_args[run_index + 1] = "78".to_owned();
    let drift = run(Command::new(&verifier).args(&verify_args));
    assert!(!drift.status.success());
    assert!(stderr(&drift).contains("differs from the expected release"));
    verify_args[run_index + 1] = original;

    let lightweight = run(Command::new("git")
        .args(["tag", "v0.9.2", &fixture.source_sha])
        .current_dir(&fixture.repository));
    assert!(lightweight.status.success());
    let ref_index = verify_args
        .iter()
        .position(|value| value == "--release-ref")
        .expect("release ref argument");
    verify_args[ref_index + 1] = "v0.9.2".to_owned();
    let not_annotated = run(Command::new(&verifier).args(&verify_args));
    assert!(!not_annotated.status.success());
    assert!(stderr(&not_annotated).contains("annotated tag object"));
}

#[test]
#[cfg(unix)]
fn github_idempotence_proof_binds_ref_object_payload_and_signature() {
    let fixture = activated_fixture("github-json");
    let message = fixture.root.join("message");
    let mut render_args = vec!["render".to_owned()];
    render_args.extend(identity_arguments(&fixture.source_sha));
    render_args.extend(["--output".to_owned(), message.display().to_string()]);
    let rendered =
        run(Command::new(fixture.scripts.join("release-tag-message.py")).args(render_args));
    assert!(rendered.status.success(), "{}", stderr(&rendered));
    create_signed_tag(&fixture, &message);
    let (ref_json, tag_json) = write_github_tag_fixture(&fixture, &message);
    let mut verify_args = vec![
        "github-json".to_owned(),
        "--ref-json".to_owned(),
        ref_json.display().to_string(),
        "--tag-json".to_owned(),
        tag_json.display().to_string(),
    ];
    verify_args.extend(identity_arguments(&fixture.source_sha));
    let verifier = fixture.scripts.join("verify-release-tag.py");
    let valid = run(Command::new(&verifier).args(&verify_args));
    assert!(valid.status.success(), "{}", stderr(&valid));
    let verification_json = fixture.root.join("verification.json");
    fs::write(&verification_json, &valid.stdout).expect("write verification JSON");
    let signed_tag_proof = fixture.root.join("signed-tag-proof.json");
    let proof = run(
        Command::new(fixture.scripts.join("signed-tag-proof.py")).args([
            "create",
            "--verification",
            verification_json.to_str().expect("verification path"),
            "--verified-at",
            "2026-07-19T12:00:00Z",
            "--output",
            signed_tag_proof.to_str().expect("proof path"),
        ]),
    );
    assert!(proof.status.success(), "{}", stderr(&proof));
    let proof_json: serde_json::Value =
        serde_json::from_slice(&fs::read(&signed_tag_proof).expect("read tag proof"))
            .expect("tag proof JSON");
    assert_eq!(proof_json["status"], "verified-signed-annotated-tag");
    assert_eq!(proof_json["release_intent_id"], "release-20260719");
    assert_eq!(proof_json["candidate_run_id"], 77);
    assert_eq!(proof_json["candidate_manifest_artifact_id"], 88);
    assert_eq!(
        proof_json["candidate_manifest_artifact_digest"],
        MANIFEST_DIGEST
    );
    assert_eq!(proof_json["candidate_manifest_sha256"], MANIFEST_SHA);
    assert_eq!(proof_json["release_policy_root_sha256"], POLICY_ROOT);
    assert_eq!(
        proof_json["signature"]["key_fingerprint"],
        serde_json::from_slice::<serde_json::Value>(&valid.stdout).expect("verification JSON")
            ["key_fingerprint"]
    );

    let fake_bin = fixture.root.join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("create fake bin");
    let fake_gh = fake_bin.join("gh");
    fs::write(
        &fake_gh,
        r#"#!/usr/bin/env bash
set -euo pipefail
endpoint=''
for argument in "$@"; do endpoint=$argument; done
case "$endpoint" in
  */git/ref/tags/v0.9.1) body=$FAKE_REF_JSON ;;
  */git/tags/*) body=$FAKE_TAG_JSON ;;
  *) echo "unexpected gh endpoint: $endpoint" >&2; exit 2 ;;
esac
printf 'HTTP/2.0 200 OK\nContent-Type: application/json\n\n'
cat "$body"
"#,
    )
    .expect("write fake gh");
    make_executable(&fake_gh);
    let mut driver_args = vec![
        "--repository".to_owned(),
        "Helvesec/rmux".to_owned(),
        "--repository-root".to_owned(),
        fixture.repository.display().to_string(),
    ];
    driver_args.extend(identity_arguments(&fixture.source_sha));
    driver_args.extend([
        "--signing-key".to_owned(),
        fixture.signing_key.display().to_string(),
    ]);
    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").expect("PATH")
    );
    let idempotent = run(
        Command::new(fixture.scripts.join("sign-and-push-release-tag.sh"))
            .args(driver_args)
            .env("PATH", path)
            .env("FAKE_REF_JSON", &ref_json)
            .env("FAKE_TAG_JSON", &tag_json)
            .env("RMUX_RELEASE_APP_ID", "4339867")
            .env("RMUX_RELEASE_APP_TOKEN", "fixture-installation-token"),
    );
    assert!(idempotent.status.success(), "{}", stderr(&idempotent));
    let idempotent_json: serde_json::Value =
        serde_json::from_slice(&idempotent.stdout).expect("idempotent JSON");
    assert_eq!(idempotent_json["mode"], "idempotent-existing");

    let mut tag_value: serde_json::Value =
        serde_json::from_slice(&fs::read(&tag_json).expect("read tag JSON"))
            .expect("parse tag JSON");
    tag_value["verification"]["payload"] = serde_json::Value::String(
        tag_value["verification"]["payload"]
            .as_str()
            .expect("payload")
            .replace("RMUX-Candidate-Run-ID: 77", "RMUX-Candidate-Run-ID: 78"),
    );
    fs::write(
        &tag_json,
        serde_json::to_vec(&tag_value).expect("serialize mutated tag JSON"),
    )
    .expect("write mutated tag JSON");
    let forged = run(Command::new(&verifier).args(&verify_args));
    assert!(!forged.status.success());
    assert!(
        stderr(&forged).contains("tag object hash mismatch")
            || stderr(&forged).contains("tag signature")
    );
}

#[test]
#[cfg(unix)]
fn tag_driver_is_local_only_in_dry_run_and_fails_without_app_authority() {
    let fixture = activated_fixture("driver");
    let driver = fixture.scripts.join("sign-and-push-release-tag.sh");
    let mut args = vec![
        "--repository".to_owned(),
        "Helvesec/rmux".to_owned(),
        "--repository-root".to_owned(),
        fixture.repository.display().to_string(),
    ];
    args.extend(identity_arguments(&fixture.source_sha));
    args.extend([
        "--signing-key".to_owned(),
        fixture.signing_key.display().to_string(),
        "--dry-run".to_owned(),
    ]);
    let dry_run = run(Command::new(&driver).args(&args));
    assert!(dry_run.status.success(), "{}", stderr(&dry_run));
    let result: serde_json::Value = serde_json::from_slice(&dry_run.stdout).expect("dry-run JSON");
    assert_eq!(result["mode"], "dry-run");
    assert_eq!(result["verification"]["source_git_sha"], fixture.source_sha);
    let source_ref = run(Command::new("git")
        .args(["show-ref", "--verify", "refs/tags/v0.9.1"])
        .current_dir(&fixture.repository));
    assert!(
        !source_ref.status.success(),
        "dry-run must not create a tag in the source repository"
    );

    args.retain(|value| value != "--dry-run");
    let no_app = run(Command::new(&driver).args(&args));
    assert!(!no_app.status.success());
    assert!(stderr(&no_app).contains("RMUX_RELEASE_APP_ID"));

    let wrong_repository = args
        .iter()
        .position(|value| value == "Helvesec/rmux")
        .expect("repository value");
    args[wrong_repository] = "attacker/fork".to_owned();
    let fork = run(Command::new(&driver).args(&args));
    assert!(!fork.status.success());
    assert!(stderr(&fork).contains("repository must be Helvesec/rmux"));

    args[wrong_repository] = "Helvesec/rmux".to_owned();
    let fake_bin = fixture.root.join("create-bin");
    fs::create_dir_all(&fake_bin).expect("create fake create bin");
    let ref_json = fixture.root.join("created-ref.json");
    let tag_json = fixture.root.join("created-tag.json");
    let fake_gh = fake_bin.join("gh");
    fs::write(
        &fake_gh,
        r#"#!/usr/bin/env bash
set -euo pipefail
endpoint=''
for argument in "$@"; do endpoint=$argument; done
case "$endpoint" in
  */git/ref/tags/v0.9.1) body=$FAKE_REF_JSON ;;
  */git/tags/*) body=$FAKE_TAG_JSON ;;
  *) echo "unexpected gh endpoint: $endpoint" >&2; exit 2 ;;
esac
if [[ ! -f $body ]]; then
  printf 'HTTP/2.0 404 Not Found\nContent-Type: application/json\n\n{}\n'
  exit 1
fi
printf 'HTTP/2.0 200 OK\nContent-Type: application/json\n\n'
cat "$body"
"#,
    )
    .expect("write fake gh");
    make_executable(&fake_gh);
    let fake_git = fake_bin.join("git");
    fs::write(
        &fake_git,
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1-} == -C && ${3-} == push ]]; then
  repository=$2
  tag_sha=$($REAL_GIT -C "$repository" rev-parse refs/tags/v0.9.1^{tag})
  raw=$FAKE_TAG_JSON.raw
  $REAL_GIT -C "$repository" cat-file tag "$tag_sha" >"$raw"
  python3 - "$raw" "$tag_sha" "$FAKE_REF_JSON" "$FAKE_TAG_JSON" "$FAKE_GITHUB_VERIFIED" <<'PY'
import json
from pathlib import Path
import sys
raw = Path(sys.argv[1]).read_bytes()
marker = b"-----BEGIN SSH SIGNATURE-----\n"
offset = raw.index(marker)
payload = raw[:offset].decode()
signature = raw[offset:].decode()
headers, message = payload.split("\n\n", 1)
header = dict(line.split(" ", 1) for line in headers.splitlines()[:3])
ref = {"ref": "refs/tags/v0.9.1", "object": {"type": "tag", "sha": sys.argv[2]}}
tag = {
    "sha": sys.argv[2], "tag": "v0.9.1", "message": message,
    "object": {"type": "commit", "sha": header["object"]},
    "verification": {
        "verified": sys.argv[5] == "true", "reason": "valid" if sys.argv[5] == "true" else "unknown_key",
        "payload": payload, "signature": signature,
    },
}
Path(sys.argv[3]).write_text(json.dumps(ref), encoding="utf-8")
Path(sys.argv[4]).write_text(json.dumps(tag), encoding="utf-8")
PY
  exit 0
fi
exec "$REAL_GIT" "$@"
"#,
    )
    .expect("write fake git");
    make_executable(&fake_git);
    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").expect("PATH")
    );
    let real_git = String::from_utf8(run(Command::new("sh").args(["-c", "command -v git"])).stdout)
        .expect("git path UTF-8")
        .trim()
        .to_owned();
    let invoke_created = |verified: &str| {
        run(Command::new(&driver)
            .args(&args)
            .env("PATH", &path)
            .env("REAL_GIT", &real_git)
            .env("FAKE_REF_JSON", &ref_json)
            .env("FAKE_TAG_JSON", &tag_json)
            .env("FAKE_GITHUB_VERIFIED", verified)
            .env("RMUX_RELEASE_APP_ID", "4339867")
            .env("RMUX_RELEASE_APP_TOKEN", "fixture-installation-token"))
    };
    let unverified = invoke_created("false");
    assert!(!unverified.status.success());
    assert!(stderr(&unverified).contains("GitHub did not verify the tag signature"));
    fs::remove_file(&ref_json).expect("reset fake ref");
    fs::remove_file(&tag_json).expect("reset fake tag");
    let created = invoke_created("true");
    assert!(created.status.success(), "{}", stderr(&created));
    let created_json: serde_json::Value =
        serde_json::from_slice(&created.stdout).expect("created JSON");
    assert_eq!(created_json["mode"], "created");
    assert_eq!(created_json["local_verification"]["mode"], "verified");
    assert_eq!(
        created_json["github_verification"]["mode"],
        "github-json-verified"
    );
    assert_eq!(
        created_json["tag_object_sha"],
        created_json["github_verification"]["tag_object_sha"]
    );
}

#[test]
#[cfg(unix)]
fn repository_policy_blocks_even_a_local_dry_run_until_key_is_configured() {
    let key_root = temp_dir("disabled-key");
    let key = key_root.join("key");
    let generated = run(Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key));
    assert!(generated.status.success());
    let script = repo_root().join("scripts/release/sign-and-push-release-tag.sh");
    let mut args = vec![
        "--repository".to_owned(),
        "Helvesec/rmux".to_owned(),
        "--repository-root".to_owned(),
        repo_root().display().to_string(),
    ];
    args.extend(identity_arguments(SOURCE_SHA));
    args.extend([
        "--signing-key".to_owned(),
        key.display().to_string(),
        "--dry-run".to_owned(),
    ]);
    let blocked = run(Command::new(script).args(args));
    assert!(!blocked.status.success());
    assert!(stderr(&blocked).contains("dedicated_release_ssh_signing_key_not_configured"));
    fs::remove_dir_all(key_root).expect("remove key fixture");
}
