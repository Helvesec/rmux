use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const SOURCE: &str = "0123456789abcdef0123456789abcdef01234567";
const CANDIDATE_RUN: u64 = 77;
const FAST_RUN: u64 = 42;
const INTENT: &str = "shadow:candidate:artifacts";
const RELEASE_REF: &str = "v0.9.0";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "rmux-candidate-artifacts-{label}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("create candidate artifact fixture");
    path
}

fn python() -> &'static str {
    if cfg!(windows) {
        "python.exe"
    } else {
        "python3"
    }
}

fn run(program: &Path, arguments: &[String]) -> Output {
    Command::new(python())
        .arg(program)
        .args(arguments)
        .current_dir(repo_root())
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "failed to run {} with {}: {error}",
                program.display(),
                python()
            )
        })
}

fn write_json(path: &Path, value: &serde_json::Value) {
    fs::write(path, serde_json::to_vec(value).expect("encode fixture"))
        .expect("write JSON fixture");
}

fn sha256(path: &Path) -> String {
    let script = concat!(
        "import hashlib, pathlib, sys; ",
        "print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())"
    );
    let output = Command::new(python())
        .args(["-c", script])
        .arg(path)
        .output()
        .unwrap_or_else(|error| panic!("failed to hash fixture with {}: {error}", python()));
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("fixture SHA-256 is UTF-8")
        .trim()
        .to_owned()
}

fn artifact_names() -> Vec<String> {
    let platforms = [
        "linux-x86_64",
        "linux-aarch64",
        "macos-x86_64",
        "macos-aarch64",
        "windows-x86_64",
    ];
    let mut names = vec![format!("rmux-fast-proof-{SOURCE}")];
    names.extend(
        platforms
            .iter()
            .map(|platform| format!("rmux-canonical-{platform}-{SOURCE}")),
    );
    names.extend(
        platforms
            .iter()
            .map(|platform| format!("rmux-canonical-provenance-{platform}-{SOURCE}")),
    );
    names
}

fn artifact(id: u64, name: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "name": name,
        "digest": format!("sha256:{id:064x}"),
        "expired": false,
        "size_in_bytes": 100 + id,
        "workflow_run": {
            "id": CANDIDATE_RUN,
            "repository_id": 1239918790,
            "head_repository_id": 1239918790,
            "head_sha": SOURCE
        }
    })
}

fn api_fixtures(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let repository = root.join("repository.json");
    let workflow_run = root.join("run.json");
    let artifacts = root.join("artifacts.json");
    write_json(
        &repository,
        &serde_json::json!({"id": 1239918790, "full_name": "Helvesec/rmux"}),
    );
    write_json(
        &workflow_run,
        &serde_json::json!({
            "id": CANDIDATE_RUN,
            "workflow_id": 277622540,
            "path": ".github/workflows/ci.yml",
            "name": "CI",
            "event": "workflow_dispatch",
            "head_branch": "main",
            "head_sha": SOURCE,
            "run_attempt": 1,
            "status": "completed",
            "conclusion": "success",
            "updated_at": "2026-07-19T12:00:00Z",
            "repository": {"id": 1239918790, "full_name": "Helvesec/rmux"},
            "head_repository": {"id": 1239918790, "full_name": "Helvesec/rmux"}
        }),
    );
    let values: Vec<_> = artifact_names()
        .iter()
        .enumerate()
        .map(|(index, name)| artifact(100 + index as u64, name))
        .collect();
    write_json(
        &artifacts,
        &serde_json::json!({"total_count": values.len(), "artifacts": values}),
    );
    (repository, workflow_run, artifacts)
}

fn resolve(root: &Path, artifacts: &Path, output: &Path) -> Output {
    let (repository, workflow_run, _) = api_fixtures(root);
    run(
        &repo_root().join("scripts/release/verify-candidate-artifacts.py"),
        &[
            "resolve".into(),
            "--candidate-run-id".into(),
            CANDIDATE_RUN.to_string(),
            "--expected-source-sha".into(),
            SOURCE.into(),
            "--repository-json".into(),
            repository.display().to_string(),
            "--run-json".into(),
            workflow_run.display().to_string(),
            "--artifacts-json".into(),
            artifacts.display().to_string(),
            "--output".into(),
            output.display().to_string(),
        ],
    )
}

#[test]
fn candidate_artifact_resolver_is_get_only_and_bounded() {
    let script = include_str!("../scripts/release/verify-candidate-artifacts.py");
    let resolution = include_str!("../scripts/release/candidate_artifact_resolution.py");
    assert!(script.lines().count() < 600);
    assert!(resolution.lines().count() < 600);
    for forbidden in [
        "gh release",
        "git push",
        "contents: write",
        "id-token: write",
    ] {
        assert!(
            !script.contains(forbidden) && !resolution.contains(forbidden),
            "resolver gained {forbidden}"
        );
    }
    for required in ["get_repository", "get_run", "gh_api"] {
        assert!(
            resolution.contains(required),
            "resolution module lost {required}"
        );
    }
    for required in [
        "expected_artifact_count\": 11",
        "canonical-build-record.py",
        "canonical-artifact-binding.py",
        "verified-for-shadow-sealing",
    ] {
        assert!(
            script.contains(required) || resolution.contains(required),
            "resolver lost {required}"
        );
    }
    assert_eq!(script.matches("write_text(").count(), 1);
    assert_eq!(resolution.matches("write_text(").count(), 0);
}

#[test]
fn candidate_artifact_resolver_requires_exact_nonexpired_namespace() {
    let root = temp_dir("resolve");
    let (_, _, artifacts) = api_fixtures(&root);
    let output = root.join("resolution.json");
    let accepted = resolve(&root, &artifacts, &output);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    let resolution: serde_json::Value =
        serde_json::from_slice(&fs::read(&output).expect("read resolution"))
            .expect("parse resolution");
    assert_eq!(resolution["expected_artifact_count"], 11);
    assert!(resolution.get("ignored_auxiliary_artifact_count").is_none());
    assert_eq!(
        resolution["artifacts"].as_array().expect("artifacts").len(),
        11
    );

    let mut fixture: serde_json::Value =
        serde_json::from_slice(&fs::read(&artifacts).expect("read artifacts"))
            .expect("parse artifacts");
    fixture["artifacts"][0]["expired"] = serde_json::json!(true);
    let expired = root.join("expired.json");
    write_json(&expired, &fixture);
    let rejected = resolve(&root, &expired, &root.join("expired-output.json"));
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("expired"));

    let extra = artifact(2000, "rmux-web-crypto-wasm");
    fixture["artifacts"][0]["expired"] = serde_json::json!(false);
    fixture["artifacts"]
        .as_array_mut()
        .expect("artifact array")
        .push(extra);
    fixture["total_count"] = serde_json::json!(12);
    let unexpected = root.join("unexpected.json");
    write_json(&unexpected, &fixture);
    let rejected = resolve(&root, &unexpected, &root.join("unexpected-output.json"));
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("exactly eleven"));

    let (_, _, original) = api_fixtures(&root);
    let mut fixture: serde_json::Value =
        serde_json::from_slice(&fs::read(&original).expect("read original artifacts"))
            .expect("parse original artifacts");
    fixture["artifacts"][0]["name"] = serde_json::json!("evil.zip");
    let wrong_name = root.join("wrong-name.json");
    write_json(&wrong_name, &fixture);
    let rejected = resolve(&root, &wrong_name, &root.join("wrong-name-output.json"));
    assert!(!rejected.status.success());
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("names differ"));
    assert!(stderr.contains("evil.zip"));

    let mut fixture: serde_json::Value =
        serde_json::from_slice(&fs::read(&original).expect("read original artifacts"))
            .expect("parse original artifacts");
    fixture["artifacts"][1]["id"] = fixture["artifacts"][0]["id"].clone();
    let duplicate = root.join("duplicate.json");
    write_json(&duplicate, &fixture);
    let rejected = resolve(&root, &duplicate, &root.join("duplicate-output.json"));
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("duplicates"));
    fs::remove_dir_all(root).expect("remove resolver fixture");
}

struct Platform {
    key: &'static str,
    runner: &'static str,
    os: &'static str,
    arch: &'static str,
    target: &'static str,
    archive: &'static str,
    linux_packages: bool,
}

const PLATFORMS: [Platform; 5] = [
    Platform {
        key: "linux-x86_64",
        runner: "ubuntu-22.04",
        os: "Linux",
        arch: "X64",
        target: "x86_64-unknown-linux-gnu",
        archive: "tar.gz",
        linux_packages: true,
    },
    Platform {
        key: "linux-aarch64",
        runner: "ubuntu-22.04-arm",
        os: "Linux",
        arch: "ARM64",
        target: "aarch64-unknown-linux-gnu",
        archive: "tar.gz",
        linux_packages: true,
    },
    Platform {
        key: "macos-x86_64",
        runner: "macos-15-intel",
        os: "macOS",
        arch: "X64",
        target: "x86_64-apple-darwin",
        archive: "tar.gz",
        linux_packages: false,
    },
    Platform {
        key: "macos-aarch64",
        runner: "macos-15",
        os: "macOS",
        arch: "ARM64",
        target: "aarch64-apple-darwin",
        archive: "tar.gz",
        linux_packages: false,
    },
    Platform {
        key: "windows-x86_64",
        runner: "windows-latest",
        os: "Windows",
        arch: "X64",
        target: "x86_64-pc-windows-msvc",
        archive: "zip",
        linux_packages: false,
    },
];

fn create_asset_bytes(directory: &Path, platform: &Platform) {
    fs::create_dir_all(directory).expect("create asset directory");
    let archive = format!("rmux-0.9.0-{}.{}", platform.key, platform.archive);
    fs::write(
        directory.join(&archive),
        format!("archive-{}", platform.key),
    )
    .expect("write archive");
    let mut names = vec![archive];
    if platform.linux_packages {
        let deb = format!("rmux_0.9.0_{}.deb", platform.arch.to_ascii_lowercase());
        let rpm = format!("rmux-0.9.0-1.{}.rpm", platform.arch.to_ascii_lowercase());
        fs::write(directory.join(&deb), format!("deb-{}", platform.key)).expect("write deb");
        fs::write(directory.join(&rpm), format!("rpm-{}", platform.key)).expect("write rpm");
        names.extend([deb, rpm]);
    }
    let checksums = names
        .iter()
        .map(|name| format!("{}  {name}", sha256(&directory.join(name))))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(directory.join("SHA256SUMS.txt"), format!("{checksums}\n")).expect("write checksums");
}

fn metadata<'a>(
    resolution: &'a serde_json::Value,
    role: &str,
    platform: &str,
) -> &'a serde_json::Value {
    resolution["artifacts"]
        .as_array()
        .expect("resolved artifacts")
        .iter()
        .find(|item| item["role"] == role && item["platform_key"] == platform)
        .expect("resolved platform artifact")
}

fn create_platform_download(root: &Path, resolution: &serde_json::Value, platform: &Platform) {
    let assets_meta = metadata(resolution, "canonical-assets", platform.key);
    let provenance_meta = metadata(resolution, "canonical-provenance", platform.key);
    let assets_root = root.join(assets_meta["name"].as_str().expect("asset name"));
    let assets = assets_root.join("assets");
    let provenance = root.join(provenance_meta["name"].as_str().expect("provenance name"));
    fs::create_dir_all(&provenance).expect("create provenance directory");
    create_asset_bytes(&assets, platform);
    let rustc = provenance.join("rustc-vV.txt");
    fs::write(
        &rustc,
        format!(
            "rustc 1.96.1\ncommit-hash: {SOURCE}\nhost: {}\nrelease: 1.96.1\n",
            platform.target
        ),
    )
    .expect("write rustc evidence");
    let record = assets_root.join("canonical-build-record.json");
    let record_create = run(
        &repo_root().join("scripts/release/canonical-build-record.py"),
        &[
            "create".into(),
            "--source-sha".into(),
            SOURCE.into(),
            "--fast-run-id".into(),
            FAST_RUN.to_string(),
            "--candidate-run-id".into(),
            CANDIDATE_RUN.to_string(),
            "--release-intent-id".into(),
            INTENT.into(),
            "--planned-release-ref".into(),
            RELEASE_REF.into(),
            "--release-kind".into(),
            "shadow".into(),
            "--platform-key".into(),
            platform.key.into(),
            "--assets-dir".into(),
            assets.display().to_string(),
            "--candidate-run-attempt".into(),
            "1".into(),
            "--runner-image".into(),
            platform.runner.into(),
            "--runner-os".into(),
            platform.os.into(),
            "--runner-arch".into(),
            platform.arch.into(),
            "--runner-environment".into(),
            "github-hosted".into(),
            "--rustc-verbose".into(),
            rustc.display().to_string(),
            "--output".into(),
            record.display().to_string(),
        ],
    );
    assert!(
        record_create.status.success(),
        "{}",
        String::from_utf8_lossy(&record_create.stderr)
    );
    let record_sha = String::from_utf8(record_create.stdout)
        .expect("record digest")
        .trim()
        .to_owned();
    fs::copy(&record, provenance.join("canonical-build-record.json")).expect("copy record");
    fs::write(
        provenance.join("build-provenance.sigstore.json"),
        format!("{{\"platform\":\"{}\"}}\n", platform.key),
    )
    .expect("write provenance bundle");
    let binding_create = run(
        &repo_root().join("scripts/release/canonical-artifact-binding.py"),
        &[
            "create".into(),
            "--source-sha".into(),
            SOURCE.into(),
            "--candidate-run-id".into(),
            CANDIDATE_RUN.to_string(),
            "--platform-key".into(),
            platform.key.into(),
            "--assets-artifact-id".into(),
            assets_meta["artifact_id"].to_string(),
            "--assets-artifact-name".into(),
            assets_meta["name"].as_str().expect("asset name").into(),
            "--assets-artifact-digest".into(),
            assets_meta["archive_digest"]
                .as_str()
                .expect("asset digest")
                .into(),
            "--build-record".into(),
            provenance
                .join("canonical-build-record.json")
                .display()
                .to_string(),
            "--build-record-sha256".into(),
            record_sha,
            "--attestation-id".into(),
            format!("attestation-{}", platform.key),
            "--attestation-bundle".into(),
            provenance
                .join("build-provenance.sigstore.json")
                .display()
                .to_string(),
            "--output".into(),
            provenance
                .join("canonical-artifact-binding.json")
                .display()
                .to_string(),
        ],
    );
    assert!(
        binding_create.status.success(),
        "{}",
        String::from_utf8_lossy(&binding_create.stderr)
    );
}

fn create_fast_download(root: &Path, resolution: &serde_json::Value, scratch: &Path) {
    let fast = resolution["artifacts"]
        .as_array()
        .expect("artifacts")
        .iter()
        .find(|item| item["role"] == "fast-proof")
        .expect("fast artifact");
    let directory = root.join(fast["name"].as_str().expect("fast artifact name"));
    fs::create_dir_all(&directory).expect("create fast proof directory");
    write_json(
        &directory.join("candidate-intent.json"),
        &serde_json::json!({
            "schema_version": 1, "repository_id": 1239918790, "source_git_sha": SOURCE,
            "fast_run_id": FAST_RUN, "release_intent_id": INTENT,
            "planned_release_ref": RELEASE_REF, "release_kind": "shadow",
            "release_version": "0.9.0", "package_version": "0.9.0",
            "is_prerelease": false, "candidate_run_attempt": 1
        }),
    );
    let mut proof = serde_json::json!({
        "schema_version": 1, "kind": "fast", "repository_id": 1239918790,
        "run_id": FAST_RUN, "run_attempt": 1, "source_git_sha": SOURCE,
        "test_fixture": false, "jobs": []
    });
    let unhashed = scratch.join("fast-proof-unhashed.json");
    write_json(&unhashed, &proof);
    let canonical = fs::read_to_string(&unhashed).expect("read unhashed proof");
    fs::write(&unhashed, canonical.trim()).expect("canonicalize fixture");
    proof["proof_sha256"] = serde_json::json!(sha256(&unhashed));
    write_json(&directory.join("fast-proof.json"), &proof);
    write_json(
        &directory.join("fast-nextest-artifact.json"),
        &serde_json::json!({
            "artifact_id": 500, "name": format!("rmux-windows-nextest-{SOURCE}"),
            "digest": format!("sha256:{}", "f".repeat(64)), "size_in_bytes": 1000,
            "run_id": FAST_RUN, "source_git_sha": SOURCE
        }),
    );
}

#[test]
fn downloaded_candidate_records_bind_every_exact_byte() {
    let root = temp_dir("download");
    let (_, _, artifacts) = api_fixtures(&root);
    let resolution_path = root.join("resolution.json");
    let resolved = resolve(&root, &artifacts, &resolution_path);
    assert!(
        resolved.status.success(),
        "{}",
        String::from_utf8_lossy(&resolved.stderr)
    );
    let resolution: serde_json::Value =
        serde_json::from_slice(&fs::read(&resolution_path).expect("read resolution"))
            .expect("parse resolution");
    let downloads = root.join("downloads");
    fs::create_dir(&downloads).expect("create download root");
    create_fast_download(&downloads, &resolution, &root);
    for platform in &PLATFORMS {
        create_platform_download(&downloads, &resolution, platform);
    }
    let verified = root.join("verified.json");
    let args = [
        "verify-downloaded".into(),
        "--candidate-run-id".into(),
        CANDIDATE_RUN.to_string(),
        "--expected-source-sha".into(),
        SOURCE.into(),
        "--fast-run-id".into(),
        FAST_RUN.to_string(),
        "--release-intent-id".into(),
        INTENT.into(),
        "--planned-release-ref".into(),
        RELEASE_REF.into(),
        "--release-kind".into(),
        "shadow".into(),
        "--resolution".into(),
        resolution_path.display().to_string(),
        "--downloads-dir".into(),
        downloads.display().to_string(),
        "--output".into(),
        verified.display().to_string(),
    ];
    let accepted = run(
        &repo_root().join("scripts/release/verify-candidate-artifacts.py"),
        &args,
    );
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    let sealed: serde_json::Value =
        serde_json::from_slice(&fs::read(&verified).expect("read verified metadata"))
            .expect("parse verified metadata");
    assert_eq!(sealed["status"], "verified-for-shadow-sealing");
    assert_eq!(
        sealed["source_artifacts"]
            .as_array()
            .expect("source artifacts")
            .len(),
        11
    );
    assert_eq!(
        sealed["canonical_platforms"]
            .as_array()
            .expect("platforms")
            .len(),
        5
    );

    let linux_provenance = metadata(&resolution, "canonical-provenance", "linux-x86_64");
    let binding_path = downloads
        .join(
            linux_provenance["name"]
                .as_str()
                .expect("Linux provenance artifact name"),
        )
        .join("canonical-artifact-binding.json");
    let original_binding = fs::read(&binding_path).expect("read canonical binding");
    let mut substituted_binding: serde_json::Value =
        serde_json::from_slice(&original_binding).expect("parse canonical binding");
    substituted_binding["build_record_sha256"] = serde_json::json!("0".repeat(64));
    write_json(&binding_path, &substituted_binding);
    let mut binding_args = args.clone();
    binding_args[18] = root.join("substituted-binding.json").display().to_string();
    let rejected_binding = run(
        &repo_root().join("scripts/release/verify-candidate-artifacts.py"),
        &binding_args,
    );
    assert!(!rejected_binding.status.success());
    fs::write(&binding_path, original_binding).expect("restore canonical binding");

    let linux = metadata(&resolution, "canonical-assets", "linux-x86_64");
    fs::write(
        downloads
            .join(linux["name"].as_str().expect("Linux artifact name"))
            .join("assets/rmux-0.9.0-linux-x86_64.tar.gz"),
        b"tampered bytes",
    )
    .expect("tamper candidate archive");
    let mut tampered_args = args;
    tampered_args[18] = root.join("tampered.json").display().to_string();
    let rejected = run(
        &repo_root().join("scripts/release/verify-candidate-artifacts.py"),
        &tampered_args,
    );
    assert!(!rejected.status.success());
    fs::remove_dir_all(root).expect("remove downloaded fixture");
}
