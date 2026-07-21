#![cfg(unix)]

use serde_json::{json, Value};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const SOURCE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TAG_OBJECT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

struct Fixture {
    root: PathBuf,
    manifest: PathBuf,
    candidate: PathBuf,
    tag: PathBuf,
    audit: PathBuf,
    sums: PathBuf,
    authorization: PathBuf,
    authorization_envelope: PathBuf,
    release_state: PathBuf,
    receipt: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove release evidence fixture");
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn temp_dir() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "rmux-release-promotion-evidence-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("create fixture directory");
    path
}

fn hex(value: u64) -> String {
    format!("{value:064x}")
}

fn write_json(path: &Path, value: &Value) {
    let mut encoded = serde_json::to_string_pretty(value).expect("encode fixture");
    encoded.push('\n');
    fs::write(path, encoded).expect("write fixture JSON");
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("read fixture JSON"))
        .expect("decode fixture JSON")
}

#[test]
fn publication_receipt_modules_stay_below_the_release_file_budget() {
    for filename in [
        "publication-receipt.py",
        "publication_authorization.py",
        "publication_release_state.py",
    ] {
        let path = repo_root().join("scripts/release").join(filename);
        let source = fs::read_to_string(&path).expect("read publication receipt module");
        assert!(
            source.lines().count() < 600,
            "{filename} exceeds the release helper size budget"
        );
    }
}

fn validate_schema(schema: &str, document: &Path) {
    let program = r#"
import importlib.util, json, sys
if importlib.util.find_spec("jsonschema"):
    import jsonschema
    schema = json.load(open(sys.argv[1], encoding="utf-8"))
    document = json.load(open(sys.argv[2], encoding="utf-8"))
    jsonschema.Draft202012Validator.check_schema(schema)
    jsonschema.Draft202012Validator(schema).validate(document)
"#;
    let output = Command::new("python3")
        .args([
            "-c",
            program,
            repo_root()
                .join(".github/release/schemas")
                .join(schema)
                .to_str()
                .expect("schema path"),
            document.to_str().expect("document path"),
        ])
        .output()
        .expect("validate JSON Schema");
    assert_success(output);
}

fn invoke(script: &str, arguments: &[OsString]) -> Output {
    Command::new("python3")
        .arg(repo_root().join("scripts/release").join(script))
        .args(arguments)
        .current_dir(repo_root())
        .output()
        .unwrap_or_else(|error| panic!("run {script}: {error}"))
}

fn assert_success(output: Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_rejected(output: Output, expected: &str) {
    assert!(!output.status.success(), "mutation unexpectedly succeeded");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(expected),
        "missing {expected:?} in:\n{stderr}"
    );
}

fn platform(key: &str, files: Vec<Value>) -> Value {
    json!({"platform_key": key, "files": files})
}

fn asset(name: &str, role: &str, size: u64, digest: u64) -> Value {
    json!({"path": name, "role": role, "size": size, "sha256": hex(digest)})
}

fn auth_predicate_args(fixture: &Fixture, output_flag: &str, output: &Path) -> Vec<OsString> {
    [
        "create-predicate".into(),
        "--candidate-manifest".into(),
        fixture.manifest.as_os_str().into(),
        "--candidate-reference".into(),
        fixture.candidate.as_os_str().into(),
        "--signed-tag".into(),
        fixture.tag.as_os_str().into(),
        "--policy-audit-reference".into(),
        fixture.audit.as_os_str().into(),
        "--sha256sums".into(),
        fixture.sums.as_os_str().into(),
        "--authorization-run-id".into(),
        "700".into(),
        "--authorization-workflow-id".into(),
        "316435346".into(),
        "--issued-at".into(),
        "2026-07-19T00:31:00Z".into(),
        "--expires-at".into(),
        "2026-07-19T00:40:00Z".into(),
        output_flag.into(),
        output.as_os_str().into(),
    ]
    .into_iter()
    .collect()
}

fn auth_verify_args(fixture: &Fixture, document: &Path) -> Vec<OsString> {
    let mut values = auth_predicate_args(fixture, "--output", document);
    values[0] = "verify-predicate".into();
    let index = values.len() - 2;
    values[index] = "--document".into();
    values
}

fn auth_envelope_args(fixture: &Fixture, output_flag: &str, output: &Path) -> Vec<OsString> {
    let bundle = fixture.root.join("SHA256SUMS.sigstore.json");
    [
        "create-envelope".into(),
        "--predicate".into(),
        fixture.authorization.as_os_str().into(),
        "--attestation-id".into(),
        "authorization-attestation-1".into(),
        "--attestation-bundle".into(),
        bundle.as_os_str().into(),
        "--bundle-artifact-id".into(),
        "800".into(),
        "--bundle-artifact-name".into(),
        format!("rmux-promotion-authorization-{SOURCE}").into(),
        "--bundle-artifact-digest".into(),
        format!("sha256:{}", hex(80)).into(),
        "--bundle-artifact-size".into(),
        "4096".into(),
        "--created-at".into(),
        "2026-07-19T00:32:00Z".into(),
        output_flag.into(),
        output.as_os_str().into(),
    ]
    .into_iter()
    .collect()
}

fn receipt_predicate_args(fixture: &Fixture, output_flag: &str, output: &Path) -> Vec<OsString> {
    [
        "create-predicate".into(),
        "--authorization-predicate".into(),
        fixture.authorization.as_os_str().into(),
        "--authorization-envelope".into(),
        fixture.authorization_envelope.as_os_str().into(),
        "--release-state".into(),
        fixture.release_state.as_os_str().into(),
        "--receipt-run-id".into(),
        "900".into(),
        "--receipt-workflow-id".into(),
        "316435347".into(),
        "--verified-at".into(),
        "2026-07-19T00:34:00Z".into(),
        output_flag.into(),
        output.as_os_str().into(),
    ]
    .into_iter()
    .collect()
}

fn receipt_verify_args(fixture: &Fixture, document: &Path) -> Vec<OsString> {
    let mut values = receipt_predicate_args(fixture, "--output", document);
    values[0] = "verify-predicate".into();
    let index = values.len() - 2;
    values[index] = "--document".into();
    values
}

fn as_verify(mut values: Vec<OsString>) -> Vec<OsString> {
    values[0] = if values[0] == "create-envelope" {
        "verify-envelope".into()
    } else {
        "verify-predicate".into()
    };
    let index = values.len() - 2;
    values[index] = "--document".into();
    values
}

fn receipt_envelope_args(fixture: &Fixture, output_flag: &str, output: &Path) -> Vec<OsString> {
    let bundle = fixture.root.join("publication-receipt.sigstore.json");
    [
        "create-envelope".into(),
        "--predicate".into(),
        fixture.receipt.as_os_str().into(),
        "--attestation-id".into(),
        "receipt-attestation-1".into(),
        "--attestation-bundle".into(),
        bundle.as_os_str().into(),
        "--bundle-artifact-id".into(),
        "950".into(),
        "--bundle-artifact-name".into(),
        format!("rmux-publication-receipt-{SOURCE}-1234").into(),
        "--bundle-artifact-digest".into(),
        format!("sha256:{}", hex(95)).into(),
        "--bundle-artifact-size".into(),
        "2048".into(),
        "--created-at".into(),
        "2026-07-19T00:35:00Z".into(),
        output_flag.into(),
        output.as_os_str().into(),
    ]
    .into_iter()
    .collect()
}

fn fixture() -> Fixture {
    let root = temp_dir();
    let manifest = root.join("candidate-manifest.json");
    let candidate = root.join("candidate-reference.json");
    let tag = root.join("signed-tag.json");
    let audit = root.join("policy-audit-reference.json");
    let sums = root.join("SHA256SUMS");
    let authorization = root.join("promotion-authorization.json");
    let authorization_envelope = root.join("promotion-authorization-envelope.json");
    let release_state = root.join("release-state.json");
    let receipt = root.join("publication-receipt.json");
    let artifacts = vec![
        platform(
            "linux-aarch64",
            vec![
                asset("rmux-v1-linux-aarch64.tar.gz", "archive", 101, 1),
                asset("rmux-v1-linux-aarch64.deb", "debian", 102, 2),
                asset("rmux-v1-linux-aarch64.rpm", "rpm", 103, 3),
                asset("rmux-1.0.0-snap-arm64.snap", "snap-arm64", 115, 15),
                asset("SHA256SUMS.txt", "checksums", 104, 4),
            ],
        ),
        platform(
            "linux-x86_64",
            vec![
                asset("rmux-v1-linux-x86_64.tar.gz", "archive", 105, 5),
                asset("rmux-v1-linux-x86_64.deb", "debian", 106, 6),
                asset("rmux-v1-linux-x86_64.rpm", "rpm", 107, 7),
                asset(
                    "rmux-1.0.0-crate-package-set.tar",
                    "crate-package-set",
                    116,
                    16,
                ),
                asset("rmux-1.0.0-snap-amd64.snap", "snap-amd64", 117, 17),
                asset("rmux-web-crypto-wasm-1.0.0.tar", "wasm-byte-set", 118, 18),
                asset(
                    "rmux-web-crypto-wasm-1.0.0.provenance.json",
                    "wasm-provenance",
                    119,
                    19,
                ),
                asset("SHA256SUMS.txt", "checksums", 108, 8),
            ],
        ),
        platform(
            "macos-aarch64",
            vec![
                asset("rmux-v1-macos-aarch64.tar.gz", "archive", 109, 9),
                asset("SHA256SUMS.txt", "checksums", 110, 10),
            ],
        ),
        platform(
            "macos-x86_64",
            vec![
                asset("rmux-v1-macos-x86_64.tar.gz", "archive", 111, 11),
                asset("SHA256SUMS.txt", "checksums", 112, 12),
            ],
        ),
        platform(
            "windows-x86_64",
            vec![
                asset("rmux-v1-windows-x86_64.zip", "archive", 113, 13),
                asset("rmux.1.0.0.nupkg", "chocolatey-package", 120, 20),
                asset("SHA256SUMS.txt", "checksums", 114, 14),
            ],
        ),
    ];
    write_json(
        &manifest,
        &json!({
            "schema_version": 1, "repository_id": 1239918790,
            "source_git_sha": SOURCE, "candidate_run_id": 500,
            "candidate_run_attempt": 1, "release_intent_id": "release:v1.0.0:test",
            "planned_release_ref": "v1.0.0", "release_kind": "stable",
            "release_version": "1.0.0", "package_version": "1.0.0", "is_prerelease": false,
            "release_policy": {"sha256": hex(20), "contract_blob_oid": SOURCE, "record_count": 150},
            "created_at": "2026-07-19T00:00:00Z", "expires_at": "2026-07-21T00:00:00Z",
            "artifacts": artifacts
        }),
    );
    let candidate_args = vec![
        OsString::from("create"),
        OsString::from("--manifest"),
        manifest.as_os_str().to_owned(),
        OsString::from("--manifest-run-id"),
        OsString::from("501"),
        OsString::from("--manifest-run-attempt"),
        OsString::from("1"),
        OsString::from("--manifest-workflow-id"),
        OsString::from("316223904"),
        OsString::from("--manifest-artifact-id"),
        OsString::from("502"),
        OsString::from("--manifest-artifact-digest"),
        OsString::from(format!("sha256:{}", hex(21))),
        OsString::from("--output"),
        candidate.as_os_str().to_owned(),
    ];
    assert_success(invoke("candidate-reference.py", &candidate_args));
    write_json(
        &tag,
        &json!({
            "schema_version": 1, "status": "verified-signed-annotated-tag",
            "repository_id": 1239918790, "release_ref": "v1.0.0",
            "release_intent_id": "release:v1.0.0:test", "release_kind": "stable",
            "tag_object_sha": TAG_OBJECT, "target_git_sha": SOURCE,
            "candidate_run_id": 500, "candidate_manifest_artifact_id": 502,
            "candidate_manifest_artifact_digest": format!("sha256:{}", hex(21)),
            "candidate_manifest_sha256": read_json(&candidate)["manifest_sha256"].clone(),
            "release_policy_root_sha256": hex(20),
            "object_type": "tag", "annotated": true,
            "signature": {"verified": true, "format": "ssh", "key_fingerprint": format!("SHA256:{}", "A".repeat(43)), "signing_principal": "rmux-release@example.invalid"},
            "verified_at": "2026-07-19T00:29:00Z"
        }),
    );
    write_json(
        &audit,
        &json!({
            "schema_version": 1, "status": "shadow-non-authoritative",
            "repository_id": 1239918790, "source_git_sha": SOURCE,
            "candidate_run_id": 500, "release_intent_id": "release:v1.0.0:test",
            "policy_audit_run_id": 700, "policy_audit_run_attempt": 1,
            "predicate_artifact_id": 601,
            "predicate_artifact_digest": format!("sha256:{}", hex(22)),
            "predicate_sha256": hex(23), "emitted_at": "2026-07-19T00:30:00Z",
            "expires_at": "2026-07-19T00:45:00Z", "app_id": 4344532,
            "installation_id": 147749910, "workflow_id": 316435346,
            "workflow_path": ".github/workflows/release-promote.yml",
            "release_policy_sha256": hex(20)
        }),
    );
    let mut sum_entries = Vec::new();
    for artifact in read_json(&manifest)["artifacts"]
        .as_array()
        .expect("artifacts")
    {
        for item in artifact["files"].as_array().expect("files") {
            if ["archive", "debian", "rpm", "snap-amd64", "snap-arm64"]
                .contains(&item["role"].as_str().expect("asset role"))
            {
                sum_entries.push((
                    item["path"].as_str().unwrap().to_owned(),
                    item["sha256"].as_str().unwrap().to_owned(),
                ));
            }
        }
    }
    sum_entries.sort_by(|left, right| left.0.cmp(&right.0));
    let sum_lines: Vec<String> = sum_entries
        .into_iter()
        .map(|(name, digest)| format!("{digest}  {name}"))
        .collect();
    fs::write(&sums, format!("{}\n", sum_lines.join("\n"))).expect("write sums");
    fs::write(root.join("SHA256SUMS.sigstore.json"), "{\"bundle\":true}\n")
        .expect("write authorization bundle");
    fs::write(
        root.join("publication-receipt.sigstore.json"),
        "{\"bundle\":true}\n",
    )
    .expect("write receipt bundle");
    Fixture {
        root,
        manifest,
        candidate,
        tag,
        audit,
        sums,
        authorization,
        authorization_envelope,
        release_state,
        receipt,
    }
}

fn complete_authorization(fixture: &Fixture) {
    assert_success(invoke(
        "promotion-authorization.py",
        &auth_predicate_args(fixture, "--output", &fixture.authorization),
    ));
    assert_success(invoke(
        "promotion-authorization.py",
        &auth_envelope_args(fixture, "--output", &fixture.authorization_envelope),
    ));
    validate_schema(
        "promotion-authorization-predicate.schema.json",
        &fixture.authorization,
    );
    validate_schema(
        "promotion-authorization-envelope.schema.json",
        &fixture.authorization_envelope,
    );
}

fn complete_release_state(fixture: &Fixture) {
    let predicate = read_json(&fixture.authorization);
    let envelope = read_json(&fixture.authorization_envelope);
    let mut assets = Vec::new();
    for (index, asset) in predicate["assets"]
        .as_array()
        .unwrap()
        .iter()
        .chain(envelope["public_metadata_assets"].as_array().unwrap())
        .enumerate()
    {
        assets.push(json!({"id": 2000 + index, "name": asset["name"], "size": asset["size"], "digest": format!("sha256:{}", asset["sha256"].as_str().unwrap())}));
    }
    assets.sort_by_key(|item| item["name"].as_str().unwrap().to_owned());
    write_json(
        &fixture.release_state,
        &json!({
            "schema_version": 1, "status": "verified-immutable-release",
            "repository_id": 1239918790, "release_id": 1234,
            "release_ref": "v1.0.0", "source_git_sha": SOURCE,
            "tag_object_sha": TAG_OBJECT, "draft": false, "prerelease": false,
            "immutable": true, "created_at": "2026-07-19T00:32:00Z",
            "published_at": "2026-07-19T00:33:00Z", "assets": assets
        }),
    );
}

#[test]
fn authority_schemas_are_split_atomic_and_non_circular() {
    let schemas = repo_root().join(".github/release/schemas");
    assert!(!schemas.join("promotion-authorization.schema.json").exists());
    assert!(!schemas.join("publication-receipt.schema.json").exists());
    for name in [
        "promotion-authorization-predicate.schema.json",
        "promotion-authorization-envelope.schema.json",
        "publication-receipt-predicate.schema.json",
        "publication-receipt-envelope.schema.json",
    ] {
        let schema = read_json(&schemas.join(name));
        assert_eq!(schema["x-rmux-status"], "atomic-authority-bound");
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["oneOf"].as_array().expect("authority states").len(),
            2
        );
    }
    for name in [
        "promotion-authorization-predicate.schema.json",
        "publication-receipt-predicate.schema.json",
    ] {
        let text = fs::read_to_string(schemas.join(name)).unwrap();
        assert!(!text.contains("bundle_artifact_id\""));
        assert!(!text.contains("envelope_artifact_id"));
    }
}

#[test]
fn promotion_authorization_round_trip_rejects_forged_evidence() {
    let fixture = fixture();
    complete_authorization(&fixture);
    let duplicate = fixture.root.join("authorization-2.json");
    assert_success(invoke(
        "promotion-authorization.py",
        &auth_predicate_args(&fixture, "--output", &duplicate),
    ));
    assert_eq!(
        fs::read(&fixture.authorization).unwrap(),
        fs::read(&duplicate).unwrap()
    );
    assert_success(invoke(
        "promotion-authorization.py",
        &auth_verify_args(&fixture, &fixture.authorization),
    ));
    let predicate = read_json(&fixture.authorization);
    assert_eq!(predicate["status"], "disarmed-non-authoritative");
    assert_eq!(predicate["publication_authority"], false);
    assert!(predicate.get("attestation_id").is_none());
    let envelope_duplicate = fixture.root.join("authorization-envelope-2.json");
    assert_success(invoke(
        "promotion-authorization.py",
        &auth_envelope_args(&fixture, "--output", &envelope_duplicate),
    ));
    assert_eq!(
        fs::read(&fixture.authorization_envelope).unwrap(),
        fs::read(&envelope_duplicate).unwrap()
    );
    assert_success(invoke(
        "promotion-authorization.py",
        &as_verify(auth_envelope_args(
            &fixture,
            "--output",
            &fixture.authorization_envelope,
        )),
    ));
    let manifest_bytes = fs::read(&fixture.manifest).unwrap();
    let mut changed_manifest = manifest_bytes.clone();
    changed_manifest.extend_from_slice(b" ");
    fs::write(&fixture.manifest, changed_manifest).unwrap();
    assert_rejected(
        invoke(
            "promotion-authorization.py",
            &auth_predicate_args(&fixture, "--output", &duplicate),
        ),
        "manifest file digest changed",
    );
    fs::write(&fixture.manifest, manifest_bytes).unwrap();
    let original_candidate = read_json(&fixture.candidate);
    let mut forged_candidate = original_candidate.clone();
    forged_candidate["manifest_workflow_id"] = json!(999_999);
    write_json(&fixture.candidate, &forged_candidate);
    assert_rejected(
        invoke(
            "promotion-authorization.py",
            &auth_predicate_args(&fixture, "--output", &duplicate),
        ),
        "candidate reference identity changed",
    );
    write_json(&fixture.candidate, &original_candidate);
    let original_tag = read_json(&fixture.tag);
    let mut forged_tag = original_tag.clone();
    forged_tag["annotated"] = json!(false);
    write_json(&fixture.tag, &forged_tag);
    assert_rejected(
        invoke(
            "promotion-authorization.py",
            &auth_predicate_args(&fixture, "--output", &duplicate),
        ),
        "signed annotated release tag",
    );
    forged_tag = original_tag.clone();
    forged_tag["candidate_manifest_artifact_id"] = json!(503);
    write_json(&fixture.tag, &forged_tag);
    assert_rejected(
        invoke(
            "promotion-authorization.py",
            &auth_predicate_args(&fixture, "--output", &duplicate),
        ),
        "signed annotated release tag",
    );
    write_json(&fixture.tag, &original_tag);
    let original_audit = read_json(&fixture.audit);
    let mut stale_audit = original_audit.clone();
    stale_audit["expires_at"] = json!("2026-07-19T00:46:00Z");
    write_json(&fixture.audit, &stale_audit);
    assert_rejected(
        invoke(
            "promotion-authorization.py",
            &auth_predicate_args(&fixture, "--output", &duplicate),
        ),
        "at most fifteen minutes",
    );
    let mut expired_audit = original_audit.clone();
    expired_audit["emitted_at"] = json!("2026-07-19T00:20:00Z");
    expired_audit["expires_at"] = json!("2026-07-19T00:25:00Z");
    write_json(&fixture.audit, &expired_audit);
    assert_rejected(
        invoke(
            "promotion-authorization.py",
            &auth_predicate_args(&fixture, "--output", &duplicate),
        ),
        "policy audit expired before authorization",
    );
    write_json(&fixture.audit, &original_audit);
    let mut forged = predicate;
    forged["asset_count"] = json!(999);
    write_json(&fixture.authorization, &forged);
    assert_rejected(
        invoke(
            "promotion-authorization.py",
            &auth_verify_args(&fixture, &fixture.authorization),
        ),
        "predicate changed",
    );
}

#[test]
fn promotion_authorization_rejects_wrong_audit_app_identity_and_symlinks() {
    let fixture = fixture();
    let output = fixture.root.join("forged-authorization.json");
    let original_audit = read_json(&fixture.audit);

    for (field, value) in [("app_id", 4344533), ("installation_id", 147749911)] {
        let mut forged = original_audit.clone();
        forged[field] = json!(value);
        write_json(&fixture.audit, &forged);
        assert_rejected(
            invoke(
                "promotion-authorization.py",
                &auth_predicate_args(&fixture, "--output", &output),
            ),
            "policy audit reference does not bind the exact candidate",
        );
    }

    let audit_target = fixture.root.join("policy-audit-reference-target.json");
    write_json(&audit_target, &original_audit);
    fs::remove_file(&fixture.audit).expect("remove audit reference");
    symlink(&audit_target, &fixture.audit).expect("symlink audit reference");
    assert_rejected(
        invoke(
            "promotion-authorization.py",
            &auth_predicate_args(&fixture, "--output", &output),
        ),
        "policy audit reference must be one non-empty regular file",
    );
}

#[test]
fn publication_receipt_round_trip_rejects_asset_and_authorization_drift() {
    let fixture = fixture();
    complete_authorization(&fixture);
    complete_release_state(&fixture);
    assert_success(invoke(
        "publication-receipt.py",
        &receipt_predicate_args(&fixture, "--output", &fixture.receipt),
    ));
    let duplicate = fixture.root.join("receipt-2.json");
    assert_success(invoke(
        "publication-receipt.py",
        &receipt_predicate_args(&fixture, "--output", &duplicate),
    ));
    assert_eq!(
        fs::read(&fixture.receipt).unwrap(),
        fs::read(&duplicate).unwrap()
    );
    assert_success(invoke(
        "publication-receipt.py",
        &receipt_verify_args(&fixture, &fixture.receipt),
    ));
    let receipt_envelope = fixture.root.join("receipt-envelope.json");
    assert_success(invoke(
        "publication-receipt.py",
        &receipt_envelope_args(&fixture, "--output", &receipt_envelope),
    ));
    let envelope_duplicate = fixture.root.join("receipt-envelope-2.json");
    assert_success(invoke(
        "publication-receipt.py",
        &receipt_envelope_args(&fixture, "--output", &envelope_duplicate),
    ));
    assert_eq!(
        fs::read(&receipt_envelope).unwrap(),
        fs::read(&envelope_duplicate).unwrap()
    );
    assert_success(invoke(
        "publication-receipt.py",
        &as_verify(receipt_envelope_args(
            &fixture,
            "--output",
            &receipt_envelope,
        )),
    ));
    validate_schema(
        "publication-receipt-predicate.schema.json",
        &fixture.receipt,
    );
    validate_schema(
        "publication-receipt-envelope.schema.json",
        &receipt_envelope,
    );
    let receipt = read_json(&fixture.receipt);
    assert_eq!(receipt["downstream_authority"], false);
    assert!(receipt.get("attestation_id").is_none());
    let original_state = read_json(&fixture.release_state);
    let mut missing = original_state.clone();
    missing["assets"].as_array_mut().unwrap().pop();
    write_json(&fixture.release_state, &missing);
    assert_rejected(
        invoke(
            "publication-receipt.py",
            &receipt_predicate_args(&fixture, "--output", &duplicate),
        ),
        "asset cardinality changed",
    );
    let mut mutable = original_state.clone();
    mutable["immutable"] = json!(false);
    write_json(&fixture.release_state, &mutable);
    assert_rejected(
        invoke(
            "publication-receipt.py",
            &receipt_predicate_args(&fixture, "--output", &duplicate),
        ),
        "authorized immutable release",
    );
    write_json(&fixture.release_state, &original_state);
    let original_envelope = read_json(&fixture.authorization_envelope);
    let mut forged_envelope = original_envelope.clone();
    forged_envelope["predicate_sha256"] = json!(hex(99));
    write_json(&fixture.authorization_envelope, &forged_envelope);
    assert_rejected(
        invoke(
            "publication-receipt.py",
            &receipt_predicate_args(&fixture, "--output", &duplicate),
        ),
        "exact predicate",
    );
    write_json(&fixture.authorization_envelope, &original_envelope);
}
