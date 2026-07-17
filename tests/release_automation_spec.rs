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
        "has no allowlist entry",
        "stale product-divergence references",
        "reference points to an untracked path",
        "cites {name} from {cited_path}, expected {path}",
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
    let normalized = current_release
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    assert!(
        normalized.contains(&expected),
        "0.9.0 changelog must record the current detached wire version: {expected}"
    );
    assert!(
        normalized.contains("already-running older server must be restarted"),
        "0.9.0 changelog must tell operators that this hard wire cut requires a server restart"
    );
}

#[test]
fn release_scripts_retain_non_bypassable_rc_tag_provenance() {
    let release_identity = include_str!("../scripts/release-identity.sh");
    let protection = include_str!("../scripts/verify-release-tag-protection.sh");

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
fn windows_installer_uses_stable_package_names_for_rc_tags() {
    let installer = include_str!("../scripts/install-windows.ps1");

    assert!(installer.contains("$packageVersion = $semver -replace '-.*$', ''"));
    assert!(installer.contains("$archive = \"rmux-$packageVersion-$platform.zip\""));
    assert!(installer.contains("Join-Path $tmp \"rmux-$packageVersion-$platform\""));
    assert!(!installer.contains("$archive = \"rmux-$semver-$platform.zip\""));
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
    assert!(release.contains("- os: windows-latest"));
    assert!(!release.contains("self-hosted"));
    assert!(!release.contains("rmux-windows-interactive"));
    assert!(!release.contains("-PortableSmokeOnly"));
    assert!(!release.contains("portable-smoke.evidence.json"));
    assert!(!release.contains("RMUX_WINDOWS_CTRL_MATRIX_EVIDENCE_JSON"));
    assert!(release.contains("& \"./scripts/windows_ctrl_matrix.ps1\" -StaticMatrixSpec"));
    assert!(!release.contains("Run \"./scripts/windows_ctrl_matrix.ps1\" @(\"-StaticMatrixSpec\")"));
    assert!(release.contains("& \"./scripts/assert-windows-static-crt.ps1\" `"));
    assert!(!release.contains("Run \"./scripts/assert-windows-static-crt.ps1\" @("));
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
    assert!(release.contains("group: rmux-release"));
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
fn github_windows_tests_keep_debug_daemons_inside_the_runner_job() {
    let ci = include_str!("../.github/workflows/ci.yml");
    let release = include_str!("../.github/workflows/release.yml");
    let opt_in = "RMUX_ALLOW_INTERNAL_DAEMON_IN_CALLER_JOB: \"1\"";

    assert_eq!(
        ci.matches(opt_in).count(),
        2,
        "both Windows CI test surfaces must opt into the runner-owned job"
    );
    assert_eq!(
        release.matches(opt_in).count(),
        1,
        "only the GitHub-hosted Windows release gate may opt into the runner-owned job"
    );
}

#[test]
fn release_publication_waits_for_native_and_package_validations() {
    let release = include_str!("../.github/workflows/release.yml");
    assert!(release.contains("concurrency:\n  group: rmux-release\n  cancel-in-progress: false"));

    let build = release
        .split("\n  build:\n")
        .nth(1)
        .expect("release build job")
        .split("\n  platform-gates:\n")
        .next()
        .expect("bounded release build job");
    assert!(build.contains("- os: windows-latest"));
    assert!(!build.contains("self-hosted"));
    assert!(!build.contains("rmux-windows-interactive"));

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
    let current_assets = prepare
        .find("sha256sum --check --strict SHA256SUMS")
        .expect("current release asset validation");
    let retained_history = prepare
        .find("scripts/retain-linux-package-history.py")
        .expect("authenticated Linux package history import");
    let repository_base_capture = prepare
        .find("git -C target/package-repository-history rev-parse --verify 'HEAD^{commit}'")
        .expect("package repository base capture");
    let repository_generation = prepare
        .find("- name: Generate Linux package repositories")
        .expect("Linux repository generation");
    assert!(current_assets < retained_history);
    assert!(repository_base_capture < retained_history);
    assert!(retained_history < repository_generation);
    let retention_step = &prepare[retained_history..repository_generation];
    assert!(retention_step.contains("target/package-repository-inputs"));
    assert!(retention_step.contains("--staging-dir target/package-repository-inputs"));
    assert!(!retention_step.contains("--assets-dir release-assets"));
    for required_architecture in [
        "--apt-architecture amd64",
        "--apt-architecture arm64",
        "--rpm-architecture x86_64",
        "--rpm-architecture aarch64",
    ] {
        assert!(retention_step.contains(required_architecture));
    }
    let repository_step = &prepare[repository_generation..];
    assert_eq!(
        repository_step
            .matches("--input-dir target/package-repository-inputs")
            .count(),
        2,
        "both repository generators must consume isolated N/N-1 staging"
    );
    assert!(repository_step.contains("PACKAGE_REPOSITORY_BASE"));
    assert!(!repository_step.contains("--input-dir release-assets"));
    for generated_architecture in ["--architecture amd64", "--architecture arm64"] {
        assert!(repository_step.contains(generated_architecture));
    }
    assert!(repository_step.contains("--rpm-signing-version \"$PACKAGE_VERSION\""));
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
    let repository_validation = linux_publish
        .find("sha256sum --check --strict SHA256SUMS")
        .expect("downloaded repository validation");
    let repository_base_guard = linux_publish
        .find("scripts/verify-package-repository-base.sh")
        .expect("package repository compare-and-swap guard");
    let repository_replace = linux_publish
        .find("rm -rf \"$work/debian\" \"$work/rpm\"")
        .expect("package repository replacement");
    let repository_stage = linux_publish
        .find("git add _headers index.html debian rpm")
        .expect("atomic repository staging");
    let repository_commit = linux_publish
        .find("git commit -m")
        .expect("atomic repository commit");
    let repository_push = linux_publish
        .find("git push")
        .expect("atomic repository push");
    assert!(repository_validation < repository_stage);
    assert!(repository_validation < repository_base_guard);
    assert!(repository_base_guard < repository_replace);
    assert!(repository_stage < repository_commit);
    assert!(repository_commit < repository_push);
    assert!(!linux_publish.contains("git push --force"));

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
fn windows_package_reuses_the_exact_release_tested_binaries() {
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
    assert!(package_step.contains("target/windows-release-evidence/release-binaries.json"));
}

#[test]
fn windows_installer_transactions_complete_package_and_preserves_failed_rollback() {
    let installer = include_str!("../scripts/install-windows.ps1");
    let lock = installer
        .split_once("function Enter-InstallTransactionLock")
        .map(|(_, lock)| lock)
        .and_then(|lock| lock.split_once("function Exit-InstallTransactionLock"))
        .map(|(lock, _)| lock)
        .expect("bounded installer transaction-lock acquisition");
    assert!(lock.contains("[System.IO.FileMode]::OpenOrCreate"));
    assert!(lock.contains("[System.IO.FileShare]::None"));
    assert!(lock.contains("[System.IO.IOException]"));
    assert!(lock.contains("$_.Exception.HResult -band 0xFFFF"));
    assert!(lock.contains("AddSeconds(30)"));
    assert!(lock.contains("Start-Sleep -Milliseconds 50"));

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
    let lock_enter = package_install
        .find("$installLock = Enter-InstallTransactionLock $installRoot")
        .expect("package transaction acquires the destination-scoped lock");
    let lock_exit = package_install
        .find("Exit-InstallTransactionLock $installLock")
        .expect("package transaction releases the destination-scoped lock");
    assert!(lock_enter < commit && commit < lock_exit);
    assert!(package_install[lock_enter..lock_exit].contains("} finally {"));
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
    fs::write(input.join("rmux-0.8.0-1.x86_64.rpm"), b"historical-rpm")
        .expect("write retained RPM");
    fs::write(
        input.join("rmux-0.9.0-legacy.x86_64.rpm"),
        b"legacy-historical-rpm",
    )
    .expect("write legacy-named retained RPM");

    let createrepo = tools.join("createrepo_c");
    fs::write(
        &createrepo,
        "#!/bin/sh\nset -eu\nmkdir -p \"$1/repodata\"\nprintf metadata > \"$1/repodata/repomd.xml\"\n",
    )
    .expect("write fake createrepo");
    make_executable(&createrepo);
    let rpmsign = tools.join("rpmsign");
    fs::write(
        &rpmsign,
        "#!/bin/sh\nset -eu\nlast=\nfor arg in \"$@\"; do last=$arg; done\nprintf '%s\\n' \"${last##*/}\" >> \"$RPM_SIGN_LOG\"\nprintf '%s' signed >> \"$last\"\n",
    )
    .expect("write fake rpmsign");
    make_executable(&rpmsign);
    let rpm = tools.join("rpm");
    fs::write(
        &rpm,
        "#!/bin/sh\nset -eu\nlast=\nfor arg in \"$@\"; do last=$arg; done\ncase \"${last##*/}\" in\n  rmux-0.9.0-legacy.*) printf 'rmux\\t0.8.0' ;;\n  rmux-0.9.0-*) printf 'rmux\\t0.9.0' ;;\n  rmux-0.8.0-*) printf 'rmux\\t0.8.0' ;;\n  *) exit 92 ;;\nesac\n",
    )
    .expect("write fake rpm query");
    make_executable(&rpm);
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
            "--rpm-signing-version",
            "0.9.0",
            "--repo-signing-key",
            "repository-key",
        ])
        .env("RPM_SIGN_LOG", root.join("rpm-sign.log"))
        .env("PATH", &path)
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
    assert_eq!(
        fs::read(output.join("rmux-0.8.0-1.x86_64.rpm")).expect("read retained RPM"),
        b"historical-rpm",
        "a retained package served from an immutable URL must stay byte-identical"
    );
    assert_eq!(
        fs::read(output.join("rmux-0.9.0-legacy.x86_64.rpm"))
            .expect("read legacy-named retained RPM"),
        b"legacy-historical-rpm",
        "signing selection must use authenticated RPM metadata, not a legacy filename"
    );
    assert_eq!(
        fs::read(output.join("rmux-0.9.0-1.x86_64.rpm")).expect("read current RPM"),
        b"rpmsigned"
    );
    assert_eq!(
        fs::read_to_string(root.join("rpm-sign.log")).expect("read RPM signing log"),
        "rmux-0.9.0-1.x86_64.rpm\n"
    );

    let rejected_output = root.join("rejected-output");
    let rejected = Command::new(repo_root().join("scripts/generate-rpm-repository.sh"))
        .args(["--input-dir"])
        .arg(&input)
        .args(["--output-dir"])
        .arg(&rejected_output)
        .args([
            "--rpm-signing-key",
            "package-key",
            "--rpm-signing-version",
            "0.9.1",
        ])
        .env("RPM_SIGN_LOG", root.join("rejected-rpm-sign.log"))
        .env("PATH", &path)
        .current_dir(repo_root())
        .output()
        .expect("run RPM repository generator without a current package");
    assert!(!rejected.status.success());
    assert!(stderr(&rejected).contains("no RPM metadata matched current version 0.9.1"));
    assert!(!root.join("rejected-rpm-sign.log").exists());
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

#[test]
#[cfg(unix)]
fn linux_repository_history_is_authenticated_before_retention() {
    let root = temp_dir("linux-package-history");
    let tools = root.join("tools");
    let repository = root.join("repository");
    let release_assets = root.join("release-assets");
    let staging = root.join("staging");
    let apt_suite = repository.join("debian/dists/stable");
    let apt_index = apt_suite.join("main/binary-amd64/Packages");
    let apt_pool = repository.join("debian/pool/main/r/rmux");
    let rpm_repository = repository.join("rpm");
    for directory in [
        &tools,
        &release_assets,
        &staging,
        &apt_pool,
        &rpm_repository,
    ] {
        fs::create_dir_all(directory).expect("create package-history fixture directory");
    }
    fs::create_dir_all(apt_index.parent().expect("APT index parent"))
        .expect("create APT index directory");

    let older_apt_package = apt_pool.join("rmux_0.7.0_amd64.deb");
    fs::write(&older_apt_package, b"deb-070").expect("write older Debian package");
    let apt_package = apt_pool.join("rmux_0.8.0_amd64.deb");
    fs::write(&apt_package, b"deb-080").expect("write predecessor Debian package");
    let packages = concat!(
        "Package: rmux\n",
        "Version: 0.7.0\n",
        "Architecture: amd64\n",
        "Filename: pool/main/r/rmux/rmux_0.7.0_amd64.deb\n",
        "Size: 7\n",
        "SHA256: 755414877800f72c0580c901cfcc5d38ea19b1ba2c8997f3164e46cec0a169f7\n",
        "\n",
        "Package: rmux\n",
        "Version: 0.8.0\n",
        "Architecture: amd64\n",
        "Filename: pool/main/r/rmux/rmux_0.8.0_amd64.deb\n",
        "Size: 7\n",
        "SHA256: 76366e852f2efac474a52363e451aadaed5d6cbece8ab61a67f184e670f3e93d\n",
        "\n"
    );
    fs::write(&apt_index, packages).expect("write signed APT index fixture");
    fs::write(
        apt_suite.join("Release"),
        concat!(
            "Origin: RMUX\n",
            "SHA256:\n",
            " 6764250dd5a01cfff3e7c7a022831339f4b3b736141ef9d56ef166ba5f123d9b 358 main/binary-amd64/Packages\n"
        ),
    )
    .expect("write APT Release fixture");
    fs::write(apt_suite.join("Release.gpg"), b"signature").expect("write APT signature fixture");
    fs::write(
        rpm_repository.join("rmux-0.7.0-1.x86_64.rpm"),
        b"older-signed-rpm",
    )
    .expect("write older RPM fixture");
    let predecessor_rpm = rpm_repository.join("rmux-0.8.0-1.x86_64.rpm");
    fs::write(&predecessor_rpm, b"signed-rpm").expect("write retained RPM fixture");
    let outside_matrix_rpm = rpm_repository.join("rmux-0.8.0-1.i686.rpm");
    fs::write(&outside_matrix_rpm, b"signed-rpm-other-architecture")
        .expect("write RPM fixture for an architecture outside the release matrix");
    fs::write(release_assets.join("rmux_0.9.0_amd64.deb"), b"current-deb")
        .expect("write current Debian package");
    fs::write(
        release_assets.join("rmux-0.9.0-1.x86_64.rpm"),
        b"current-rpm",
    )
    .expect("write current RPM package");
    for current in ["rmux_0.9.0_amd64.deb", "rmux-0.9.0-1.x86_64.rpm"] {
        fs::copy(release_assets.join(current), staging.join(current))
            .expect("stage current release package");
    }

    let apt_fingerprint = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let rpm_fingerprint = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
    let alternate_rpm_fingerprint = "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";
    let rpm_subkey_fingerprint = "DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD";
    let gpg = tools.join("gpg");
    fs::write(
        &gpg,
        format!(
            "#!/bin/sh\nset -eu\ncase \" $* \" in\n  *\" --with-colons --fingerprint apt-key \"*)\n    printf 'pub:::::::::\\nfpr:::::::::{apt_fingerprint}:\\n'\n    ;;\n  *\" --with-colons --fingerprint rpm-key \"*)\n    printf 'pub:::::::::\\nfpr:::::::::{rpm_fingerprint}:\\nsub:::::::::\\nfpr:::::::::{rpm_subkey_fingerprint}:\\n'\n    if [ \"${{FAKE_AMBIGUOUS_RPM_SELECTOR:-0}}\" = 1 ]; then\n      printf 'pub:::::::::\\nfpr:::::::::{alternate_rpm_fingerprint}:\\n'\n    fi\n    ;;\n  *\" --status-fd 1 --verify \"*)\n    printf '[GNUPG:] VALIDSIG {apt_fingerprint} 2026 0 0 0 0 0 0 0 {apt_fingerprint}\\n'\n    ;;\n  *\" --armor --export {rpm_fingerprint} \"*) printf '%s\\n' 'PUBLIC KEY' ;;\n  *) exit 91 ;;\nesac\n"
        ),
    )
    .expect("write fake gpg");
    make_executable(&gpg);
    let rpmkeys = tools.join("rpmkeys");
    fs::write(
        &rpmkeys,
        "#!/bin/sh\nset -eu\ncase \" $* \" in\n  *\" --import \"*) exit 0 ;;\n  *\" --checksig \"*)\n    if [ \"${FAKE_RPM_DIGEST_ONLY:-0}\" = 1 ]; then\n      printf '%s\\n' 'package.rpm: digests OK'\n    else\n      printf '%s\\n' 'package.rpm: digests signatures OK'\n    fi\n    exit 0\n    ;;\n  *) exit 92 ;;\nesac\n",
    )
    .expect("write fake rpmkeys");
    make_executable(&rpmkeys);
    let rpm = tools.join("rpm");
    fs::write(
        &rpm,
        "#!/bin/sh\nset -eu\n[ \"$1\" = -qp ]\ncase \" $* \" in *0.7.0*) version=0.7.0 ;; *0.8.0*) version=0.8.0 ;; *) exit 93 ;; esac\ncase \" $* \" in *i686*) architecture=i686 ;; *) architecture=x86_64 ;; esac\nprintf 'rmux\\n%s\\n%s\\n' \"$version\" \"$architecture\"\n",
    )
    .expect("write fake rpm");
    make_executable(&rpm);

    let path = format!(
        "{}:{}",
        tools.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let run_retention = |current_version: &str,
                         apt_architecture: &str,
                         rpm_architecture: &str,
                         digest_only: bool,
                         ambiguous_rpm_selector: bool| {
        Command::new(repo_root().join("scripts/retain-linux-package-history.py"))
            .args(["--repository-dir"])
            .arg(&repository)
            .args(["--staging-dir"])
            .arg(&staging)
            .args([
                "--apt-signing-key",
                "apt-key",
                "--rpm-signing-key",
                "rpm-key",
                "--current-version",
                current_version,
                "--apt-architecture",
                apt_architecture,
                "--rpm-architecture",
                rpm_architecture,
            ])
            .env("FAKE_RPM_DIGEST_ONLY", if digest_only { "1" } else { "0" })
            .env(
                "FAKE_AMBIGUOUS_RPM_SELECTOR",
                if ambiguous_rpm_selector { "1" } else { "0" },
            )
            .env("PATH", &path)
            .current_dir(repo_root())
            .output()
            .expect("run package-history retention")
    };

    let accepted = run_retention("0.9.0", "amd64", "x86_64", false, false);
    assert!(accepted.status.success(), "{}", stderr(&accepted));
    for retained in [
        "rmux_0.8.0_amd64.deb",
        "rmux-0.8.0-1.x86_64.rpm",
        "rmux_0.9.0_amd64.deb",
        "rmux-0.9.0-1.x86_64.rpm",
    ] {
        assert!(staging.join(retained).is_file(), "missing {retained}");
    }
    for pruned in [
        "rmux_0.7.0_amd64.deb",
        "rmux-0.7.0-1.x86_64.rpm",
        "rmux-0.8.0-1.i686.rpm",
    ] {
        assert!(
            !staging.join(pruned).exists(),
            "N-2 package was retained: {pruned}"
        );
    }
    assert_eq!(
        fs::read(release_assets.join("rmux_0.9.0_amd64.deb"))
            .expect("read current Debian release asset"),
        b"current-deb"
    );
    assert_eq!(
        fs::read(release_assets.join("rmux-0.9.0-1.x86_64.rpm"))
            .expect("read current RPM release asset"),
        b"current-rpm"
    );
    assert_eq!(
        fs::read_dir(&release_assets)
            .expect("read release assets")
            .count(),
        2,
        "retention must not add N-1 to canonical release assets"
    );

    let rejected_rc = run_retention("0.9.0-rc.1", "amd64", "x86_64", false, false);
    assert!(!rejected_rc.status.success());
    assert!(stderr(&rejected_rc).contains("stable MAJOR.MINOR.PATCH"));

    let rejected_missing_arch = run_retention("0.9.0", "arm64", "x86_64", false, false);
    assert!(!rejected_missing_arch.status.success());
    assert!(stderr(&rejected_missing_arch).contains("lacks architecture(s): arm64"));

    let rejected_digest_only = run_retention("0.9.0", "amd64", "x86_64", true, false);
    assert!(!rejected_digest_only.status.success());
    assert!(stderr(&rejected_digest_only).contains("not authenticated by the configured key"));

    let rejected_ambiguous_selector = run_retention("0.9.0", "amd64", "x86_64", false, true);
    assert!(!rejected_ambiguous_selector.status.success());
    assert!(stderr(&rejected_ambiguous_selector)
        .contains("RPM signing key selector must resolve to exactly one primary key"));

    fs::write(staging.join("rmux_0.8.0_amd64.deb"), b"different")
        .expect("replace retained package with divergent bytes");
    let rejected_collision = run_retention("0.9.0", "amd64", "x86_64", false, false);
    assert!(!rejected_collision.status.success());
    assert!(stderr(&rejected_collision).contains("collides with different release asset"));
    fs::copy(&apt_package, staging.join("rmux_0.8.0_amd64.deb"))
        .expect("restore retained package in staging");

    fs::remove_file(&predecessor_rpm).expect("remove RPM predecessor");
    fs::remove_file(&outside_matrix_rpm).expect("remove outside-matrix RPM predecessor");
    let rejected_divergence = run_retention("0.9.0", "amd64", "x86_64", false, false);
    assert!(!rejected_divergence.status.success());
    assert!(stderr(&rejected_divergence).contains("disagree on the latest stable predecessor"));
    fs::write(&predecessor_rpm, b"signed-rpm").expect("restore RPM predecessor");
    fs::write(&outside_matrix_rpm, b"signed-rpm-other-architecture")
        .expect("restore outside-matrix RPM predecessor");

    let rejected_downgrade = run_retention("0.7.0", "amd64", "x86_64", false, false);
    assert!(!rejected_downgrade.status.success());
    assert!(stderr(&rejected_downgrade).contains("refusing to replace newer APT release"));

    fs::remove_file(staging.join("rmux_0.8.0_amd64.deb"))
        .expect("remove accepted retained package");
    fs::write(&apt_package, b"tampered").expect("tamper retained Debian package");
    let rejected = run_retention("0.9.0", "amd64", "x86_64", false, false);
    assert!(!rejected.status.success());
    assert!(stderr(&rejected).contains("signed index checksum"));
    assert!(!staging.join("rmux_0.8.0_amd64.deb").exists());

    fs::remove_dir_all(root).expect("remove package-history fixture");
}

#[test]
#[cfg(unix)]
fn package_repository_base_guard_rejects_a_stale_snapshot() {
    let root = temp_dir("package-repository-base");
    let repository = root.join("repository");
    fs::create_dir_all(&repository).expect("create package repository fixture");

    let git = |arguments: &[&str]| {
        Command::new("git")
            .args(arguments)
            .current_dir(&repository)
            .output()
            .expect("run git for package repository fixture")
    };
    for arguments in [
        &["init", "-q"][..],
        &["config", "user.name", "RMUX Release Test"][..],
        &["config", "user.email", "release-test@example.invalid"][..],
    ] {
        let result = git(arguments);
        assert!(result.status.success(), "{}", stderr(&result));
    }

    fs::write(repository.join("state"), b"first").expect("write first repository state");
    for arguments in [&["add", "state"][..], &["commit", "-q", "-m", "first"][..]] {
        let result = git(arguments);
        assert!(result.status.success(), "{}", stderr(&result));
    }
    let first_head = stdout(&git(&["rev-parse", "HEAD"])).trim().to_owned();

    fs::write(repository.join("state"), b"second").expect("write advanced repository state");
    let result = git(&["commit", "-q", "-am", "second"]);
    assert!(result.status.success(), "{}", stderr(&result));
    let second_head = stdout(&git(&["rev-parse", "HEAD"])).trim().to_owned();
    assert_ne!(first_head, second_head);

    let repository_arg = repository.to_string_lossy().into_owned();
    let rejected = run(
        "scripts/verify-package-repository-base.sh",
        &[&repository_arg, &first_head],
    );
    assert!(!rejected.status.success());
    assert!(stderr(&rejected).contains("advanced after release preparation"));
    assert_eq!(
        stdout(&git(&["rev-parse", "HEAD"])).trim(),
        second_head,
        "a failed compare-and-swap guard must not mutate the repository"
    );
    assert_eq!(
        fs::read(repository.join("state")).expect("read advanced repository state"),
        b"second"
    );

    let accepted = run(
        "scripts/verify-package-repository-base.sh",
        &[&repository_arg, &second_head],
    );
    assert!(accepted.status.success(), "{}", stderr(&accepted));

    fs::remove_dir_all(root).expect("remove package repository fixture");
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).expect("read permissions").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("set executable permissions");
}
