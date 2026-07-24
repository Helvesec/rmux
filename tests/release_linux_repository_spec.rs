#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("rmux-{label}-{}-{nonce}", std::process::id()))
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .expect("read tool metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make tool executable");
}

fn sha256(path: &Path) -> String {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .expect("run sha256sum");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("sha256sum output is UTF-8")
        .split_whitespace()
        .next()
        .expect("sha256sum emitted a digest")
        .to_owned()
}

#[test]
#[cfg(unix)]
fn apt_repository_publishes_sha256_by_hash_indexes() {
    let root = temp_dir("apt-by-hash");
    let input = root.join("input");
    let output = root.join("output");
    let tools = root.join("tools");
    fs::create_dir_all(&input).expect("create input");
    fs::create_dir_all(&tools).expect("create tools");
    fs::write(input.join("rmux_0.9.1_amd64.deb"), b"amd64 package").expect("write amd64 package");
    fs::write(input.join("rmux_0.9.1_arm64.deb"), b"arm64 package").expect("write arm64 package");

    let dpkg_deb = tools.join("dpkg-deb");
    fs::write(
        &dpkg_deb,
        r#"#!/bin/sh
set -eu
test "$1" = -f
case "$2" in
  *_amd64.deb) architecture=amd64 ;;
  *_arm64.deb) architecture=arm64 ;;
  *) exit 64 ;;
esac
printf 'Package: rmux\nVersion: 0.9.1\nArchitecture: %s\n' "$architecture"
"#,
    )
    .expect("write dpkg-deb fixture");
    make_executable(&dpkg_deb);

    let path = std::env::join_paths(std::iter::once(tools.clone()).chain(std::env::split_paths(
        &std::env::var_os("PATH").expect("PATH is defined"),
    )))
    .expect("compose PATH");
    let result = Command::new(repo_root().join("scripts/generate-apt-repository.sh"))
        .args(["--input-dir"])
        .arg(&input)
        .args(["--output-dir"])
        .arg(&output)
        .args([
            "--suite",
            "stable",
            "--component",
            "main",
            "--architecture",
            "amd64",
            "--architecture",
            "arm64",
        ])
        .env("PATH", path)
        .current_dir(repo_root())
        .output()
        .expect("generate APT repository");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let suite = output.join("dists/stable");
    let release = fs::read_to_string(suite.join("Release")).expect("read Release");
    assert!(release.contains("\nAcquire-By-Hash: yes\n"));
    for architecture in ["amd64", "arm64"] {
        let binary = suite.join(format!("main/binary-{architecture}"));
        for name in ["Packages", "Packages.gz"] {
            let index = binary.join(name);
            let digest = sha256(&index);
            let by_hash = binary.join("by-hash/SHA256").join(&digest);
            assert_eq!(
                fs::read(&by_hash).expect("read by-hash index"),
                fs::read(&index).expect("read canonical index")
            );
            assert!(
                release.contains(&format!(" main/binary-{architecture}/{name}\n")),
                "Release does not bind {architecture}/{name}"
            );
        }
    }

    fs::remove_dir_all(root).expect("remove fixture");
}
