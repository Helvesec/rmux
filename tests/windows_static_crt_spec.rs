#[test]
fn windows_release_builds_and_reuses_only_static_crt_binaries() {
    let workflow = include_str!("../.github/workflows/release.yml");
    let package = include_str!("../scripts/package-windows.ps1");
    let verify = include_str!("../scripts/verify-package-windows.ps1");

    assert!(workflow.contains(
        "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS: \"-C target-feature=+crt-static\""
    ));
    assert!(workflow
        .contains("$env:RUSTFLAGS = \"-C target-feature=+crt-static -Clink-arg=/DEBUG:NONE\""));
    let workflow_gate = workflow
        .find("Run \"./scripts/assert-windows-static-crt.ps1\"")
        .expect("release workflow static CRT gate");
    let manifest = workflow
        .find("kind = \"rmux-windows-release-binaries\"")
        .expect("release binary manifest");
    assert!(
        workflow_gate < manifest,
        "unverified binaries reached the manifest"
    );
    for required in [
        "\"-Binary\", $releaseBin",
        "\"-HelperBinary\", $packageHelper",
        "\"-DaemonBinary\", $releaseDaemon",
    ] {
        assert!(workflow.contains(required), "workflow gate lost {required}");
    }

    let static_flag = package
        .find("$env:RUSTFLAGS = \"$originalRustFlags -C target-feature=+crt-static\".Trim()")
        .expect("direct package static CRT flag");
    let full_build = package
        .find("& cargo @cargoArgs --bin rmux")
        .expect("full Windows build");
    let tiny_build = package
        .find("& cargo @tinyCargoArgs --bin rmux")
        .expect("tiny Windows build");
    let daemon_build = package
        .find("& cargo @daemonCargoArgs --bin rmux-daemon")
        .expect("Windows daemon build");
    let restore = package
        .find("Remove-Item Env:\\RUSTFLAGS -ErrorAction SilentlyContinue")
        .expect("RUSTFLAGS restoration");
    assert!(
        [full_build, tiny_build, daemon_build]
            .into_iter()
            .all(|build| static_flag < build && build < restore),
        "all three release executables must be built before RUSTFLAGS restoration"
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
