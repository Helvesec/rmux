#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Command, Output};
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[cfg(unix)]
fn run(script: &str, args: &[&str]) -> Output {
    Command::new(repo_root().join(script))
        .args(args)
        .current_dir(repo_root())
        .output()
        .unwrap_or_else(|error| panic!("failed to run {script}: {error}"))
}

#[cfg(unix)]
fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[cfg(unix)]
fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[cfg(unix)]
fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("rmux-{label}-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&path).expect("create temp directory");
    path
}

#[test]
#[cfg(unix)]
fn changelog_checker_rejects_unversioned_tmux_claim_without_test_link() {
    let root = temp_dir("changelog-tmux-claim");
    let changelog = root.join("CHANGELOG.md");
    let required_sections = "## 0.9.0\n\n- Matches tmux queued attach sequencing.\n\n## 0.8.0\n\n## 0.7.1\n\n## 0.7.0\n";
    fs::write(&changelog, required_sections).expect("write changelog fixture");
    let changelog_arg = changelog.to_string_lossy().into_owned();

    let rejected = run("scripts/check-changelog-release.py", &[&changelog_arg]);
    assert!(!rejected.status.success(), "{}", stdout(&rejected));
    assert!(
        stderr(&rejected).contains("tmux compatibility claim lacks a fixture/test link"),
        "{}",
        stderr(&rejected)
    );

    let linked = required_sections.replace(
        "Matches tmux queued attach sequencing.",
        "Matches tmux queued attach sequencing, backed by [tests](tests/cli_attach_flow.rs).",
    );
    fs::write(&changelog, linked).expect("write linked changelog fixture");
    let accepted = run("scripts/check-changelog-release.py", &[&changelog_arg]);
    assert!(accepted.status.success(), "{}", stderr(&accepted));

    fs::remove_dir_all(root).expect("remove changelog fixture");
}

#[test]
fn tmux_ledger_gate_reads_authoritative_inventories_and_named_divergence_tests() {
    let checker = include_str!("../scripts/check-tmux-release-ledger.py");

    for authoritative_path in [
        "crates/rmux-core/src/command_inventory/signatures.rs",
        "crates/rmux-core/src/options/table.rs",
    ] {
        assert!(
            checker.contains(authoritative_path),
            "tmux ledger gate lost authoritative inventory {authoritative_path}"
        );
    }
    for stale_facade in [
        "COMMAND_INVENTORY = Path(\"src/cli/command_inventory.rs\")",
        "OPTIONS_REGISTRY = Path(\"crates/rmux-core/src/options/registry.rs\")",
    ] {
        assert!(
            !checker.contains(stale_facade),
            "tmux ledger gate regressed to stale facade {stale_facade}"
        );
    }
    for exhaustive_guard in [
        "PRODUCT_DIVERGENCE_TEST",
        "git\", \"ls-files\"",
        "has no ledger entry",
        "stale product-divergence test reference(s)",
        "uses a non-auditable product-divergence wildcard",
        "path.as_posix() not in fixture",
    ] {
        assert!(
            checker.contains(exhaustive_guard),
            "tmux ledger gate lost exhaustive guard {exhaustive_guard}"
        );
    }
}

#[test]
fn winget_portable_archive_preserves_the_private_runtime_layout() {
    let generator = include_str!("../scripts/generate-winget-manifest.sh");
    let validator = include_str!("../scripts/validate-winget-manifest.ps1");

    assert!(
        generator.contains("ArchiveBinariesDependOnPath: true"),
        "WinGet would install only the public shim and discard its private helper layout"
    );
    assert!(
        validator.contains("AssertManifestValue \"ArchiveBinariesDependOnPath\" \"true\""),
        "the WinGet validator must reject manifests that discard private runtime files"
    );
}

#[test]
fn current_changelog_records_the_exact_detached_wire_version() {
    let changelog = include_str!("../CHANGELOG.md");
    let current_release = changelog
        .split("\n## 0.8.0\n")
        .next()
        .expect("0.9.0 changelog section");
    let expected = format!(
        "detached RPC frame envelope from wire version 3 to {}",
        rmux_proto::RMUX_WIRE_VERSION
    );

    assert!(
        current_release.contains(&expected),
        "0.9.0 changelog must record the current detached wire version: {expected}"
    );
    assert!(
        current_release.contains("already-running older server must be\n  restarted"),
        "0.9.0 changelog must tell operators that this hard wire cut requires a server restart"
    );
}

#[test]
fn release_runbook_retains_non_bypassable_rc_tag_provenance() {
    let releasing = include_str!("../RELEASING.md");
    let release_identity = include_str!("../scripts/release-identity.sh");
    let protection = include_str!("../scripts/verify-release-tag-protection.sh");

    assert!(releasing.contains("Retain the protected RC tag"));
    assert!(!releasing.contains("Delete the disposable RC tag"));
    assert!(release_identity.contains("immutable stable or RC tag"));
    assert!(!release_identity.contains("disposable RC tag"));
    assert!(protection.contains("bypass_actors"));
}

#[cfg(unix)]
fn run_release_ref_fixture(
    fake_bin: &Path,
    ref_type: &str,
    ref_sha: &str,
    peeled_type: &str,
    peeled_sha: &str,
    tag_verified: bool,
    release_target: Option<&str>,
) -> Output {
    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::new(repo_root().join("scripts/verify-release-ref.sh"))
        .args([
            "Helvesec/rmux",
            "v0.9.0",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ])
        .env("PATH", path)
        .env("FAKE_REF_TYPE", ref_type)
        .env("FAKE_REF_SHA", ref_sha)
        .env("FAKE_PEELED_TYPE", peeled_type)
        .env("FAKE_PEELED_SHA", peeled_sha)
        .env("FAKE_TAG_VERIFIED", tag_verified.to_string())
        .env("FAKE_RELEASE_TARGET", release_target.unwrap_or_default())
        .current_dir(repo_root())
        .output()
        .expect("run release ref verifier")
}

#[test]
#[cfg(unix)]
fn release_ref_verifier_peels_tags_and_rejects_identity_drift() {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_dir("release-ref-verifier");
    let fake_bin = root.join("bin");
    fs::create_dir_all(&fake_bin).expect("create fake bin");
    let fake_gh = fake_bin.join("gh");
    fs::write(
        &fake_gh,
        r#"#!/usr/bin/env bash
set -euo pipefail
endpoint=''
for argument in "$@"; do endpoint=$argument; done
case "$endpoint" in
  */git/ref/tags/*)
    printf '{"object":{"type":"%s","sha":"%s"}}\n' "$FAKE_REF_TYPE" "$FAKE_REF_SHA"
    ;;
  */git/tags/*)
    reason=unsigned
    if [[ "$FAKE_TAG_VERIFIED" == true ]]; then reason=valid; fi
    printf '{"object":{"type":"%s","sha":"%s"},"verification":{"verified":%s,"reason":"%s"}}\n' \
      "$FAKE_PEELED_TYPE" "$FAKE_PEELED_SHA" "$FAKE_TAG_VERIFIED" "$reason"
    ;;
  */releases/tags/*)
    if [[ -z ${FAKE_RELEASE_TARGET:-} ]]; then exit 1; fi
    printf '{"tag_name":"v0.9.0","target_commitish":"%s"}\n' "$FAKE_RELEASE_TARGET"
    ;;
  *)
    echo "unexpected endpoint: $endpoint" >&2
    exit 2
    ;;
esac
"#,
    )
    .expect("write fake gh");
    let mut permissions = fs::metadata(&fake_gh)
        .expect("fake gh metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_gh, permissions).expect("chmod fake gh");

    let expected = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let tag_object = "1111111111111111111111111111111111111111";
    let lightweight = run_release_ref_fixture(
        &fake_bin, "commit", expected, "commit", expected, true, None,
    );
    assert!(!lightweight.status.success());
    assert!(stderr(&lightweight).contains("verified signed annotated tag"));

    let annotated = run_release_ref_fixture(
        &fake_bin,
        "tag",
        tag_object,
        "commit",
        expected,
        true,
        Some(expected),
    );
    assert!(annotated.status.success(), "{}", stderr(&annotated));

    let unsigned = run_release_ref_fixture(
        &fake_bin, "tag", tag_object, "commit", expected, false, None,
    );
    assert!(!unsigned.status.success());
    assert!(stderr(&unsigned).contains("no verified signature"));

    let moved = run_release_ref_fixture(
        &fake_bin,
        "tag",
        tag_object,
        "commit",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        true,
        None,
    );
    assert!(
        !moved.status.success(),
        "moved release tag must fail closed"
    );
    assert!(stderr(&moved).contains("expected"));

    let wrong_release = run_release_ref_fixture(
        &fake_bin,
        "tag",
        tag_object,
        "commit",
        expected,
        true,
        Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
    );
    assert!(
        !wrong_release.status.success(),
        "pre-existing release target drift must fail closed"
    );
    assert!(stderr(&wrong_release).contains("target_commitish"));

    fs::remove_dir_all(root).expect("remove release ref fixture");
}

#[test]
#[cfg(unix)]
fn release_identity_separates_rc_tag_from_cargo_package_version() {
    let stable = run("scripts/release-identity.sh", &["v0.9.0"]);
    assert!(stable.status.success(), "{}", stderr(&stable));
    let stable_text = stdout(&stable);
    assert!(stable_text.contains("RELEASE_VERSION=0.9.0\n"));
    assert!(stable_text.contains("PACKAGE_VERSION=0.9.0\n"));
    assert!(stable_text.contains("IS_PRERELEASE=false\n"));

    let rc = run("scripts/release-identity.sh", &["v0.9.0-rc.1"]);
    assert!(rc.status.success(), "{}", stderr(&rc));
    let rc_text = stdout(&rc);
    assert!(rc_text.contains("RELEASE_VERSION=0.9.0-rc.1\n"));
    assert!(rc_text.contains("PACKAGE_VERSION=0.9.0\n"));
    assert!(rc_text.contains("IS_PRERELEASE=true\n"));

    for invalid in ["v0.9.1-rc.1", "v0.9.0-rc.0", "v0.9.0-rc.01", "0.9.0"] {
        let output = run("scripts/release-identity.sh", &[invalid]);
        assert!(
            !output.status.success(),
            "invalid release identity passed: {invalid}"
        );
    }
}

#[test]
#[cfg(unix)]
fn rc_package_manager_urls_use_rc_tag_but_stable_asset_version() {
    let root = temp_dir("rc-identity");
    let checksums = root.join("SHA256SUMS");
    let formula = root.join("rmux.rb");
    let hash = "a".repeat(64);
    fs::write(
        &checksums,
        format!(
            "{hash}  rmux-0.9.0-macos-aarch64.tar.gz\n{hash}  rmux-0.9.0-macos-x86_64.tar.gz\n"
        ),
    )
    .expect("write checksums");
    let output = Command::new(repo_root().join("scripts/generate-homebrew-formula.sh"))
        .args([
            "--version",
            "0.9.0",
            "--release-tag",
            "v0.9.0-rc.1",
            "--checksums",
        ])
        .arg(&checksums)
        .arg("--output")
        .arg(&formula)
        .current_dir(repo_root())
        .output()
        .expect("run Homebrew generator");
    assert!(output.status.success(), "{}", stderr(&output));
    let generated = fs::read_to_string(&formula).expect("read formula");
    assert!(generated.contains("releases/download/v0.9.0-rc.1/rmux-0.9.0-"));
    assert!(generated.contains("version \"0.9.0\""));
    fs::remove_dir_all(root).expect("remove temp directory");
}

#[test]
fn release_workflows_bind_perf_and_do_not_mask_snap_or_ctrl_failures() {
    let release = include_str!("../.github/workflows/release.yml");
    let ci = include_str!("../.github/workflows/ci.yml");
    for workflow in [release, ci] {
        for required in [
            "RMUX_PERF_EXPECTED_GIT_SHA: ${{ github.sha }}",
            "RMUX_PERF_EXPECTED_PLATFORM: linux",
            "RMUX_PERF_EXPECTED_PROVENANCE:",
            "RMUX_PERF_MAX_CURRENT_AGE_SECONDS: \"21600\"",
            "RMUX_PERF_GATE_MODE: portable-budget",
        ] {
            assert!(
                workflow.contains(required),
                "workflow lost perf binding {required:?}"
            );
        }
    }
    let snap_section = release
        .split("name: Publish Snap to latest/candidate")
        .nth(1)
        .expect("Snap publish step");
    assert!(!snap_section
        .lines()
        .take(12)
        .any(|line| line.contains("continue-on-error")));
    assert!(release.contains("if-no-files-found: error"));
    assert!(release.contains("rmux-windows-interactive"));
    assert!(!release.contains("RMUX_WINDOWS_CTRL_MATRIX_EVIDENCE_JSON"));
    assert!(release.contains("\"-Rmux\", $releaseBin"));
    assert!(
        release.contains("$packageHelper = \"target/$env:TARGET/$env:PROFILE_DIR/rmux-full.exe\"")
    );
    assert!(release.contains(
        "$runtimeHelper = \"target/$env:TARGET/$env:PROFILE_DIR/libexec/rmux/rmux.exe\""
    ));
    assert!(release.contains("kind = \"rmux-windows-release-binaries\""));
    assert!(release.contains("-ReuseReleaseBinaries"));
    assert!(release.contains("-ReleaseBinaryManifest"));
    assert!(release.contains("\"--features\", \"tiny-cli\""));
    assert!(release.contains("needs.source-gates.outputs.is_prerelease != 'true'"));
    assert!(release.contains("release_args+=(--prerelease)"));
    assert!(release.contains("--verify-tag"));
    assert!(release.contains("--target \"$SOURCE_GIT_SHA\""));
    assert!(release.contains(
        "group: release-${{ github.event_name == 'workflow_dispatch' && inputs.ref || github.ref_name }}"
    ));
    assert!(release.contains("cancel-in-progress: false"));
    let release_asset_step = release
        .split("- name: Create or update release")
        .nth(1)
        .expect("release asset upload step")
        .split("\n  publish-snap:")
        .next()
        .expect("bounded release asset upload step");
    assert_eq!(release_asset_step.matches("--json isDraft").count(), 2);
    let final_draft_guard = release_asset_step
        .rfind("--json isDraft")
        .expect("final draft guard");
    let asset_upload = release_asset_step
        .find("gh release upload")
        .expect("GitHub release asset upload");
    assert!(final_draft_guard < asset_upload);
    assert!(release_asset_step.contains("Refusing to replace assets on published release $tag"));
    assert!(
        release.matches("scripts/verify-release-ref.sh").count() >= 6,
        "release ref must be revalidated before and after mutable publication boundaries"
    );
    assert_eq!(
        release
            .matches("scripts/verify-release-tag-protection.sh")
            .count(),
        release.matches("scripts/verify-release-ref.sh").count(),
        "every remote tag identity check must also require immutable v* tag rules"
    );
    let tag_protection = include_str!("../scripts/verify-release-tag-protection.sh");
    for required in ["refs/tags/v*", "index(\"update\")", "index(\"deletion\")"] {
        assert!(
            tag_protection.contains(required),
            "release tag protection gate lost {required}"
        );
    }
    assert!(release.contains("RPM-GPG-KEY-rmux-repository"));
    assert_eq!(
        release.matches("uses: actions/checkout@").count(),
        10,
        "every release source checkout must remain covered by this identity assertion"
    );
    assert_eq!(release.matches("ref: ${{ github.sha }}").count(), 1);
    assert_eq!(release.matches("ref: ${{ env.SOURCE_GIT_SHA }}").count(), 9);
    assert_eq!(
        release
            .matches("- name: Verify immutable source checkout")
            .count(),
        9
    );
    assert!(
        !release.contains("ref: ${{ env.RELEASE_REF }}"),
        "mutable release tags must never be used as checkout identities"
    );
}

#[test]
fn release_publication_waits_for_native_and_package_validations() {
    let release = include_str!("../.github/workflows/release.yml");

    let build = release
        .split("\n  build:\n")
        .nth(1)
        .expect("release build job")
        .split("\n  platform-gates:\n")
        .next()
        .expect("bounded release build job");
    assert!(build.contains("rmux-windows-interactive"));

    let platform_gates = release
        .split("\n  platform-gates:\n")
        .nth(1)
        .expect("native platform gates job")
        .split("\n  snap:\n")
        .next()
        .expect("bounded native platform gates job");
    assert!(platform_gates.contains("macos-15-intel"));
    assert!(platform_gates.contains("macos-15"));
    assert!(platform_gates.contains("windows-latest"));
    assert!(platform_gates.contains("name: Windows native release review gate"));
    assert!(platform_gates.contains("release-review-gate-windows.ps1 -SkipPackage -SkipClippy"));
    assert!(platform_gates.contains("name: macOS native runtime smoke"));
    assert!(platform_gates.contains("run: scripts/smoke-macos.sh"));

    let prepare = release
        .split("\n  prepare-release:\n")
        .nth(1)
        .expect("release preparation job")
        .split("\n  publish-snap:\n")
        .next()
        .expect("bounded release preparation job");
    assert!(prepare.contains("name: Prepare signed release draft"));
    assert!(prepare.contains("- source-gates\n      - build\n      - platform-gates\n      - snap"));
    assert!(prepare.contains("--draft"));
    assert!(prepare.contains("gh release upload"));
    for public_mutation in ["--draft=false", "git push", "choco push", "action-publish"] {
        assert!(
            !prepare.contains(public_mutation),
            "release preparation performed public mutation {public_mutation:?}"
        );
    }
    for store_secret in [
        "SNAPCRAFT_STORE_CREDENTIALS",
        "RMUX_PACKAGE_REPO_TOKEN",
        "CHOCOLATEY_API_KEY",
    ] {
        assert!(
            !prepare.contains(store_secret),
            "draft preparation must not receive store secret {store_secret}"
        );
    }

    let winget = release
        .split("\n  validate-winget:\n")
        .nth(1)
        .expect("WinGet validation job")
        .split("\n  validate-chocolatey:\n")
        .next()
        .expect("bounded WinGet validation job");
    assert!(winget.contains("- prepare-release"));
    assert!(winget.contains("release-validation-assets-${{ env.RELEASE_REF }}"));
    assert!(winget.contains("winget validate --manifest"));
    assert!(!winget.contains("gh release download"));

    let chocolatey_validation = release
        .split("\n  validate-chocolatey:\n")
        .nth(1)
        .expect("Chocolatey validation job")
        .split("\n  publish:\n")
        .next()
        .expect("bounded Chocolatey validation job");
    assert!(chocolatey_validation.contains("- prepare-release"));
    assert!(chocolatey_validation.contains("choco pack"));
    assert!(!chocolatey_validation.contains("choco push"));
    assert!(!chocolatey_validation.contains("CHOCOLATEY_API_KEY"));

    let publish = release
        .split("\n  publish:\n")
        .nth(1)
        .expect("canonical publication job")
        .split("\n  publish-linux-repositories:\n")
        .next()
        .expect("bounded canonical publication job");
    for prerequisite in [
        "- prepare-release",
        "- validate-external-configuration",
        "- validate-winget",
        "- validate-chocolatey",
    ] {
        assert!(publish.contains(prerequisite));
    }
    assert!(publish.contains("name: Publish canonical GitHub release"));
    assert!(publish.contains("--draft=false"));
    assert!(publish.contains("cannot be made transactionally atomic"));
    assert!(publish.contains("is already public"));
    for store_secret in [
        "SNAPCRAFT_STORE_CREDENTIALS",
        "RMUX_PACKAGE_REPO_TOKEN",
        "CHOCOLATEY_API_KEY",
    ] {
        assert!(
            !publish.contains(store_secret),
            "canonical publication must not receive store secret {store_secret}"
        );
    }

    let linux_publish = release
        .split("\n  publish-linux-repositories:\n")
        .nth(1)
        .expect("Linux repository publication job")
        .split("\n  publish-chocolatey:\n")
        .next()
        .expect("bounded Linux repository publication job");
    assert!(linux_publish.contains("- publish"));
    assert!(linux_publish.contains("git push"));

    let snap_publish = release
        .split("\n  publish-snap:\n")
        .nth(1)
        .expect("Snap publication job")
        .split("\n  validate-winget:\n")
        .next()
        .expect("bounded Snap publication job");
    assert!(snap_publish.contains("- publish"));
    assert!(snap_publish.contains("snapcore/action-publish@"));

    let chocolatey_publish = release
        .split("\n  publish-chocolatey:\n")
        .nth(1)
        .expect("Chocolatey publication job");
    assert!(chocolatey_publish.contains("- publish"));
    assert!(chocolatey_publish.contains("- validate-chocolatey"));
    assert!(chocolatey_publish.contains("choco push"));
}

#[test]
fn workflows_install_the_workspace_toolchain_instead_of_floating_stable() {
    let ci = include_str!("../.github/workflows/ci.yml");
    let release = include_str!("../.github/workflows/release.yml");

    for (name, workflow, deliberate_other_toolchains) in
        [("CI", ci, 1_usize), ("release", release, 0_usize)]
    {
        assert!(
            !workflow.contains("toolchain: stable"),
            "{name} installs components on floating stable even though rust-toolchain.toml pins 1.96.1"
        );
        let action_count = workflow.matches("uses: dtolnay/rust-toolchain@").count();
        let workspace_pin_count = workflow.matches("toolchain: \"1.96.1\"").count();
        assert_eq!(
            workspace_pin_count + deliberate_other_toolchains,
            action_count,
            "every {name} rust-toolchain action must install the workspace pin; only the byte-reproducible WASM job may use its separate recorded compiler"
        );
    }
}

#[test]
fn every_ci_and_release_job_has_a_bounded_runtime() {
    for (workflow_name, workflow) in [
        ("ci", include_str!("../.github/workflows/ci.yml")),
        ("release", include_str!("../.github/workflows/release.yml")),
        (
            "scorecard",
            include_str!("../.github/workflows/scorecard.yml"),
        ),
    ] {
        let jobs = workflow
            .split_once("\njobs:\n")
            .map(|(_, jobs)| jobs)
            .expect("workflow jobs section");
        let mut current_job: Option<&str> = None;
        let mut current_has_timeout = false;

        for line in jobs.lines().chain(std::iter::once("  end:")) {
            let is_job_header =
                line.starts_with("  ") && !line.starts_with("    ") && line.ends_with(':');
            if is_job_header {
                if let Some(job) = current_job {
                    assert!(
                        current_has_timeout,
                        "{workflow_name} job {job} must set timeout-minutes"
                    );
                }
                current_job = Some(line.trim_end_matches(':').trim());
                current_has_timeout = false;
            } else if line.trim_start().starts_with("timeout-minutes:") {
                current_has_timeout = true;
            }
        }
    }
}

#[test]
fn windows_package_reuses_the_exact_ctrl_tested_release_binaries() {
    let release = include_str!("../.github/workflows/release.yml");
    let package = include_str!("../scripts/package-windows.ps1");

    for required in [
        "rmux-windows-release-binaries",
        "binary_sha256",
        "helper_binary_sha256",
        "daemon_binary_sha256",
        "-ReuseReleaseBinaries",
        "-ReleaseBinaryManifest",
    ] {
        assert!(
            release.contains(required) && package.contains(required),
            "Windows release binary reuse lost {required}"
        );
    }
    assert!(package.contains("-SkipBuild and -ReuseReleaseBinaries are mutually exclusive"));
    assert!(package.contains("-ReuseReleaseBinaries requires a clean tracked worktree"));
    assert!(package.contains("release binary manifest Git commit does not match HEAD"));
    assert!(package.contains("-not $SkipBuild -and -not $ReuseReleaseBinaries"));

    let package_step = release
        .split("- name: Create Windows archive")
        .nth(1)
        .expect("Windows archive step");
    let package_step = package_step
        .split("- uses: actions/upload-artifact")
        .next()
        .expect("bounded Windows archive step");
    assert!(package_step.contains("-ReuseReleaseBinaries"));
    assert!(package_step.contains("target/windows-ctrl-matrix/release-binaries.json"));
}

#[test]
fn windows_installer_transactions_complete_package_and_preserves_failed_rollback() {
    let installer = include_str!("../scripts/install-windows.ps1");
    let transaction = installer
        .split_once("function Install-PackageFileSet")
        .map(|(_, transaction)| transaction)
        .and_then(|transaction| transaction.split_once("function Test-PackageRoot"))
        .map(|(transaction, _)| transaction)
        .expect("bounded Install-PackageFileSet function");

    let initial_state = transaction
        .find("$preserveTransactionBackup = $false")
        .expect("backup cleanup is enabled by default");
    let transaction_body = transaction.find("try {").expect("transaction body");
    assert!(initial_state < transaction_body);

    let rollback_failure = transaction
        .split_once("if ($rollbackErrors.Count -gt 0)")
        .map(|(_, failure)| failure)
        .and_then(|failure| failure.split_once("previous package restored"))
        .map(|(failure, _)| failure)
        .expect("bounded rollback-failure branch");
    assert!(rollback_failure.contains("$preserveTransactionBackup = $true"));
    assert!(rollback_failure.contains("recovery backup preserved at"));
    assert!(rollback_failure.contains("Stop running rmux processes"));
    assert!(rollback_failure.contains("restore"));

    let cleanup = transaction
        .rsplit_once("} finally {")
        .map(|(_, cleanup)| cleanup)
        .expect("transaction cleanup");
    let cleanup_guard = cleanup
        .find("if (-not $preserveTransactionBackup)")
        .expect("cleanup guard");
    let remove_backup = cleanup
        .find("Remove-Item -Recurse -Force")
        .expect("backup cleanup");
    assert!(cleanup_guard < remove_backup);

    assert_eq!(
        transaction
            .matches("$preserveTransactionBackup = $false")
            .count(),
        1,
        "normal success and a successful rollback must retain the default cleanup state"
    );
    assert_eq!(
        transaction.matches("$preserveTransactionBackup = $true").count(),
        1,
        "only a failed rollback may retain the backup; normal success and a successful rollback must still clean it up"
    );

    let package_install = installer
        .split_once("function Install-PackageRoot")
        .map(|(_, install)| install)
        .and_then(|install| install.split_once("if ([string]::IsNullOrWhiteSpace($InstallDir))"))
        .map(|(install, _)| install)
        .expect("bounded Install-PackageRoot function");
    let share_plan = package_install
        .find("Get-ChildItem -LiteralPath $shareSource -Recurse -File -Force")
        .expect("share files are enumerated into the package transaction");
    let root_plan = package_install
        .find("foreach ($optional in")
        .expect("optional root files are enumerated into the package transaction");
    let commit = package_install
        .find("Install-PackageFileSet $installPlan $destination $Verify")
        .expect("one package transaction installs the complete plan");
    assert!(share_plan < commit && root_plan < commit);
    assert!(!package_install.contains("Copy-Tree"));
    assert!(transaction.contains("Invoke-InstallCheckpoint \"after-copy-package\""));
}

#[test]
fn perf_current_and_darwin_baseline_validation_fail_closed_on_identity_drift() {
    let bench = include_str!("../scripts/perf-bench.sh");
    let baseline_generator = include_str!("../scripts/perf-baseline.sh");
    let comparator = include_str!("../scripts/perf-diff.py");
    let baseline_check = include_str!("../scripts/check-perf-baseline.py");
    let gate = include_str!("../scripts/release-review-gate.sh");
    for required in [
        "rmux-perf-current",
        "expected_git_commit",
        "host_fingerprint",
        "binary_sha256",
        "build_mode",
    ] {
        assert!(
            bench.contains(required),
            "perf current artifact lost {required}"
        );
    }
    for required in [
        "--expected-current-commit",
        "--expected-current-host-fingerprint",
        "--expected-current-provenance",
        "--max-current-age-seconds",
    ] {
        assert!(
            comparator.contains(required),
            "perf comparator lost {required}"
        );
    }
    assert!(baseline_check.contains("baseline was recorded from a dirty worktree"));
    assert!(baseline_check.contains("Darwin baseline is missing an explicit clean-worktree stamp"));
    assert!(baseline_check.contains("missing or invalid environment.host_fingerprint"));
    assert!(baseline_check.contains("does not match expected"));
    assert!(baseline_check.contains("personal home path leaked"));
    assert!(baseline_check.contains("must be repository-relative"));
    assert!(baseline_generator.contains("write_portable_source"));
    assert!(baseline_generator.contains("python3 scripts/check-perf-baseline.py"));
    assert!(baseline_generator.contains("\"$json_path\""));
    assert!(baseline_generator.contains("--expected-platform \"$platform\""));
    for (name, baseline) in [
        (
            "Darwin",
            include_str!("../benches/perf/baselines/release-0.9.0.json"),
        ),
        (
            "Linux",
            include_str!("../benches/perf/baselines/release-0.9.0-linux.json"),
        ),
    ] {
        assert!(
            !baseline.contains("/Users/") && !baseline.contains("/home/"),
            "{name} perf baseline leaked a personal home path"
        );
    }
    assert!(gate.contains("missing required Darwin perf baseline"));
    assert!(gate.contains("run this mandatory comparison on the baseline owner host"));
    assert!(gate.contains("perf-gate=portable-budget enforcement=absolute-budgets"));
    assert!(gate.contains("scripts/check-perf-current.py"));
}

#[test]
fn debug_configuration_cannot_produce_or_satisfy_release_artifact_metadata() {
    for producer in [
        include_str!("../scripts/package-unix.sh"),
        include_str!("../scripts/package-debian.sh"),
        include_str!("../scripts/package-rpm.sh"),
    ] {
        assert!(
            producer.contains("[ \"$configuration\" != \"release\" ]"),
            "shell package producer must make debug release_artifact impossible"
        );
    }
    assert!(include_str!("../scripts/package-windows.ps1")
        .contains("($Configuration -eq \"release\") -and"));

    for verifier in [
        include_str!("../scripts/verify-package.sh"),
        include_str!("../scripts/verify-debian-package.sh"),
        include_str!("../scripts/verify-rpm-package.sh"),
    ] {
        assert!(verifier.contains("release artifact metadata configuration is not release"));
    }
    assert!(include_str!("../scripts/verify-package-windows.ps1")
        .contains("release artifact metadata configuration is not release"));
}

#[test]
fn packaged_artifact_metadata_never_embeds_builder_paths() {
    let unix = include_str!("../scripts/package-unix.sh");
    for expected in [
        "\"binary_path\": \"bin/rmux\"",
        "\"helper_binary_path\": \"libexec/rmux/rmux\"",
        "\"daemon_binary_path\": \"bin/rmux-daemon\"",
    ] {
        assert!(unix.contains(expected), "Unix metadata lost {expected}");
    }

    for (name, producer) in [
        ("Debian", include_str!("../scripts/package-debian.sh")),
        ("RPM", include_str!("../scripts/package-rpm.sh")),
    ] {
        for expected in [
            "\"binary_path\": \"/usr/bin/rmux\"",
            "\"helper_binary_path\": \"/usr/libexec/rmux/rmux\"",
            "\"daemon_binary_path\": \"/usr/bin/rmux-daemon\"",
        ] {
            assert!(
                producer.contains(expected),
                "{name} metadata lost {expected}"
            );
        }
    }

    let windows = include_str!("../scripts/package-windows.ps1");
    for expected in [
        "binary_path = \"rmux.exe\"",
        "helper_binary_path = \"libexec/rmux/rmux.exe\"",
        "daemon_binary_path = \"rmux-daemon.exe\"",
    ] {
        assert!(
            windows.contains(expected),
            "Windows metadata lost {expected}"
        );
    }
    assert!(!unix.contains("\"binary_path\": \"$(printf"));
    assert!(!windows.contains("binary_path = $binaryAbs"));
}

#[test]
fn distro_package_glibc_floor_is_derived_and_verified_for_all_binaries() {
    let deb = include_str!("../scripts/package-debian.sh");
    let rpm = include_str!("../scripts/package-rpm.sh");
    for producer in [deb, rpm] {
        for required in [
            "binary_glibc_min",
            "helper_binary_glibc_min",
            "daemon_binary_glibc_min",
            "package_glibc_min",
            "max_supported_glibc",
            "glibc-symbol-floor.sh",
        ] {
            assert!(producer.contains(required), "producer lost {required}");
        }
    }
    assert!(deb.contains("Depends: libc6 (>= $package_glibc_min)"));
    assert!(rpm.contains("Requires: glibc >= $package_glibc_min"));
    assert!(deb.contains("newer than supported GLIBC_"));
    assert!(rpm.contains("newer than supported GLIBC_"));
    assert!(
        include_str!("../scripts/verify-debian-package.sh").contains("older than imported GLIBC_")
    );
    assert!(include_str!("../scripts/verify-rpm-package.sh").contains("older than imported GLIBC_"));
}

#[test]
#[cfg(unix)]
fn glibc_floor_helper_selects_newest_imported_symbol() {
    let root = temp_dir("glibc-floor");
    let fake_readelf = root.join("readelf");
    let first = root.join("first");
    let second = root.join("second");
    fs::write(&first, b"elf").expect("write first fixture");
    fs::write(&second, b"elf").expect("write second fixture");
    fs::write(
        &fake_readelf,
        "#!/bin/sh\ncase \"$2\" in *first) echo 'Name: GLIBC_2.31';; *) echo 'Name: GLIBC_2.34'; echo 'Name: GLIBC_2.17';; esac\n",
    )
    .expect("write fake readelf");
    make_executable(&fake_readelf);
    let output = Command::new(repo_root().join("scripts/glibc-symbol-floor.sh"))
        .arg(&first)
        .arg(&second)
        .env("READELF", &fake_readelf)
        .current_dir(repo_root())
        .output()
        .expect("run glibc floor helper");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "2.34");
    fs::remove_dir_all(root).expect("remove temp directory");
}

#[test]
fn rss_fd_detector_uses_proc_identity_not_ps_awk_text_matching() {
    let script = include_str!("../scripts/smoke-rss-fd-drift-unix.sh");
    assert!(script.contains("/proc/$pid/exe"));
    assert!(script.contains("--__internal-daemon"));
    assert!(script.contains("process_has_exact_argument"));
    assert!(!script.contains("ps -eo pid=,args= | awk"));
}

#[test]
fn public_readmes_do_not_advertise_unmanaged_windows_installer() {
    for (path, readme) in [
        ("README.md", include_str!("../README.md")),
        (
            "docs/i18n/README.fr.md",
            include_str!("../docs/i18n/README.fr.md"),
        ),
        (
            "docs/i18n/README.ja.md",
            include_str!("../docs/i18n/README.ja.md"),
        ),
        (
            "docs/i18n/README.zh-CN.md",
            include_str!("../docs/i18n/README.zh-CN.md"),
        ),
    ] {
        assert!(
            !readme.contains("https://rmux.io/install.ps1"),
            "{path} advertises an installer whose source and deployment are external"
        );
        for required in ["rmux.exe", "rmux-daemon.exe", "libexec/rmux/rmux.exe"] {
            assert!(
                readme.contains(required),
                "{path} must preserve the Windows package component {required}"
            );
        }
    }

    let releasing = include_str!("../RELEASING.md");
    assert!(releasing
        .contains("The `rmux.io` install-script sources and deployment pipeline are not part of"));
    assert!(releasing.contains("copying only `rmux.exe` is not valid"));
}

#[cfg(target_os = "linux")]
#[test]
fn rss_fd_detector_does_not_match_foreign_process_arguments() {
    let output = Command::new(repo_root().join("scripts/smoke-rss-fd-drift-unix.sh"))
        .env("RMUX_RSS_FD_PROCESS_SCAN_SELF_TEST", "1")
        .current_dir(repo_root())
        .output()
        .expect("run RSS/FD scanner self-test");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("process scanner self-test passed"));
}

#[test]
#[cfg(unix)]
fn rpm_repository_publishes_both_package_and_repodata_key_urls() {
    let root = temp_dir("rpm-repository-keys");
    let tools = root.join("tools");
    let input = root.join("input");
    let output = root.join("output");
    fs::create_dir_all(&tools).expect("create fake tool directory");
    fs::create_dir_all(&input).expect("create RPM input directory");
    fs::write(input.join("rmux-0.9.0-1.x86_64.rpm"), b"rpm").expect("write fake RPM");

    let createrepo = tools.join("createrepo_c");
    fs::write(
        &createrepo,
        "#!/bin/sh\nset -eu\nmkdir -p \"$1/repodata\"\nprintf metadata > \"$1/repodata/repomd.xml\"\n",
    )
    .expect("write fake createrepo");
    make_executable(&createrepo);
    let rpmsign = tools.join("rpmsign");
    fs::write(&rpmsign, "#!/bin/sh\nexit 0\n").expect("write fake rpmsign");
    make_executable(&rpmsign);
    let gpg = tools.join("gpg");
    fs::write(
        &gpg,
        "#!/bin/sh\nset -eu\ncase \" $* \" in\n  *\" --with-colons --fingerprint \"*)\n    last=\n    for arg in \"$@\"; do last=$arg; done\n    case $last in package-key) fpr=PACKAGEFINGERPRINT ;; repository-key) fpr=REPOSITORYFINGERPRINT ;; *) exit 1 ;; esac\n    printf 'fpr:::::::::%s:\\n' \"$fpr\"\n    exit 0\n    ;;\nesac\nout=\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = --output ]; then out=$2; shift 2; else shift; fi\ndone\n[ -n \"$out\" ]\nprintf signature > \"$out\"\n",
    )
    .expect("write fake gpg");
    make_executable(&gpg);

    let path = format!(
        "{}:{}",
        tools.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let result = Command::new(repo_root().join("scripts/generate-rpm-repository.sh"))
        .args(["--input-dir"])
        .arg(&input)
        .args(["--output-dir"])
        .arg(&output)
        .args([
            "--baseurl",
            "https://packages.example/rpm",
            "--gpg-key-url",
            "https://packages.example/rpm/package.asc",
            "--repo-gpg-key-url",
            "https://packages.example/rpm/repodata.asc",
            "--rpm-signing-key",
            "package-key",
            "--repo-signing-key",
            "repository-key",
        ])
        .env("PATH", path)
        .current_dir(repo_root())
        .output()
        .expect("run RPM repository generator");
    assert!(result.status.success(), "{}", stderr(&result));
    let config = fs::read_to_string(output.join("rmux.repo")).expect("read generated repo file");
    assert!(config.contains(
        "gpgkey=https://packages.example/rpm/package.asc https://packages.example/rpm/repodata.asc"
    ));
    assert!(config.contains("gpgcheck=1"));
    assert!(config.contains("repo_gpgcheck=1"));
    fs::remove_dir_all(root).expect("remove temp directory");
}

#[test]
#[cfg(unix)]
fn rpm_repository_rejects_reusing_the_package_signing_key() {
    let root = temp_dir("rpm-repo-same-key");
    let input = root.join("input");
    let output = root.join("output");
    fs::create_dir_all(&input).expect("create RPM input directory");
    fs::write(input.join("rmux-0.9.0-1.x86_64.rpm"), b"rpm").expect("write fake RPM");

    let result = Command::new(repo_root().join("scripts/generate-rpm-repository.sh"))
        .args(["--input-dir"])
        .arg(&input)
        .args(["--output-dir"])
        .arg(&output)
        .args([
            "--rpm-signing-key",
            "same-key",
            "--repo-signing-key",
            "same-key",
        ])
        .current_dir(repo_root())
        .output()
        .expect("run RPM repository generator");
    assert!(!result.status.success());
    assert!(stderr(&result).contains("signing keys must be distinct"));
    fs::remove_dir_all(root).expect("remove temp directory");
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).expect("read permissions").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("set executable permissions");
}
