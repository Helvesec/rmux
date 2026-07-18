const VERIFIER: &str = include_str!("../scripts/verify-package-windows.ps1");
const NEXTEST_SMOKE: &str = include_str!("../scripts/run-nextest-package-smoke.ps1");

fn function_body<'a>(source: &'a str, name: &str, next_name: &str) -> &'a str {
    source
        .split(&format!("function {name}"))
        .nth(1)
        .unwrap_or_else(|| panic!("missing PowerShell function {name}"))
        .split(&format!("function {next_name}"))
        .next()
        .unwrap_or_else(|| panic!("unbounded PowerShell function {name}"))
}

#[test]
fn archived_package_smokes_list_exactly_one_test_before_running() {
    for required in [
        "\"nextest\", \"list\"",
        "\"--archive-file\", $archiveFull",
        "kind(test) & package(=$Package) & binary(=$TestTarget) & test(=$TestName)",
        "\"--filterset\", $filterExpression",
        "\"--message-format\", \"oneline\"",
        "\"--\", $TestName, \"--exact\"",
        "$selected.Count -ne 1",
        "$expected = \"$Package::$TestTarget $TestName\"",
        "[System.StringComparison]::Ordinal",
        "\"nextest\", \"run\"",
        "\"--extract-to\", $extractRoot",
        "\"--workspace-remap\", $workspaceFull",
        "\"--retries\", \"0\"",
        "\"--flaky-result\", \"fail\"",
    ] {
        assert!(
            NEXTEST_SMOKE.contains(required),
            "archived package smoke lost {required:?}"
        );
    }

    assert_eq!(
        NEXTEST_SMOKE
            .matches("\"--archive-file\", $archiveFull")
            .count(),
        2,
        "list and run must consume the same archive"
    );
    assert!(!NEXTEST_SMOKE.contains("\"nextest\", \"archive\""));
    assert!(!NEXTEST_SMOKE.contains("\"test\", \"--locked\""));
}

#[test]
fn archived_package_smoke_owns_only_its_unique_extraction_root() {
    let create = NEXTEST_SMOKE
        .find("rmux-nextest-package-smoke-$PID-")
        .expect("unique package smoke temporary root");
    let cleanup = NEXTEST_SMOKE
        .rfind("Remove-Item -LiteralPath $temporaryRoot -Recurse -Force")
        .expect("package smoke cleanup");

    assert!(create < cleanup);
    assert!(NEXTEST_SMOKE.contains("Test-Path -LiteralPath $archiveFull -PathType Leaf"));
    assert!(NEXTEST_SMOKE.contains("Test-Path -LiteralPath $workspaceFull -PathType Container"));
    assert!(
        !NEXTEST_SMOKE.contains("Remove-Item -LiteralPath $Archive"),
        "a caller-owned archive must never be removed"
    );
}

#[test]
fn package_verifier_preserves_cargo_fallback_and_zip_binary_environment() {
    assert!(VERIFIER.contains("[string]$NextestArchive = \"\""));

    let sdk = function_body(VERIFIER, "InvokeSdkWindowsSmoke", "InvokeMouseBorderSmoke");
    for required in [
        "$env:RMUX_SDK_WINDOWS_SMOKE_RMUX_BIN = [System.IO.Path]::GetFullPath($Binary)",
        "$env:RMUX_SDK_WINDOWS_SMOKE_PIPE = $pipePath",
        "if ([string]::IsNullOrWhiteSpace($NextestArchive))",
        "Assert-CargoFilter 1",
        "& cargo @(",
        "run-nextest-package-smoke.ps1",
        "-Package \"rmux-sdk\"",
        "-TestTarget \"smoke_v1_windows\"",
    ] {
        assert!(
            sdk.contains(required),
            "SDK package smoke lost {required:?}"
        );
    }

    let mouse = function_body(VERIFIER, "InvokeMouseBorderSmoke", "InvokeCtrlMatrixSmoke");
    for required in [
        "$env:RMUX_MOUSE_BORDER_RMUX_BIN = [System.IO.Path]::GetFullPath($Binary)",
        "$env:RMUX_MOUSE_BORDER_RMUX_DAEMON_BIN = [System.IO.Path]::GetFullPath($DaemonBinary)",
        "if ([string]::IsNullOrWhiteSpace($NextestArchive))",
        "assert-cargo-filter-nonempty.ps1",
        "& cargo @(",
        "run-nextest-package-smoke.ps1",
        "-Package \"rmux\"",
        "-TestTarget \"windows_mouse_border_resize\"",
        "\"--exact\"",
        "\"--test-threads=1\"",
    ] {
        assert!(
            mouse.contains(required),
            "mouse package smoke lost {required:?}"
        );
    }
}

#[test]
fn release_package_metadata_is_bound_to_the_expected_commit() {
    let release_gate = VERIFIER
        .split("if ($RequireReleaseArtifact)")
        .nth(1)
        .expect("release artifact metadata gate")
        .split("$packagedBinaryHash = Sha256File $binary")
        .next()
        .expect("bounded release artifact metadata gate");

    for required in [
        "if (-not [string]::IsNullOrWhiteSpace($ExpectedGitSha))",
        "'^[0-9a-f]{40}$'",
        "$metadata.PSObject.Properties.Name -contains \"git_commit\"",
        "[string]$metadata.git_commit",
        "[System.StringComparison]::Ordinal",
        "release artifact metadata git_commit does not match expected Git SHA",
    ] {
        assert!(
            release_gate.contains(required),
            "release metadata binding lost {required:?}"
        );
    }
}
