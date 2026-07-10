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
fn windows_release_gate_runs_ctrl_matrix_and_nonempty_cargo_filters() {
    let workflow = include_str!("../.github/workflows/release.yml");
    let gate = include_str!("../scripts/gate-windows-fast.ps1");
    let assert_filter = include_str!("../scripts/assert-cargo-filter-nonempty.ps1");
    let package_verify = include_str!("../scripts/verify-package-windows.ps1");

    for required in [
        r#"Run "./scripts/assert-cargo-filter-nonempty.ps1" @("1", "--", "test", "-p", "rmux-client", "--locked", "output_writer_failure_wakes")"#,
        r#"Run "./scripts/assert-cargo-filter-nonempty.ps1" @("1", "--", "test", "-p", "rmux", "--locked", "--test", "windows_attach_exit")"#,
        r#"Run "./scripts/assert-cargo-filter-nonempty.ps1" @("1", "--", "test", "-p", "rmux", "--locked", "--test", "windows_cli_queue_formats")"#,
        r#"Run "./scripts/windows_ctrl_matrix.ps1" $ctrlArgs"#,
        r#""-ExpectedGitSha", $env:RMUX_EXPECTED_GIT_SHA"#,
        "RMUX_WINDOWS_CTRL_MATRIX_EVIDENCE_JSON",
        "portable-smoke.evidence.json",
        "rmux-windows-release-binaries",
        "-ReuseReleaseBinaries",
        "-ReleaseBinaryManifest",
        r#"if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }"#,
        "rmux-${{ env.RELEASE_REF }}-${{ matrix.target }}-windows-ctrl-matrix",
        "-RunCtrlMatrixSmoke",
    ] {
        assert!(
            workflow.contains(required),
            "Windows release workflow lost required Lot 7 gate snippet {required:?}"
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
            && package_verify.contains("-PortableSmokeOnly")
            && package_verify.contains("ExpectedGitSha")
            && package_verify.contains("CtrlMatrixEvidence")
            && package_verify.contains("produced no passing evidence"),
        "Windows package verification must keep the packaged Ctrl matrix smoke"
    );
    assert!(
        !workflow.contains(
            r#""-PortableSmokeOnly", "-OutDir", "target/windows-ctrl-matrix", "-AllowPortableSmokeSkip""#
        ) && workflow.contains("if-no-files-found: error"),
        "release workflow must not accept a session-0 Ctrl smoke skip as passing evidence"
    );
}
