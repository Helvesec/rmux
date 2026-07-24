#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
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

fn generate_repository(
    input: &Path,
    output: &Path,
    tools: &Path,
    previous: Option<&Path>,
) -> Output {
    let path = std::env::join_paths(std::iter::once(tools.to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").expect("PATH is defined")),
    ))
    .expect("compose PATH");
    let mut command = Command::new(repo_root().join("scripts/generate-apt-repository.sh"));
    command
        .args(["--input-dir"])
        .arg(input)
        .args(["--output-dir"])
        .arg(output);
    if let Some(previous) = previous {
        command.args(["--previous-repository-dir"]).arg(previous);
    }
    command
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
        .expect("generate APT repository")
}

#[test]
#[cfg(unix)]
fn apt_repository_retains_exactly_one_previous_by_hash_generation() {
    let root = temp_dir("apt-by-hash");
    let input = root.join("input");
    let first = root.join("first");
    let second = root.join("second");
    let rejected = root.join("rejected");
    let tools = root.join("tools");
    fs::create_dir_all(&input).expect("create input");
    fs::create_dir_all(&tools).expect("create tools");
    fs::write(input.join("rmux_0.9.1_amd64.deb"), b"amd64 generation one")
        .expect("write first amd64 package");
    fs::write(input.join("rmux_0.9.1_arm64.deb"), b"arm64 generation one")
        .expect("write first arm64 package");

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

    let result = generate_repository(&input, &first, &tools, None);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let first_suite = first.join("dists/stable");
    let release = fs::read_to_string(first_suite.join("Release")).expect("read Release");
    assert!(release.contains("\nAcquire-By-Hash: yes\n"));
    let mut old_hashes = Vec::new();
    for architecture in ["amd64", "arm64"] {
        let binary = first_suite.join(format!("main/binary-{architecture}"));
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
            old_hashes.push((architecture, name, digest));
        }
    }
    let older_index = root.join("older-index");
    fs::write(&older_index, b"valid but no longer Release-bound index")
        .expect("write older by-hash generation");
    let older_hash = sha256(&older_index);
    fs::copy(
        &older_index,
        first_suite
            .join("main/binary-amd64/by-hash/SHA256")
            .join(&older_hash),
    )
    .expect("add older by-hash generation");

    fs::write(input.join("rmux_0.9.1_amd64.deb"), b"amd64 generation two")
        .expect("write second amd64 package");
    fs::write(input.join("rmux_0.9.1_arm64.deb"), b"arm64 generation two")
        .expect("write second arm64 package");
    let result = generate_repository(&input, &second, &tools, Some(&first));
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let second_suite = second.join("dists/stable");
    for architecture in ["amd64", "arm64"] {
        let binary = second_suite.join(format!("main/binary-{architecture}"));
        let by_hash = binary.join("by-hash/SHA256");
        for name in ["Packages", "Packages.gz"] {
            let new_hash = sha256(&binary.join(name));
            let old_hash = old_hashes
                .iter()
                .find(|(old_architecture, old_name, _)| {
                    *old_architecture == architecture && *old_name == name
                })
                .map(|(_, _, digest)| digest)
                .expect("old generation hash");
            assert_ne!(old_hash, &new_hash, "fixture generations must differ");
            assert!(by_hash.join(old_hash).is_file(), "missing previous {name}");
            assert!(by_hash.join(new_hash).is_file(), "missing current {name}");
        }
        assert_eq!(
            fs::read_dir(by_hash)
                .expect("read by-hash directory")
                .count(),
            4,
            "repository must contain only current and previous index generations"
        );
    }
    assert!(
        !second_suite
            .join("main/binary-amd64/by-hash/SHA256")
            .join(older_hash)
            .exists(),
        "an index older than the signed previous Release was retained"
    );

    let (_, _, first_hash) = &old_hashes[0];
    let mislabeled = first_suite
        .join("main/binary-amd64/by-hash/SHA256")
        .join(first_hash);
    fs::write(
        &mislabeled,
        b"bytes that do not match the retained hash name",
    )
    .expect("mislabel previous by-hash index");
    let result = generate_repository(&input, &rejected, &tools, Some(&first));
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("does not match its SHA-256 name"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    fs::remove_dir_all(root).expect("remove fixture");
}
