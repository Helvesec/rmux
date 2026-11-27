use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn write(root: &Path, name: &str, content: &str) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

pub(super) fn cargo(root: &Path, args: &[&str]) -> Vec<u8> {
    let mut command = Command::new(env!("CARGO"));
    // These generated workspaces are independent of the tested checkout. Carry
    // tool discovery and native linker inputs, not the parent's build wrappers,
    // runners, flags or build-script state. Only this child environment changes.
    command.env_clear();
    for name in [
        "PATH",
        "PATHEXT",
        "HOME",
        "USERPROFILE",
        "SYSTEMROOT",
        "WINDIR",
        // Rustc finds MSVC link.exe through the Visual Studio setup COM server
        // (%ProgramData%) or vswhere (%ProgramFiles(x86)%); without them it falls
        // back to whatever `link.exe` PATH holds, such as Git's GNU `link`.
        "ProgramData",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "TEMP",
        "TMP",
        "TMPDIR",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "RUSTUP_TOOLCHAIN",
        "RUSTC",
        "LIB",
        "LIBPATH",
        "INCLUDE",
        "SDKROOT",
        "MACOSX_DEPLOYMENT_TARGET",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let output = command
        .args(args)
        .current_dir(root)
        .env("CARGO_TARGET_DIR", root.join("target"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn notice_fixture_reports_real_compiler_errors() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("target")
        .join("tmp")
        .join(format!("notice-error-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    write(
        &root,
        "Cargo.toml",
        "[workspace]\n[package]\nname = \"notice-error-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(
        &root,
        "src/main.rs",
        "compile_error!(\"notice fixture compiler error\");\nfn main() {}\n",
    );
    let parent = std::env::vars_os().collect::<std::collections::BTreeMap<_, _>>();
    let error = std::panic::catch_unwind(|| cargo(&root, &["build", "--offline"]))
        .expect_err("a failed fixture build must remain a failure");
    let message = error
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| error.downcast_ref::<&str>().copied())
        .expect("Cargo failure diagnostic");
    assert!(
        message.contains("notice fixture compiler error"),
        "{message}"
    );
    assert_eq!(parent, std::env::vars_os().collect());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn notice_sidecars_reach_release_distribution() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let output = Command::new(if cfg!(windows) {
        "python.exe"
    } else {
        "python3"
    })
    .arg(root.join("tests/fixtures/release_notice_distribution.py"))
    .current_dir(root)
    .output()
    .expect("run release notice acceptance fixture");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn notice_uses_actual_build_and_enforces_license_policy() {
    let root = std::env::temp_dir().join(format!(
        "rmux-notice-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    write(
        &root,
        "Cargo.toml",
        r#"
[workspace]
members = ["app", "used", "unused", "build-only"]
resolver = "2"
"#,
    );
    for (name, license) in [
        ("app", "MIT"),
        ("used", "MIT OR GPL-3.0-only"),
        ("unused", "GPL-3.0-only"),
        ("build-only", "Apache-2.0"),
    ] {
        write(&root, &format!("{name}/Cargo.toml"), &format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\nlicense = \"{license}\"\n"
        ));
        write(
            &root,
            &format!("{name}/LICENSE"),
            &format!("Copyright fixture author: {name}\nPermission fixture: {license}\n"),
        );
        write(
            &root,
            &format!("{name}/src/lib.rs"),
            "pub fn value() -> u8 { 7 }\n",
        );
    }
    let app = root.join("app/Cargo.toml");
    fs::write(&app, format!("{}\n[dependencies]\nused = {{ path = \"../used\" }}\nunused = {{ path = \"../unused\", optional = true }}\n[build-dependencies]\nbuild-only = {{ path = \"../build-only\" }}\n", fs::read_to_string(&app).unwrap())).unwrap();
    write(
        &root,
        "app/src/main.rs",
        "fn main() { println!(\"{}\", used::value()); }\n",
    );
    write(
        &root,
        "app/build.rs",
        "fn main() { let _ = build_only::value(); }\n",
    );
    let metadata = root.join("metadata.json");
    fs::write(
        &metadata,
        cargo(&root, &["metadata", "--offline", "--format-version=1"]),
    )
    .unwrap();
    let log = root.join("build.jsonl");
    fs::write(
        &log,
        cargo(
            &root,
            &[
                "build",
                "--offline",
                "--locked",
                "-p",
                "app",
                "--bin",
                "app",
                "--message-format=json-render-diagnostics",
            ],
        ),
    )
    .unwrap();
    let policy = root.join("deny.toml");
    write(
        &root,
        "deny.toml",
        "[licenses]\nallow = [\"MIT\", \"Apache-2.0\"]\n",
    );

    let notice = super::generate(&metadata, std::slice::from_ref(&log), &policy)
        .expect("completed Cargo build must produce its third-party notice");
    assert!(notice.contains("used 0.1.0"));
    assert!(notice.contains("build-only 0.1.0"));
    assert!(
        !notice.contains("unused 0.1.0"),
        "resolved optional packages are not built packages"
    );
    assert!(notice.contains("Copyright fixture author: used"));
    assert!(notice.contains("Copyright fixture author: build-only"));
    assert_eq!(
        notice,
        super::generate(&metadata, std::slice::from_ref(&log), &policy).unwrap()
    );

    write(&root, "deny.toml", "[licenses]\nallow = [\"MIT\"]\n");
    assert!(
        super::generate(&metadata, std::slice::from_ref(&log), &policy)
            .unwrap_err()
            .contains("Apache-2.0")
    );
    write(
        &root,
        "deny.toml",
        "[licenses]\nallow = [\"MIT\", \"Apache-2.0\"]\n",
    );
    let original = fs::read_to_string(&log).unwrap();
    fs::write(
        &log,
        original
            .lines()
            .filter(|line| !line.contains("build-finished"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    assert!(super::generate(&metadata, std::slice::from_ref(&log), &policy).is_err());
    fs::write(&log, original).unwrap();
    fs::remove_file(root.join("used/LICENSE")).unwrap();
    assert!(super::generate(&metadata, &[log], &policy).is_err());
    fs::remove_dir_all(root).unwrap();
}
