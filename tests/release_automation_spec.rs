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
    assert!(release.contains("RMUX_WINDOWS_CTRL_MATRIX_EVIDENCE_JSON"));
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
    assert!(release.contains("RPM-GPG-KEY-rmux-repository"));
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
fn perf_current_and_darwin_baseline_validation_fail_closed_on_identity_drift() {
    let bench = include_str!("../scripts/perf-bench.sh");
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
        "#!/bin/sh\nset -eu\nout=\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = --output ]; then out=$2; shift 2; else shift; fi\ndone\n[ -n \"$out\" ]\nprintf signature > \"$out\"\n",
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

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).expect("read permissions").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("set executable permissions");
}
