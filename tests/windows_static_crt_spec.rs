#[path = "support/python3.rs"]
mod python3;

#[test]
fn windows_release_builds_and_reuses_only_static_crt_binaries() {
    // Keep full, tiny and daemon build provenance bound to CRT verification.
    let workflow = include_str!("../.github/workflows/release.yml");
    let package = include_str!("../scripts/package-windows.ps1");
    let verify = include_str!("../scripts/verify-package-windows.ps1");

    assert!(workflow.contains(
        "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS: \"-C target-feature=+crt-static\""
    ));
    assert!(workflow
        .contains("$env:RUSTFLAGS = \"-C target-feature=+crt-static -Clink-arg=/DEBUG:NONE\""));
    let workflow_gate = workflow
        .find("& \"./scripts/assert-windows-static-crt.ps1\" `")
        .expect("release workflow static CRT gate");
    assert!(
        !workflow.contains("Run \"./scripts/assert-windows-static-crt.ps1\" @("),
        "PowerShell named parameters must not use positional array splatting"
    );
    let manifest = workflow
        .find("kind = \"rmux-windows-release-binaries\"")
        .expect("release binary manifest");
    assert!(
        workflow_gate < manifest,
        "unverified binaries reached the manifest"
    );
    let workflow_gate_block = &workflow[workflow_gate..manifest];
    for required in [
        "-Binary $releaseBin",
        "-HelperBinary $packageHelper",
        "-DaemonBinary $releaseDaemon",
    ] {
        assert!(
            workflow_gate_block.contains(required),
            "workflow gate lost {required}"
        );
    }

    let cargo_start = package
        .find("$cargoArgs =")
        .expect("package Cargo arguments");
    let cargo_end = package[cargo_start..]
        .find("$targetDir =")
        .map(|offset| cargo_start + offset)
        .expect("end of package Cargo arguments");
    assert_eq!(
        package[cargo_start..cargo_end].trim(),
        r#"$cargoArgs = @("build", "--package", "rmux", "--locked", "--target", $Target)
if ($Configuration -eq "release") {
    $cargoArgs += "--release"
}"#
    );

    // Notices now own the three Cargo builds. Pin the complete calling scope,
    // then execute the driver to check each role, its flags and failure path.
    let build_start = package
        .find("$buildsReleaseMsvc =")
        .expect("MSVC build scope");
    let build_end = package[build_start..]
        .find("$noticeRequirement =")
        .map(|offset| build_start + offset)
        .expect("end of MSVC build scope");
    assert_eq!(
        package[build_start..build_end].trim(),
        r#"$buildsReleaseMsvc = $Configuration -eq "release" -and
    $Target -match '-pc-windows-msvc$' -and
    -not $SkipBuild -and
    -not $ReuseReleaseBinaries
$originalRustFlags = $env:RUSTFLAGS
if ($buildsReleaseMsvc -and $originalRustFlags -notlike "*+crt-static*") {
    $env:RUSTFLAGS = "$originalRustFlags -C target-feature=+crt-static".Trim()
}
try {
    if (-not $SkipBuild -and -not $ReuseReleaseBinaries) {
        Invoke-Notices (@("build", "--binary-dir", (Split-Path -Parent $binary), "--", "cargo") + $cargoArgs)
    }
} finally {
    if ($null -eq $originalRustFlags) {
        Remove-Item Env:\RUSTFLAGS -ErrorAction SilentlyContinue
    } else {
        $env:RUSTFLAGS = $originalRustFlags
    }
}"#,
        "all delegated builds must inherit static CRT flags before restoration"
    );
    let invoke = package
        .split("function Invoke-Notices([string[]]$Arguments) {\n")
        .nth(1)
        .and_then(|body| body.split_once("\n}").map(|(body, _)| body))
        .expect("notice driver invocation");
    assert_eq!(
        invoke,
        r#"    $python = Resolve-LayoutPython
    & $python (Join-Path $PSScriptRoot "packaging/notices.py") @Arguments
    if ($LASTEXITCODE -ne 0) { Fail "third-party notice generation or payload binding failed" }"#
    );
    let output = python3::command()
        .args([
            "-B",
            "-c",
            include_str!("fixtures/windows_notice_build.py"),
            env!("CARGO_MANIFEST_DIR"),
        ])
        .output()
        .expect("execute notice build driver fixture");
    assert!(
        output.status.success(),
        "notice build contract failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let reuse_validation = package
        .rfind("ValidateReleaseBinaryManifest")
        .expect("release binary reuse validation");
    let package_gate = package
        .find("assert-windows-static-crt.ps1")
        .expect("package static CRT gate");
    let staging = package
        .find("$distDir = [System.IO.Path]::GetFullPath($OutputDir)")
        .expect("package staging");
    assert!(
        reuse_validation < package_gate && package_gate < staging,
        "both built and reused binaries must be checked before staging"
    );
    for required in [
        "-Binary $binary",
        "-HelperBinary $helperBinary",
        "-DaemonBinary $daemonBinary",
    ] {
        assert!(package.contains(required), "package gate lost {required}");
    }

    let verifier_gate = verify
        .find("assert-windows-static-crt.ps1")
        .expect("canonical package verifier static CRT gate");
    let verifier_binary = verify
        .find("$binary = Join-Path $packageRoot \"rmux.exe\"")
        .expect("extracted public binary");
    let verifier_smokes = verify
        .find("$portableAlias = $null")
        .expect("package runtime smokes");
    assert!(verifier_binary < verifier_gate && verifier_gate < verifier_smokes);
    assert!(verify[verifier_binary..verifier_gate].contains("if ($RequireReleaseArtifact)"));
    for required in [
        "-Binary $binary",
        "-HelperBinary $helperBinary",
        "-DaemonBinary $daemonBinary",
    ] {
        assert!(verify.contains(required), "verifier gate lost {required}");
    }
}

#[test]
fn windows_static_crt_gate_rejects_redistributable_runtime_imports() {
    let gate = include_str!("../scripts/assert-windows-static-crt.ps1");

    for required in [
        "Find-Dumpbin",
        "vswhere.exe",
        "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
        "bin\\HostX86\\x64\\dumpbin.exe",
        "/DEPENDENTS",
        "'^(VCRUNTIME|MSVCP|MSVCR|CONCRT).*\\.dll$'",
        "dumpbin.exe reported no DLL imports",
        "imports redistributable MSVC runtime DLLs",
        "windows-static-crt=ok",
    ] {
        assert!(gate.contains(required), "static CRT gate lost {required}");
    }
    assert!(
        !gate.contains("api-ms-win-crt-"),
        "the gate must allow Windows' system UCRT API-set imports"
    );
}
