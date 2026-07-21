use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "rmux-canonical-{label}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("create canonical fixture directory");
    path
}

fn python() -> &'static str {
    if cfg!(windows) {
        "python.exe"
    } else {
        "python3"
    }
}

fn run(program: &Path, arguments: &[&str]) -> Output {
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

fn run_python(arguments: &[&str]) -> Output {
    Command::new(python())
        .args(arguments)
        .current_dir(repo_root())
        .output()
        .unwrap_or_else(|error| panic!("failed to run {}: {error}", python()))
}

fn job_block<'a>(workflow: &'a str, job: &str, next: &str) -> &'a str {
    workflow
        .split(&format!("\n  {job}:\n"))
        .nth(1)
        .unwrap_or_else(|| panic!("missing canonical job {job}"))
        .split(&format!("\n  {next}:\n"))
        .next()
        .unwrap_or_else(|| panic!("unbounded canonical job {job}"))
}

#[test]
fn canonical_workflow_has_five_literal_native_allocations() {
    let workflow = include_str!("../.github/workflows/canonical-native-build.yml");
    let expected = [
        (
            "build-linux-x86-64",
            "build-linux-aarch64",
            "runs-on: ubuntu-22.04",
            "platform-key: linux-x86_64",
            "target-triple: x86_64-unknown-linux-gnu",
        ),
        (
            "build-linux-aarch64",
            "build-macos-x86-64",
            "runs-on: ubuntu-22.04-arm",
            "platform-key: linux-aarch64",
            "target-triple: aarch64-unknown-linux-gnu",
        ),
        (
            "build-macos-x86-64",
            "build-macos-aarch64",
            "runs-on: macos-15-intel",
            "platform-key: macos-x86_64",
            "target-triple: x86_64-apple-darwin",
        ),
        (
            "build-macos-aarch64",
            "build-windows-x86-64",
            "runs-on: macos-15",
            "platform-key: macos-aarch64",
            "target-triple: aarch64-apple-darwin",
        ),
        (
            "build-windows-x86-64",
            "smoke-linux-x86-64",
            "runs-on: windows-latest",
            "platform-key: windows-x86_64",
            "target-triple: x86_64-pc-windows-msvc",
        ),
    ];
    for (job, next, runner, platform, target) in expected {
        let block = job_block(workflow, job, next);
        for required in [
            runner,
            platform,
            target,
            "uses: ./.github/actions/canonical-build",
        ] {
            assert!(block.contains(required), "{job} lost {required}");
        }
    }
    assert_eq!(
        workflow
            .matches("uses: ./.github/actions/canonical-build")
            .count(),
        5
    );
    assert!(!workflow.contains("runs-on: ${{"));
    assert!(!workflow.contains("self-hosted"));
}

#[test]
fn canonical_producers_are_object_cold_and_non_publishing() {
    let workflow = include_str!("../.github/workflows/canonical-native-build.yml");
    let build = include_str!("../.github/actions/canonical-build/action.yml");
    for forbidden in [
        "actions/cache@",
        "sccache",
        "contents: write",
        "packages: write",
        "secrets: inherit",
        "environment:",
        "gh release",
        "git push",
    ] {
        assert!(!workflow.contains(forbidden), "workflow gained {forbidden}");
        assert!(
            !build.contains(forbidden),
            "build action gained {forbidden}"
        );
    }
    for required in [
        "test ! -e \"$canonical_target\"",
        "test -z \"${RUSTC_WRAPPER:-}\"",
        "test -z \"${RUSTC_WORKSPACE_WRAPPER:-}\"",
        "cargo fetch --locked",
        "scripts/package-unix.sh",
        "scripts/package-windows.ps1",
        "canonical-wasm-bundle.py create",
        "canonical-crate-set.py create",
        "generate-chocolatey-package.sh",
        "canonical-build-record.py checksums",
        "RMUX_SNAP_PACKAGE: ${{ inputs.snap-package }}",
        "test -n \"$RMUX_SNAP_PACKAGE\"",
        "subject-checksums:",
        "create-storage-record: false",
        "retention-days: 7",
        "compression-level: 0",
    ] {
        assert!(build.contains(required), "canonical build lost {required}");
    }
    assert_eq!(build.matches("scripts/package-windows.ps1").count(), 1);
    assert!(!build.contains("test -n \"${{ inputs.snap-package }}\""));
    for api_bound in [
        "value: ${{ steps.assets-api.outputs.artifact_digest }}",
        "value: ${{ steps.provenance-api.outputs.artifact_digest }}",
        "RMUX_ASSETS_ARTIFACT_DIGEST: ${{ steps.assets-api.outputs.artifact_digest }}",
        "scripts/release/actions-artifact.py resolve-id",
        "--max-attempts 6",
        "--retry-delay-seconds 2",
    ] {
        assert!(
            build.contains(api_bound),
            "canonical build lost {api_bound}"
        );
    }
    assert!(!build.contains("steps.assets-upload.outputs.artifact-digest"));
    assert!(!build.contains("steps.provenance-upload.outputs.artifact-digest"));
    assert!(workflow.contains("attestations: write"));
    assert!(workflow.contains("id-token: write"));
    assert_eq!(workflow.matches("actions: read").count(), 10);
    assert_eq!(build.matches("description:").count(), 17);
    assert_eq!(build.matches("required: true").count(), 8);
    let smoke = include_str!("../.github/actions/canonical-smoke/action.yml");
    assert_eq!(smoke.matches("description:").count(), 14);
    assert_eq!(smoke.matches("required: true").count(), 13);
}

#[test]
fn canonical_version_resolution_is_structured_and_python_310_compatible() {
    let build = include_str!("../.github/actions/canonical-build/action.yml");

    assert_eq!(
        build
            .matches("cargo metadata --locked --no-deps --format-version 1")
            .count(),
        1
    );
    assert!(build.contains("RMUX_PACKAGE_VERSION=$version"));
    assert_eq!(
        build.matches("version=\"$RMUX_PACKAGE_VERSION\"").count(),
        3
    );
    assert!(build.contains("package[\"manifest_path\"]"));
    assert!(build.contains("planned_version=\"${planned_version%%-rc.*}\""));
    assert!(!build.contains("tomllib"));
}

#[cfg(unix)]
#[test]
fn canonical_checksums_reject_a_symbolic_manifest() {
    use std::os::unix::fs::symlink;

    let root = temp_dir("checksum-symlink");
    let assets = root.join("assets");
    fs::create_dir(&assets).expect("create asset directory");
    fs::write(assets.join("rmux.tar.gz"), b"asset").expect("write asset");
    let outside = root.join("outside.txt");
    fs::write(&outside, b"outside").expect("write symlink target");
    symlink(&outside, assets.join("SHA256SUMS.txt")).expect("create checksum symlink");

    let script = repo_root().join("scripts/release/canonical-build-record.py");
    let output = run(
        &script,
        &[
            "checksums",
            "--assets-dir",
            assets.to_str().expect("asset path"),
        ],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("checksum manifest must be a regular file"));
    assert_eq!(fs::read(&outside).expect("read symlink target"), b"outside");
    fs::remove_dir_all(root).expect("remove checksum fixture");
}

#[cfg(unix)]
#[test]
fn canonical_binding_rejects_symbolic_evidence_inputs() {
    let root = temp_dir("binding-symlink");
    let fixture = r#"
import hashlib
import pathlib
import subprocess
import sys

repo, root = map(pathlib.Path, sys.argv[1:])
script = repo / "scripts/release/canonical-artifact-binding.py"
record = root / "record.json"
bundle = root / "bundle.json"
record.write_bytes(b"record")
bundle.write_bytes(b"bundle")
common = [
    sys.executable, script, "create",
    "--source-sha", "a" * 40,
    "--candidate-run-id", "1",
    "--platform-key", "linux-x86_64",
    "--assets-artifact-id", "2",
    "--assets-artifact-name", f"rmux-canonical-linux-x86_64-{'a' * 40}",
    "--assets-artifact-digest", "sha256:" + "b" * 64,
    "--build-record-sha256", hashlib.sha256(record.read_bytes()).hexdigest(),
    "--attestation-id", "attestation",
    "--output", root / "binding.json",
]

record_link = root / "record-link.json"
record_link.symlink_to(record)
rejected = subprocess.run(
    [*common, "--build-record", record_link, "--attestation-bundle", bundle],
    cwd=repo, capture_output=True, text=True,
)
assert rejected.returncode != 0
assert "build record must be one non-empty regular file" in rejected.stderr

bundle_link = root / "bundle-link.json"
bundle_link.symlink_to(bundle)
rejected = subprocess.run(
    [*common, "--build-record", record, "--attestation-bundle", bundle_link],
    cwd=repo, capture_output=True, text=True,
)
assert rejected.returncode != 0
assert "attestation bundle must be one non-empty regular file" in rejected.stderr
"#;
    let output = run_python(&[
        "-c",
        fixture,
        repo_root().to_str().expect("repository path"),
        root.to_str().expect("fixture path"),
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(root).expect("remove binding fixture");
}

#[test]
fn canonical_inputs_check_symlinks_before_resolution() {
    let binding = include_str!("../scripts/release/canonical-artifact-binding.py");
    let binding_check = binding
        .find("if path.is_symlink():")
        .expect("binding symlink check");
    let binding_resolve = binding
        .find("resolved = path.resolve(strict=True)")
        .expect("binding path resolution");
    assert!(binding_check < binding_resolve);

    let record = include_str!("../scripts/release/canonical-build-record.py");
    let assets_check = record
        .find("if directory.is_symlink():")
        .expect("asset directory symlink check");
    let assets_resolve = record
        .find("root = directory.resolve(strict=True)")
        .expect("asset directory resolution");
    assert!(assets_check < assets_resolve);
    let rustc_check = record
        .find("if args.rustc_verbose.is_symlink():")
        .expect("rustc evidence symlink check");
    let rustc_resolve = record
        .find("rustc_path = args.rustc_verbose.resolve(strict=True)")
        .expect("rustc evidence resolution");
    assert!(rustc_check < rustc_resolve);
}

#[test]
fn canonical_smokes_consume_numeric_ids_and_exact_fast_drivers() {
    let smoke = include_str!("../.github/actions/canonical-smoke/action.yml");
    assert_eq!(smoke.matches("merge-multiple: true").count(), 2);
    for required in [
        "artifact-ids: ${{ inputs.assets-artifact-id }}",
        "test \"$RUNNER_ENVIRONMENT\" = github-hosted",
        "--expected-digest \"$RMUX_ASSETS_ARTIFACT_DIGEST\"",
        "--expected-record-sha256 \"$RMUX_EXPECTED_BUILD_RECORD_SHA256\"",
        "canonical-build-record.py verify",
        "artifact-ids: ${{ inputs.fast-nextest-artifact-id }}",
        "windows-nextest.tar.zst.sha256",
        "sha256sum --check",
        "Install cargo-nextest for persisted Windows test drivers",
        "uses: taiki-e/install-action@c7eb1735f09259a5035e8e5d44b1406b1cddc0fb",
        "tool: nextest@0.9.138",
        "fallback: none",
        "inputs.smoke-kind == 'runtime'",
        "inputs.smoke-kind == 'sdk'",
        "inputs.smoke-kind == 'mouse'",
        "-RunBinary",
        "-RunDaemonSmoke",
        "-RunSdkSmoke",
        "-RunMouseBorderSmoke",
    ] {
        assert!(smoke.contains(required), "canonical smoke lost {required}");
    }
    assert_eq!(smoke.matches("tool: nextest@0.9.138").count(), 1);
    assert!(
        !smoke.contains("-RunCtrlMatrixSmoke"),
        "GitHub-hosted canonical smokes cannot run the interactive Ctrl matrix"
    );
    let record_step = smoke
        .split("    - name: Verify the build record and all downloaded bytes\n")
        .nth(1)
        .expect("canonical record verification step")
        .split("\n    - name: Install Linux verification tools\n")
        .next()
        .expect("bounded canonical record verification step");
    assert!(record_step
        .contains("RMUX_EXPECTED_BUILD_RECORD_SHA256: ${{ inputs.expected-build-record-sha256 }}"));
    assert!(record_step.contains("--expected-record-sha256 \"$RMUX_EXPECTED_BUILD_RECORD_SHA256\""));
    let workflow = include_str!("../.github/workflows/canonical-native-build.yml");
    assert!(workflow.contains("smoke: [runtime, sdk, mouse]"));
    assert!(workflow.contains("max-parallel: 3"));
    assert_eq!(
        workflow
            .matches("expected-build-record-sha256: ${{ needs.build-")
            .count(),
        5
    );
}

#[test]
fn canonical_record_rejects_mutated_record_or_downloaded_bytes() {
    let root = temp_dir("record");
    let assets = root.join("assets");
    fs::create_dir(&assets).expect("create asset directory");
    fs::write(
        assets.join("rmux-0.9.0-linux-x86_64.tar.gz"),
        b"canonical-bytes",
    )
    .expect("write canonical asset");
    fs::write(assets.join("rmux_0.9.0_amd64.deb"), b"canonical-deb").expect("write canonical deb");
    fs::write(assets.join("rmux-0.9.0-1.x86_64.rpm"), b"canonical-rpm")
        .expect("write canonical rpm");
    for (name, bytes) in [
        ("rmux-0.9.0-crate-package-set.tar", b"crate-set".as_slice()),
        ("rmux-0.9.0-snap-amd64.snap", b"snap".as_slice()),
        ("rmux-web-crypto-wasm-0.9.0.tar", b"wasm".as_slice()),
        (
            "rmux-web-crypto-wasm-0.9.0.provenance.json",
            b"provenance".as_slice(),
        ),
    ] {
        fs::write(assets.join(name), bytes).expect("write supplemental asset");
    }
    let rustc = root.join("rustc-vV.txt");
    fs::write(
        &rustc,
        b"rustc 1.96.1\nbinary: rustc\ncommit-hash: 0123456789abcdef0123456789abcdef01234567\nhost: x86_64-unknown-linux-gnu\nrelease: 1.96.1\n",
    )
    .expect("write rustc evidence");
    let record = root.join("canonical-build-record.json");
    let script = repo_root().join("scripts/release/canonical-build-record.py");
    let checksums = run(
        &script,
        &[
            "checksums",
            "--assets-dir",
            assets.to_str().expect("asset path"),
        ],
    );
    assert!(
        checksums.status.success(),
        "{}",
        String::from_utf8_lossy(&checksums.stderr)
    );
    let source = "0123456789abcdef0123456789abcdef01234567";
    let common = [
        "--source-sha",
        source,
        "--fast-run-id",
        "42",
        "--candidate-run-id",
        "77",
        "--release-intent-id",
        "shadow:canonical:test",
        "--planned-release-ref",
        "v0.9.0",
        "--release-kind",
        "shadow",
        "--platform-key",
        "linux-x86_64",
        "--assets-dir",
        assets.to_str().expect("asset path"),
    ];
    let mut create = vec!["create"];
    create.extend(common);
    create.extend([
        "--candidate-run-attempt",
        "1",
        "--runner-image",
        "ubuntu-22.04",
        "--runner-os",
        "Linux",
        "--runner-arch",
        "X64",
        "--runner-environment",
        "github-hosted",
        "--rustc-verbose",
        rustc.to_str().expect("rustc path"),
        "--output",
        record.to_str().expect("record path"),
    ]);
    let created = run(&script, &create);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let created_record: serde_json::Value =
        serde_json::from_slice(&fs::read(&record).expect("read created record"))
            .expect("parse created record");
    let created_paths = created_record["files"]
        .as_array()
        .expect("created record files")
        .iter()
        .map(|entry| entry["path"].as_str().expect("created record file path"))
        .collect::<Vec<_>>();
    assert_eq!(
        created_paths,
        [
            "SHA256SUMS.txt",
            "rmux-0.9.0-1.x86_64.rpm",
            "rmux-0.9.0-crate-package-set.tar",
            "rmux-0.9.0-linux-x86_64.tar.gz",
            "rmux-0.9.0-snap-amd64.snap",
            "rmux-web-crypto-wasm-0.9.0.provenance.json",
            "rmux-web-crypto-wasm-0.9.0.tar",
            "rmux_0.9.0_amd64.deb",
        ],
        "canonical asset order must not depend on host path semantics"
    );
    let record_sha = String::from_utf8(created.stdout).expect("record digest is UTF-8");
    let record_sha = record_sha.trim();
    let mut verify = vec!["verify"];
    verify.extend(common);
    verify.extend([
        "--record",
        record.to_str().expect("record path"),
        "--expected-record-sha256",
        record_sha,
    ]);
    assert!(run(&script, &verify).status.success());
    let mut noncanonical_digest = verify.clone();
    *noncanonical_digest
        .last_mut()
        .expect("expected record digest argument") =
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let rejected_digest = run(&script, &noncanonical_digest);
    assert!(!rejected_digest.status.success());
    assert!(String::from_utf8_lossy(&rejected_digest.stderr)
        .contains("expected canonical build record digest is invalid"));

    let original_record = fs::read(&record).expect("read canonical record");
    let mut substituted_record = original_record.clone();
    substituted_record.push(b'\n');
    fs::write(&record, substituted_record).expect("substitute equivalent record bytes");
    let rejected_record = run(&script, &verify);
    assert!(!rejected_record.status.success());
    assert!(String::from_utf8_lossy(&rejected_record.stderr)
        .contains("canonical build record digest differs from build output"));
    fs::write(&record, original_record).expect("restore canonical record");

    fs::write(
        assets.join("rmux-0.9.0-linux-x86_64.tar.gz"),
        b"mutated-bytes",
    )
    .expect("mutate asset");
    assert!(run(
        &script,
        &[
            "checksums",
            "--assets-dir",
            assets.to_str().expect("asset path"),
        ],
    )
    .status
    .success());
    let rejected = run(&script, &verify);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("asset set or digest changed"));
    fs::remove_dir_all(root).expect("remove canonical record fixture");
}

#[test]
fn actions_artifact_binding_rejects_digest_or_run_drift() {
    let root = temp_dir("artifact");
    let artifact_path = root.join("artifact.json");
    let source = "0123456789abcdef0123456789abcdef01234567";
    let name = format!("rmux-canonical-linux-x86_64-{source}");
    let digest = format!("sha256:{}", "a".repeat(64));
    fs::write(
        &artifact_path,
        serde_json::json!({
            "id": 88,
            "name": name,
            "digest": digest,
            "expired": false,
            "size_in_bytes": 123,
            "created_at": "2026-07-20T00:00:00Z",
            "updated_at": "2026-07-20T00:00:01Z",
            "expires_at": "2026-07-27T00:00:00Z",
            "workflow_run": {
                "id": 77,
                "repository_id": 1239918790,
                "head_repository_id": 1239918790,
                "head_sha": source
            }
        })
        .to_string(),
    )
    .expect("write artifact fixture");
    let script = repo_root().join("scripts/release/actions-artifact.py");
    let github_output = root.join("github-output.txt");
    let resolved = run(
        &script,
        &[
            "resolve-id",
            "--run-id",
            "77",
            "--artifact-id",
            "88",
            "--name",
            &name,
            "--expected-source-sha",
            source,
            "--artifact-json",
            artifact_path.to_str().expect("artifact fixture path"),
            "--github-output",
            github_output.to_str().expect("GitHub output path"),
        ],
    );
    assert!(
        resolved.status.success(),
        "{}",
        String::from_utf8_lossy(&resolved.stderr)
    );
    let api_outputs = fs::read_to_string(&github_output).expect("read API outputs");
    let expected_outputs = [
        "artifact_id=88".to_owned(),
        format!("artifact_digest={digest}"),
        format!("artifact_name={name}"),
    ];
    assert_eq!(api_outputs.lines().count(), expected_outputs.len());
    for expected in expected_outputs {
        assert!(api_outputs.lines().any(|line| line == expected));
    }
    let retained = run(
        &script,
        &[
            "resolve-id",
            "--run-id",
            "77",
            "--artifact-id",
            "88",
            "--name",
            &name,
            "--expected-source-sha",
            source,
            "--artifact-json",
            artifact_path.to_str().expect("artifact fixture path"),
            "--include-retention",
        ],
    );
    assert!(
        retained.status.success(),
        "{}",
        String::from_utf8_lossy(&retained.stderr)
    );
    let retained: serde_json::Value =
        serde_json::from_slice(&retained.stdout).expect("parse retention metadata");
    assert_eq!(retained["created_at"], "2026-07-20T00:00:00Z");
    assert_eq!(retained["updated_at"], "2026-07-20T00:00:01Z");
    assert_eq!(retained["expires_at"], "2026-07-27T00:00:00Z");

    let accepted = run(
        &script,
        &[
            "verify",
            "--run-id",
            "77",
            "--artifact-id",
            "88",
            "--name",
            &name,
            "--expected-digest",
            &digest,
            "--expected-source-sha",
            source,
            "--artifact-json",
            artifact_path.to_str().expect("artifact fixture path"),
        ],
    );
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    let run_path = root.join("run.json");
    fs::write(
        &run_path,
        serde_json::json!({
            "id": 77,
            "workflow_id": 316223904,
            "path": ".github/workflows/release-shadow.yml",
            "event": "workflow_dispatch",
            "run_attempt": 1,
            "head_sha": source,
            "head_branch": "main",
            "status": "completed",
            "conclusion": "success",
            "repository": {"id": 1239918790},
            "head_repository": {"id": 1239918790}
        })
        .to_string(),
    )
    .expect("write run fixture");
    let strict_args = [
        "verify",
        "--run-id",
        "77",
        "--artifact-id",
        "88",
        "--name",
        &name,
        "--expected-digest",
        &digest,
        "--expected-source-sha",
        source,
        "--artifact-json",
        artifact_path.to_str().expect("artifact fixture path"),
        "--run-json",
        run_path.to_str().expect("run fixture path"),
        "--expected-workflow-id",
        "316223904",
        "--expected-workflow-path",
        ".github/workflows/release-shadow.yml",
        "--expected-event",
        "workflow_dispatch",
        "--expected-head-branch",
        "main",
    ];
    let strict = run(&script, &strict_args);
    assert!(
        strict.status.success(),
        "{}",
        String::from_utf8_lossy(&strict.stderr)
    );
    let mut forged_run: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&run_path).expect("read run fixture"))
            .expect("parse run fixture");
    forged_run["workflow_id"] = serde_json::json!(999);
    fs::write(&run_path, forged_run.to_string()).expect("forge run fixture");
    let forged_origin = run(&script, &strict_args);
    assert!(!forged_origin.status.success());
    assert!(String::from_utf8_lossy(&forged_origin.stderr)
        .contains("workflow run workflow_id mismatch"));
    let mut running_run: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&run_path).expect("read forged fixture"))
            .expect("parse forged fixture");
    running_run["workflow_id"] = serde_json::json!(316223904);
    running_run["status"] = serde_json::json!("in_progress");
    running_run["conclusion"] = serde_json::Value::Null;
    fs::write(&run_path, running_run.to_string()).expect("write running fixture");
    let running = Command::new(python())
        .arg(&script)
        .args(strict_args)
        .arg("--allow-running-current-run")
        .env("GITHUB_RUN_ID", "77")
        .env("GITHUB_RUN_ATTEMPT", "1")
        .env("GITHUB_SHA", source)
        .env("GITHUB_REF_NAME", "main")
        .env("GITHUB_EVENT_NAME", "workflow_dispatch")
        .current_dir(repo_root())
        .output()
        .expect("verify current running artifact");
    assert!(
        running.status.success(),
        "{}",
        String::from_utf8_lossy(&running.stderr)
    );
    let wrong_current_run = Command::new(python())
        .arg(&script)
        .args(strict_args)
        .arg("--allow-running-current-run")
        .env("GITHUB_RUN_ID", "78")
        .env("GITHUB_RUN_ATTEMPT", "1")
        .env("GITHUB_SHA", source)
        .env("GITHUB_REF_NAME", "main")
        .env("GITHUB_EVENT_NAME", "workflow_dispatch")
        .current_dir(repo_root())
        .output()
        .expect("reject another running artifact");
    assert!(!wrong_current_run.status.success());
    assert!(String::from_utf8_lossy(&wrong_current_run.stderr)
        .contains("not the current attempt-1 run"));
    let rejected = run(
        &script,
        &[
            "verify",
            "--run-id",
            "78",
            "--artifact-id",
            "88",
            "--name",
            &name,
            "--expected-digest",
            &digest,
            "--expected-source-sha",
            source,
            "--artifact-json",
            artifact_path.to_str().expect("artifact fixture path"),
        ],
    );
    assert!(!rejected.status.success());

    fs::write(
        &artifact_path,
        serde_json::json!({
            "id": 88,
            "name": name,
            "digest": "a".repeat(64),
            "expired": false,
            "size_in_bytes": 123,
            "created_at": "2026-07-20T00:00:00Z",
            "updated_at": "2026-07-20T00:00:01Z",
            "expires_at": "2026-07-27T00:00:00Z",
            "workflow_run": {
                "id": 77,
                "repository_id": 1239918790,
                "head_repository_id": 1239918790,
                "head_sha": source
            }
        })
        .to_string(),
    )
    .expect("write raw digest fixture");
    let raw_digest = run(
        &script,
        &[
            "resolve-id",
            "--run-id",
            "77",
            "--artifact-id",
            "88",
            "--name",
            &name,
            "--expected-source-sha",
            source,
            "--artifact-json",
            artifact_path.to_str().expect("artifact fixture path"),
        ],
    );
    assert!(!raw_digest.status.success());
    assert!(String::from_utf8_lossy(&raw_digest.stderr)
        .contains("artifact digest is not a SHA-256 digest"));

    let wrong_id = run(
        &script,
        &[
            "resolve-id",
            "--run-id",
            "77",
            "--artifact-id",
            "89",
            "--name",
            &name,
            "--expected-source-sha",
            source,
            "--artifact-json",
            artifact_path.to_str().expect("artifact fixture path"),
        ],
    );
    assert!(!wrong_id.status.success());
    assert!(String::from_utf8_lossy(&wrong_id.stderr)
        .contains("artifact ID does not match the requested ID"));

    let unbounded_retry = run(
        &script,
        &[
            "resolve-id",
            "--run-id",
            "77",
            "--artifact-id",
            "88",
            "--name",
            &name,
            "--expected-source-sha",
            source,
            "--artifact-json",
            artifact_path.to_str().expect("artifact fixture path"),
            "--max-attempts",
            "11",
        ],
    );
    assert!(!unbounded_retry.status.success());
    assert!(String::from_utf8_lossy(&unbounded_retry.stderr)
        .contains("artifact lookup max attempts must be between 1 and 10"));
    fs::remove_dir_all(root).expect("remove artifact fixture");
}

#[test]
fn canonical_post_upload_binding_rejects_bundle_mutation() {
    let root = temp_dir("binding");
    let record = root.join("canonical-build-record.json");
    let bundle = root.join("build-provenance.sigstore.json");
    let binding = root.join("canonical-artifact-binding.json");
    fs::write(&record, b"{\"record\":true}\n").expect("write build record fixture");
    fs::write(&bundle, b"{\"bundle\":true}\n").expect("write attestation fixture");
    let record_sha = "5898faeed3cd8f317892bae8c3a05873926b10cc762135e2fef6fbb7b597fcba";
    let source = "0123456789abcdef0123456789abcdef01234567";
    let name = format!("rmux-canonical-linux-x86_64-{source}");
    let digest = format!("sha256:{}", "a".repeat(64));
    let script = repo_root().join("scripts/release/canonical-artifact-binding.py");
    let common = [
        "--source-sha",
        source,
        "--candidate-run-id",
        "77",
        "--platform-key",
        "linux-x86_64",
        "--assets-artifact-id",
        "88",
        "--assets-artifact-name",
        &name,
        "--assets-artifact-digest",
        &digest,
        "--build-record",
        record.to_str().expect("record path"),
        "--build-record-sha256",
        record_sha,
        "--attestation-id",
        "attestation-1",
        "--attestation-bundle",
        bundle.to_str().expect("bundle path"),
    ];
    let mut create = vec!["create"];
    create.extend(common);
    create.extend(["--output", binding.to_str().expect("binding path")]);
    let created = run(&script, &create);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let mut verify = vec!["verify"];
    verify.extend(common);
    verify.extend(["--binding", binding.to_str().expect("binding path")]);
    assert!(run(&script, &verify).status.success());

    fs::write(&bundle, b"{\"bundle\":false}\n").expect("mutate attestation fixture");
    assert!(!run(&script, &verify).status.success());
    fs::remove_dir_all(root).expect("remove binding fixture");
}

#[test]
fn canonical_runner_schema_is_a_target_specific_allowlist() {
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../.github/release/schemas/candidate-manifest.schema.json"
    ))
    .expect("parse candidate manifest schema");
    assert_eq!(
        schema["properties"]["artifacts"]["items"]["properties"]["runner_image"]["enum"],
        serde_json::json!([
            "macos-15",
            "macos-15-intel",
            "ubuntu-22.04",
            "ubuntu-22.04-arm",
            "windows-latest"
        ])
    );
    assert_eq!(
        schema["properties"]["artifacts"]["items"]["allOf"]
            .as_array()
            .expect("target runner bindings")
            .len(),
        5
    );
    let contract: serde_json::Value = serde_json::from_str(include_str!(
        "../.github/release/canonical-build-contract.json"
    ))
    .expect("parse canonical build contract");
    assert_eq!(
        contract["platforms"].as_array().expect("platforms").len(),
        5
    );
}

#[test]
fn canonical_contract_loader_rejects_duplicate_json_keys() {
    let root = temp_dir("duplicate-json");
    let fixture = root.join("contract.json");
    fs::write(&fixture, b"{\"schema_version\":1,\"schema_version\":1}\n")
        .expect("write duplicate-key contract");
    let script = concat!(
        "import pathlib, sys; ",
        "sys.path.insert(0, 'scripts/release'); ",
        "from canonical_contract import _load; ",
        "_load(pathlib.Path(sys.argv[1]))"
    );
    let rejected = run_python(&[
        "-c",
        script,
        fixture.to_str().expect("contract fixture path"),
    ]);
    assert!(!rejected.status.success());
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("duplicate JSON object key"), "{stderr}");
    fs::remove_dir_all(root).expect("remove duplicate-key fixture");
}
