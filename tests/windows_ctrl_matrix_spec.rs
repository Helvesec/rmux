#[test]
fn windows_ctrl_matrix_script_keeps_direct_attach_send_keys_axes() {
    let script = include_str!("../scripts/windows_ctrl_matrix.ps1");
    let implementation = windows_ctrl_matrix_implementation(script);
    assert!(
        !implementation.contains("$requiredSnippets = @("),
        "Windows Ctrl matrix spec must not validate against its own snippet list"
    );

    for required in [
        "function Invoke-DirectCase",
        "function Invoke-AttachCase",
        "function Invoke-SendKeysCase",
        "Direct natif",
        "RMUX attach",
        "RMUX send-keys",
        "SendKeys-ControlTokens",
        "StaticMatrixSpec",
        "\"Ctrl-C\" { return @(\"C-c\", \"Enter\") }",
        "\"Ctrl-Z\" { return @(\"C-z\", \"Enter\") }",
        "Windows Terminal",
        "WezTerm",
        "Alacritty",
        "PortableSmokeOnly",
        "AllowPortableSmokeSkip",
        "ExpectedGitSha",
        "portable-smoke.evidence.json",
        "matrix_script_sha256",
        "executed_cases",
        "windows-ctrl-matrix-portable-smoke requires an interactive session",
        "portable-smoke.skip.txt",
        "owner=release-engineering",
        "cadence=release-candidate-and-manual-windows-review",
        "Ctrl-C",
        "Ctrl-D",
        "Ctrl-A",
        "Ctrl-Z",
        "Ctrl-H",
        "Esc",
        "python sleep",
        "python descendant sleep",
        "python stdin",
        "line idle",
        "wsl python sleep",
        "wsl python stdin",
        "powershell.exe",
        "wsl-bash",
        "git-bash",
        "timeout",
        "ping",
        "fzf",
    ] {
        assert!(
            implementation.contains(required),
            "Windows Ctrl matrix script lost required axis/snippet {required:?}"
        );
    }

    assert!(
        implementation.contains(
            "$Direct.Returned -eq $Attach.Returned -and $Direct.Returned -eq $Send.Returned"
        ),
        "Windows Ctrl matrix must continue comparing native, attach, and send-keys outcomes"
    );

    assert!(
        implementation.contains("$Results.Count -eq 0")
            && implementation.contains("Where-Object { $_.Verdict -eq \"NO GO\" }")
            && implementation.contains("Windows Ctrl matrix found"),
        "Windows Ctrl matrix must fail closed on empty or NO GO results"
    );

    assert!(
        implementation.contains("Windows Ctrl matrix executed no cases (all skipped)"),
        "Windows Ctrl matrix must fail closed when every case is skipped"
    );
}

fn windows_ctrl_matrix_implementation(script: &str) -> String {
    let start = script
        .find("if ($StaticMatrixSpec) {")
        .expect("StaticMatrixSpec block start");
    let end = script[start..]
        .find("\nif ($PortableSmokeOnly")
        .map(|offset| start + offset)
        .expect("StaticMatrixSpec block end");
    let mut implementation = String::with_capacity(script.len() - (end - start));
    implementation.push_str(&script[..start]);
    implementation.push_str(&script[end..]);
    implementation
}

#[test]
fn windows_test_build_installs_clippy_before_running_the_windows_msvc_gate() {
    let workflow = include_str!("../.github/workflows/ci.yml");
    let windows_test_build = workflow
        .split_once("\n  windows-test-build:\n")
        .map(|(_, suffix)| suffix)
        .and_then(|suffix| suffix.split_once("\n  windows-tests:\n"))
        .map(|(section, _)| section)
        .expect("windows-test-build workflow section");

    assert!(
        windows_test_build.contains("toolchain: \"1.96.1\"\n          components: clippy"),
        "windows-test-build must install cargo-clippy for windows-latest"
    );
    assert!(
        windows_test_build
            .contains("cargo clippy --workspace --all-targets --locked -- -D warnings"),
        "windows-test-build must keep the Windows MSVC clippy gate"
    );
}

#[test]
fn windows_release_gate_uses_hosted_checks_and_nonempty_cargo_filters() {
    let workflow = include_str!("../.github/workflows/release.yml");
    let gate = include_str!("../scripts/gate-windows-fast.ps1");
    let assert_filter = include_str!("../scripts/assert-cargo-filter-nonempty.ps1");
    let package_verify = include_str!("../scripts/verify-package-windows.ps1");
    let canonical_smoke = include_str!("../.github/actions/canonical-smoke/action.yml");

    for required in [
        r#"Run "./scripts/assert-cargo-filter-nonempty.ps1" @("1", "--", "test", "-p", "rmux-client", "--locked", "output_writer_failure_wakes")"#,
        r#"Run "./scripts/assert-cargo-filter-nonempty.ps1" @("1", "--", "test", "-p", "rmux", "--locked", "--test", "windows_attach_exit")"#,
        r#"Run "./scripts/assert-cargo-filter-nonempty.ps1" @("1", "--", "test", "-p", "rmux", "--locked", "--test", "windows_cli_queue_formats")"#,
        r#"& "./scripts/windows_ctrl_matrix.ps1" -StaticMatrixSpec"#,
        "- os: windows-latest",
        "rmux-windows-release-binaries",
        "-ReuseReleaseBinaries",
        "-ReleaseBinaryManifest",
        r#"if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }"#,
        "rmux-${{ env.RELEASE_REF }}-${{ matrix.target }}-windows-release-evidence",
    ] {
        assert!(
            workflow.contains(required),
            "Windows release workflow lost required gate snippet {required:?}"
        );
    }
    assert!(!workflow.contains("RMUX_WINDOWS_CTRL_MATRIX_EVIDENCE_JSON"));
    assert!(
        !workflow.contains(r#"Run "./scripts/windows_ctrl_matrix.ps1" @("-StaticMatrixSpec")"#),
        "PowerShell switch parameters must not be passed through positional array splatting"
    );
    for forbidden in [
        "self-hosted",
        "rmux-windows-interactive",
        "-PortableSmokeOnly",
        "portable-smoke.evidence.json",
        "-RunCtrlMatrixSmoke",
        "-CtrlMatrixEvidence",
    ] {
        assert!(
            !workflow.contains(forbidden),
            "release workflow must not depend on an external interactive runner: {forbidden:?}"
        );
    }

    assert!(
        gate.contains("assert-cargo-filter-nonempty.ps1")
            && gate.contains("windows prompt overlay chain")
            && gate.contains("windows ctrl matrix spec"),
        "fast Windows gate must assert non-empty filtered Windows probes"
    );
    assert!(
        assert_filter.contains("--list")
            && assert_filter.contains("':\\s+test$'")
            && assert_filter.contains("cargo-filter-nonempty=ok"),
        "PowerShell cargo-filter assertion must list tests and fail closed"
    );
    assert!(
        package_verify.contains("RunCtrlMatrixSmoke")
            && package_verify.contains("windows_ctrl_matrix.ps1")
            && package_verify.contains("PortableSmokeOnly = $true")
            && package_verify.contains("ExpectedGitSha")
            && package_verify.contains("CtrlMatrixEvidence")
            && package_verify.contains("produced no passing evidence"),
        "manual Windows package verification must keep the optional interactive Ctrl matrix smoke"
    );
    assert!(
        package_verify.contains("$arguments = @{\n            Rmux =")
            && package_verify.contains("PortableSmokeOnly = $true")
            && package_verify.contains("$arguments.EvidencePath =")
            && !package_verify.contains("\"-Rmux\", [System.IO.Path]::GetFullPath($Binary)"),
        "PowerShell script parameters must use named hashtable splatting"
    );
    assert!(
        canonical_smoke.contains("-RunBinary")
            && canonical_smoke.contains("-RunDaemonSmoke")
            && !canonical_smoke.contains("-RunCtrlMatrixSmoke"),
        "GitHub-hosted package smokes must not require an interactive Windows session"
    );
    assert!(workflow.contains("if-no-files-found: error"));
}
