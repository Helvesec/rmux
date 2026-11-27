#![cfg(unix)]

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    fs::canonicalize(std::env::temp_dir())
        .expect("canonical temporary directory")
        .join(format!("rmux-{label}-{}-{nonce}", std::process::id()))
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

/// Installs `script` as the executable tool `path`.
///
/// Linux refuses to execute a file that any process still holds open for
/// writing (ETXTBSY). The other tests of this binary fork from their own
/// threads, and a child forked while this process holds a write descriptor
/// keeps its copy until its exec closes it; a tool written here could then
/// fail to start once the scheduler delays that child. So this process only
/// writes a staging copy that nothing executes, and the tool itself is
/// written by a `cp` process that has exited before this returns.
fn install_tool(path: &Path, script: &str) {
    install_tool_while_writing(path, script, |_| {});
}

/// `install_tool`, calling `while_writing` while this process holds the
/// only write descriptor it ever opens, the one a concurrent fork inherits.
fn install_tool_while_writing(path: &Path, script: &str, while_writing: impl FnOnce(&fs::File)) {
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .expect("tool path names a file");
    let staging = path.with_file_name(format!(".{name}.staging"));
    let mut file = fs::File::create(&staging).expect("create tool staging copy");
    file.write_all(script.as_bytes())
        .expect("write tool staging copy");
    while_writing(&file);
    drop(file);
    let copied = Command::new("cp")
        .arg(&staging)
        .arg(path)
        .status()
        .expect("run cp for the tool");
    assert!(copied.success(), "cp {} failed: {copied}", path.display());
    make_executable(path);
}

fn sha256(path: &Path) -> String {
    let output = Command::new("python3")
        .args([
            "-c",
            "import hashlib, sys; print(hashlib.sha256(open(sys.argv[1], 'rb').read()).hexdigest())",
        ])
        .arg(path)
        .output()
        .expect("run Python SHA-256");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("SHA-256 output is UTF-8")
        .split_whitespace()
        .next()
        .expect("SHA-256 emitted a digest")
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

const APT_ARCHITECTURES: [&str; 2] = ["amd64", "arm64"];

fn install_apt_tools(tools: &Path) {
    fs::create_dir_all(tools).expect("create APT tool directory");
    let dpkg_deb = tools.join("dpkg-deb");
    install_tool(
        &dpkg_deb,
        r#"#!/bin/sh
set -eu
test "$1" = -f
case "$2" in
  *_amd64.deb) architecture=amd64 ;;
  *_arm64.deb) architecture=arm64 ;;
  *) exit 64 ;;
esac
case "${3:-}" in
  "") printf 'Package: rmux\nVersion: 0.9.1\nArchitecture: %s\n' "$architecture" ;;
  Package) printf 'rmux\n' ;;
  Architecture) printf '%s\n' "$architecture" ;;
  *) exit 64 ;;
esac
"#,
    );
}

fn write_apt_packages(input: &Path, generation: &str) {
    fs::create_dir_all(input).expect("create APT input");
    for architecture in APT_ARCHITECTURES {
        fs::write(
            input.join(format!("rmux_0.9.1_{architecture}.deb")),
            format!("{architecture} {generation}"),
        )
        .expect("write APT package fixture");
    }
}

/// Reshape a repository generated by this branch into the layout a pre-by-hash
/// generator published: canonical indexes only, and a `Release` that never
/// advertised `Acquire-By-Hash`. The result is byte-identical (modulo `Date`)
/// to what `v0.9.1:scripts/generate-apt-repository.sh` writes.
fn unpublish_by_hash(suite_root: &Path) {
    for architecture in APT_ARCHITECTURES {
        fs::remove_dir_all(suite_root.join(format!("main/binary-{architecture}/by-hash")))
            .expect("remove the by-hash generation");
    }
    let release = suite_root.join("Release");
    let advertised = fs::read_to_string(&release).expect("read Release");
    let bootstrap: String = advertised
        .lines()
        .filter(|line| !line.starts_with("Acquire-By-Hash:"))
        .map(|line| format!("{line}\n"))
        .collect();
    assert_ne!(
        bootstrap, advertised,
        "fixture Release never advertised by-hash"
    );
    fs::write(&release, bootstrap).expect("write the pre-by-hash Release");
}

fn by_hash_digests(suite_root: &Path, architecture: &str) -> BTreeSet<String> {
    let by_hash = suite_root.join(format!("main/binary-{architecture}/by-hash/SHA256"));
    fs::read_dir(&by_hash)
        .expect("read by-hash directory")
        .map(|entry| {
            entry
                .expect("read by-hash entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

fn current_index_digests(suite_root: &Path, architecture: &str) -> BTreeSet<String> {
    let binary = suite_root.join(format!("main/binary-{architecture}"));
    ["Packages", "Packages.gz"]
        .into_iter()
        .map(|name| sha256(&binary.join(name)))
        .collect()
}

fn install_rpm_metadata_tools(tools: &Path) {
    fs::create_dir_all(tools).expect("create RPM metadata tool directory");
    let createrepo = tools.join("createrepo_c");
    install_tool(
        &createrepo,
        r#"#!/bin/sh
set -eu
python3 - "$1" "${RPM_METADATA_ID:?}" <<'PY'
import hashlib
from pathlib import Path
import sys

root = Path(sys.argv[1]) / "repodata"
identity = sys.argv[2]
root.mkdir(parents=True, exist_ok=True)
payload = f"{identity}-metadata".encode()
digest = hashlib.sha256(payload).hexdigest()
name = f"{digest}-primary.xml.gz"
(root / name).write_bytes(payload)
(root / "repomd.xml").write_text(
    '<?xml version="1.0" encoding="UTF-8"?>\n'
    '<repomd xmlns="http://linux.duke.edu/metadata/repo">\n'
    '  <data type="primary">\n'
    f'    <checksum type="sha256">{digest}</checksum>\n'
    f'    <location href="repodata/{name}"/>\n'
    f'    <size>{len(payload)}</size>\n'
    '  </data>\n'
    '</repomd>\n',
    encoding="utf-8",
)
PY
"#,
    );

    let gpg = tools.join("gpg");
    install_tool(
        &gpg,
        r#"#!/bin/sh
set -eu
case " $* " in
  *" --with-colons --fingerprint "*)
    printf 'pub:::::::::\n'
    printf 'fpr:::::::::0123456789ABCDEF0123456789ABCDEF01234567:\n'
    exit 0
    ;;
  *" --export "*)
    printf 'authorized-rpm-repository-key'
    exit 0
    ;;
esac
output=
while [ "$#" -gt 0 ]; do
  if [ "$1" = --output ]; then
    output=$2
    shift 2
  else
    shift
  fi
done
test -n "$output"
printf 'trusted-signature' > "$output"
"#,
    );

    let gpgv = tools.join("gpgv");
    install_tool(
        &gpgv,
        r#"#!/bin/sh
set -eu
keyring=
signature=
document=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --homedir) shift 2 ;;
    --keyring) keyring=$2; shift 2 ;;
    *)
      if [ -z "$signature" ]; then signature=$1; else document=$1; fi
      shift
      ;;
  esac
done
test -n "$keyring" && test -n "$signature" && test -n "$document"
grep -q '^authorized-rpm-repository-key$' "$keyring"
grep -q '^trusted-signature$' "$signature"
grep -q '<repomd ' "$document"
"#,
    );
}

fn generate_rpm_repository(
    input: &Path,
    output: &Path,
    previous: Option<&Path>,
    identity: &str,
    path: &OsStr,
) -> Output {
    let mut command = Command::new(repo_root().join("scripts/generate-rpm-repository.sh"));
    command
        .args(["--input-dir"])
        .arg(input)
        .args(["--output-dir"])
        .arg(output)
        .args(["--repo-signing-key", "repository-key"]);
    if let Some(previous) = previous {
        command.args(["--previous-repository-dir"]).arg(previous);
    }
    command
        .env("PATH", path)
        .env("RPM_METADATA_ID", identity)
        .current_dir(repo_root())
        .output()
        .expect("generate signed RPM repository")
}

fn retained_metadata(repository: &Path) -> BTreeSet<Vec<u8>> {
    fs::read_dir(repository.join("repodata"))
        .expect("list RPM repodata")
        .map(|entry| entry.expect("read RPM repodata entry").path())
        .filter(|path| {
            !matches!(
                path.file_name().and_then(OsStr::to_str),
                Some("repomd.xml" | "repomd.xml.asc")
            )
        })
        .map(|path| fs::read(path).expect("read retained RPM metadata"))
        .collect()
}

/// The ETXTBSY race made deterministic: the write descriptor `install_tool`
/// opens is handed to a child, as a fork from another test thread inherits
/// it, and that child still holds it when the tool runs.
#[test]
#[cfg(unix)]
fn a_tool_runs_while_a_concurrent_child_holds_its_write_descriptor() {
    let root = temp_dir("tool-write-descriptor");
    fs::create_dir_all(&root).expect("create tool fixture");
    let tool = root.join("tool");
    let mut holder = None;
    install_tool_while_writing(&tool, "#!/bin/sh\necho ran\n", |file| {
        let inherited = file.try_clone().expect("share the write descriptor");
        holder = Some(
            Command::new("cat")
                .stdin(Stdio::piped())
                .stdout(inherited)
                .spawn()
                .expect("start the descriptor holder"),
        );
    });
    let mut holder = holder.expect("the write descriptor reached a child");

    let ran = Command::new(&tool).output();
    drop(holder.stdin.take());
    holder.wait().expect("reap the descriptor holder");

    let ran = ran.expect("run the tool while a child holds the write descriptor");
    assert!(ran.status.success(), "tool failed: {}", ran.status);
    assert_eq!(ran.stdout, b"ran\n");
    fs::remove_dir_all(root).expect("remove tool fixture");
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
    install_apt_tools(&tools);
    write_apt_packages(&input, "generation one");

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

    write_apt_packages(&input, "generation two");
    let result = generate_repository(&input, &second, &tools, Some(&first));
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8_lossy(&result.stdout).contains("by_hash_retention=retained\n"),
        "{}",
        String::from_utf8_lossy(&result.stdout)
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

#[test]
fn apt_repository_bootstraps_from_a_publication_without_by_hash_indexes() {
    let root = temp_dir("apt-by-hash-bootstrap");
    let input = root.join("input");
    let previous = root.join("previous");
    let bootstrapped = root.join("bootstrapped");
    let tools = root.join("tools");
    install_apt_tools(&tools);
    write_apt_packages(&input, "published by 0.9.1");

    let result = generate_repository(&input, &previous, &tools, None);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let previous_suite = previous.join("dists/stable");
    unpublish_by_hash(&previous_suite);

    write_apt_packages(&input, "published by 0.10.0");
    let result = generate_repository(&input, &bootstrapped, &tools, Some(&previous));
    assert!(
        result.status.success(),
        "the first by-hash generation must not require a by-hash predecessor: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8_lossy(&result.stdout).contains("by_hash_retention=bootstrap\n"),
        "the bootstrap must report its skipped retention: {}",
        String::from_utf8_lossy(&result.stdout)
    );
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("does not advertise Acquire-By-Hash"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let bootstrapped_suite = bootstrapped.join("dists/stable");
    let release = fs::read_to_string(bootstrapped_suite.join("Release")).expect("read Release");
    assert!(release.contains("\nAcquire-By-Hash: yes\n"));
    for architecture in APT_ARCHITECTURES {
        assert_eq!(
            by_hash_digests(&bootstrapped_suite, architecture),
            current_index_digests(&bootstrapped_suite, architecture),
            "the first generation must publish exactly its own by-hash indexes"
        );
    }

    fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn apt_repository_rejects_a_predecessor_that_lost_advertised_by_hash_indexes() {
    let root = temp_dir("apt-by-hash-dropped");
    let input = root.join("input");
    let previous = root.join("previous");
    let rejected = root.join("rejected");
    let tools = root.join("tools");
    install_apt_tools(&tools);
    write_apt_packages(&input, "generation one");

    let result = generate_repository(&input, &previous, &tools, None);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    for architecture in APT_ARCHITECTURES {
        fs::remove_dir_all(
            previous.join(format!("dists/stable/main/binary-{architecture}/by-hash")),
        )
        .expect("drop an advertised by-hash generation");
    }

    write_apt_packages(&input, "generation two");
    let result = generate_repository(&input, &rejected, &tools, Some(&previous));
    assert!(
        !result.status.success(),
        "a Release advertising Acquire-By-Hash must still require its indexes"
    );
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .contains("authenticated previous by-hash index main/binary-amd64/Packages is missing"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn apt_repository_authenticates_previous_indexes_while_bootstrapping_by_hash() {
    let root = temp_dir("apt-by-hash-bootstrap-authentication");
    let input = root.join("input");
    let previous = root.join("previous");
    let rejected = root.join("rejected");
    let tools = root.join("tools");
    install_apt_tools(&tools);
    write_apt_packages(&input, "published by 0.9.1");

    let result = generate_repository(&input, &previous, &tools, None);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let previous_suite = previous.join("dists/stable");
    unpublish_by_hash(&previous_suite);
    fs::write(
        previous_suite.join("main/binary-arm64/Packages"),
        b"bytes the signed previous Release does not bind",
    )
    .expect("tamper with a canonical previous index");

    write_apt_packages(&input, "published by 0.10.0");
    let result = generate_repository(&input, &rejected, &tools, Some(&previous));
    assert!(
        !result.status.success(),
        "bootstrapping must not skip canonical index authentication"
    );
    assert!(
        String::from_utf8_lossy(&result.stderr).contains(
            "authenticated previous index main/binary-arm64/Packages does not match its SHA-256 name"
        ),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn rpm_repository_retains_exactly_one_authenticated_metadata_generation() {
    let root = temp_dir("rpm-metadata-retention");
    let input = root.join("input");
    let tools = root.join("tools");
    fs::create_dir_all(&input).expect("create RPM input");
    fs::write(input.join("rmux-0.10.0-1.x86_64.rpm"), b"rpm").expect("write RPM input");
    install_rpm_metadata_tools(&tools);
    let path = std::env::join_paths(std::iter::once(tools).chain(std::env::split_paths(
        &std::env::var_os("PATH").expect("PATH is defined"),
    )))
    .expect("compose PATH");

    let first = root.join("first");
    let result = generate_rpm_repository(&input, &first, None, "old", &path);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    fs::write(
        first.join("repodata/unreferenced.xml.gz"),
        b"untrusted-extra",
    )
    .expect("write unreferenced metadata");

    let second = root.join("second");
    let result = generate_rpm_repository(&input, &second, Some(&first), "current", &path);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        retained_metadata(&second),
        BTreeSet::from([b"current-metadata".to_vec(), b"old-metadata".to_vec()]),
        "H_old and H_new must be available without retaining arbitrary files"
    );

    let third = root.join("third");
    let result = generate_rpm_repository(&input, &third, Some(&second), "next", &path);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        retained_metadata(&third),
        BTreeSet::from([b"current-metadata".to_vec(), b"next-metadata".to_vec()]),
        "N-2 metadata must be pruned while the immediate previous generation remains"
    );

    fs::remove_dir_all(root).expect("remove RPM metadata fixture");
}

#[test]
fn rpm_repository_rejects_unauthenticated_or_unsafe_metadata_history() {
    use std::os::unix::fs::symlink;

    let root = temp_dir("rpm-metadata-rejection");
    let input = root.join("input");
    let tools = root.join("tools");
    fs::create_dir_all(&input).expect("create RPM input");
    fs::write(input.join("rmux-0.10.0-1.x86_64.rpm"), b"rpm").expect("write RPM input");
    install_rpm_metadata_tools(&tools);
    let path = std::env::join_paths(std::iter::once(tools).chain(std::env::split_paths(
        &std::env::var_os("PATH").expect("PATH is defined"),
    )))
    .expect("compose PATH");

    let previous = root.join("previous");
    let result = generate_rpm_repository(&input, &previous, None, "old", &path);
    assert!(result.status.success());
    let signature = previous.join("repodata/repomd.xml.asc");
    let repomd = previous.join("repodata/repomd.xml");
    let signed_repomd = fs::read_to_string(&repomd).expect("read signed repomd fixture");
    fs::write(&signature, b"untrusted-signature").expect("tamper signature");
    let rejected = generate_rpm_repository(
        &input,
        &root.join("bad-signature"),
        Some(&previous),
        "new",
        &path,
    );
    assert!(!rejected.status.success(), "untrusted history was accepted");

    fs::write(&signature, b"trusted-signature").expect("restore signature fixture");
    fs::write(&repomd, signed_repomd.replace("repodata/", "repodata/../"))
        .expect("write traversal repomd fixture");
    let rejected = generate_rpm_repository(
        &input,
        &root.join("traversal"),
        Some(&previous),
        "new",
        &path,
    );
    assert!(
        !rejected.status.success(),
        "traversal metadata was accepted"
    );
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("unsafe or duplicate"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );

    fs::write(&repomd, signed_repomd).expect("restore signed repomd fixture");
    let metadata = fs::read_dir(previous.join("repodata"))
        .expect("list previous repodata")
        .map(|entry| entry.expect("read repodata entry").path())
        .find(|path| path.extension().and_then(OsStr::to_str) == Some("gz"))
        .expect("find referenced metadata");
    let payload = fs::read(&metadata).expect("read referenced metadata");
    fs::remove_file(&metadata).expect("remove referenced metadata");
    let outside = root.join("outside-metadata");
    fs::write(&outside, payload).expect("write outside metadata");
    symlink(&outside, &metadata).expect("replace referenced metadata with symlink");
    let rejected =
        generate_rpm_repository(&input, &root.join("symlink"), Some(&previous), "new", &path);
    assert!(
        !rejected.status.success(),
        "symlinked metadata was accepted"
    );
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("symbolic link"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );

    fs::remove_dir_all(root).expect("remove RPM rejection fixture");
}

#[test]
fn release_workflows_supply_the_authenticated_previous_rpm_repository() {
    let release = include_str!("../.github/workflows/release.yml");
    let downstream = include_str!("../.github/workflows/release-linux-repository-build.yml");
    assert!(release.contains("--previous-repository-dir target/package-repository-history/rpm"));
    assert!(downstream.contains("--previous-repository-dir \"$root/history/rpm\""));
}

#[test]
fn package_repository_deployment_keeps_credentials_out_of_git_urls() {
    let output = Command::new("python3")
        .args(["-B", "-c", PACKAGE_REPOSITORY_CREDENTIAL_REGRESSION])
        .arg(repo_root())
        .output()
        .expect("run package repository credential regression");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

const PACKAGE_REPOSITORY_CREDENTIAL_REGRESSION: &str = r#"
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest

SOURCE = Path(sys.argv.pop())
GIT = shutil.which('git')
WORKFLOW = (SOURCE / '.github/workflows/release.yml').read_text()
STEP = WORKFLOW.split('      - name: Deploy Linux package repositories\n', 1)[1]
PREPARE = textwrap.dedent(re.search(
    r'(?ms)^          work=.*?(?=^          expected_base=)', STEP).group())
PUSH = textwrap.dedent(re.search(r'(?m)^          git push .*$', STEP).group())
TOKEN = 'fixture-package-credential'
URL = 'https://github.com/fixture/packages.git'

class PackageRepositoryCredentials(unittest.TestCase):
    def deploy(self, root, clone_fails=False):
        home, temporary, runner = (root / name for name in ('home', 'temporary', 'runner'))
        for path in (home, temporary, runner):
            path.mkdir()
        env = dict(os.environ, HOME=str(home), TMPDIR=str(temporary),
                   RUNNER_TEMP=str(runner), GIT_CONFIG_GLOBAL=os.devnull,
                   GIT_CONFIG_NOSYSTEM='1', GIT_ALLOW_PROTOCOL='file',
                   RMUX_PACKAGE_REPO_TOKEN=TOKEN, RMUX_PACKAGE_REPO='fixture/packages',
                   RMUX_PACKAGE_REPO_BRANCH='packages', FIXTURE_ROOT=str(root))
        # Never consume a caller's Git command-scope configuration or identity.
        for key in list(env):
            if key.startswith(('GIT_CONFIG_', 'GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE')):
                if key not in ('GIT_CONFIG_GLOBAL', 'GIT_CONFIG_NOSYSTEM'):
                    del env[key]
        seed = root / 'seed'
        def git(*args):
            subprocess.run([GIT, *args], env=env, check=True, capture_output=True)
        git('init', '--initial-branch=packages', str(seed))
        (seed / 'payload').write_text('fixture\n')
        git('-C', str(seed), 'add', 'payload')
        git('-C', str(seed), '-c', 'user.name=Fixture', '-c',
            'user.email=fixture@example.invalid', 'commit', '-m', 'fixture')
        bare = root / 'repository.git'
        git('clone', '--bare', str(seed), str(bare))
        destination = root / 'missing.git' if clone_fails else bare
        # Both historical and corrected URLs reach only this local fixture.
        env.update(GIT_CONFIG_COUNT='2',
                   GIT_CONFIG_KEY_0=f'url.{destination.as_uri()}.insteadOf',
                   GIT_CONFIG_VALUE_0=URL,
                   GIT_CONFIG_KEY_1=f'url.{destination.as_uri()}.insteadOf',
                   GIT_CONFIG_VALUE_1=URL.replace('https://', f'https://x-access-token:{TOKEN}@'))
        observe = '''
cp "$work/.git/config" "$FIXTURE_ROOT/origin-config"
printf '%s' "$package_repo_url" > "$FIXTURE_ROOT/transport-url"
if [ -n "${GIT_ASKPASS:-}" ]; then
  test -x "$GIT_ASKPASS"
  printf '%s' "$GIT_ASKPASS" > "$FIXTURE_ROOT/askpass-path"
  "$GIT_ASKPASS" 'Username for fixture' > "$FIXTURE_ROOT/username"
  "$GIT_ASKPASS" 'Password for fixture' > "$FIXTURE_ROOT/password"
  if "$GIT_ASKPASS" 'Unsupported prompt' > /dev/null; then exit 81; fi
fi
cd "$work"
'''
        result = subprocess.run(['/bin/bash', '-euo', 'pipefail', '-c',
                                 'mktemp() { command mktemp "$@" "$TMPDIR/deploy.XXXXXXXX"; }\n'
                                 + PREPARE + observe + PUSH], env=env,
                                text=True, capture_output=True, timeout=30)
        self.assertEqual(list(temporary.iterdir()), [], 'clone work directory leaked')
        self.assertEqual(list(runner.iterdir()), [], 'askpass executable leaked')
        return result

    def test_clone_and_push_do_not_persist_the_credential(self):
        with tempfile.TemporaryDirectory(prefix='rmux-package-credentials-') as name:
            root = Path(name)
            result = self.deploy(root)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn(TOKEN, (root / 'origin-config').read_text(),
                             'credential persisted in Git remote configuration')
            self.assertEqual((root / 'transport-url').read_text(), URL)
            self.assertEqual((root / 'username').read_text(), 'x-access-token\n')
            self.assertEqual((root / 'password').read_text(), TOKEN + '\n')
            self.assertFalse(Path((root / 'askpass-path').read_text()).exists())

    def test_clone_failure_cleans_temporary_credentials(self):
        with tempfile.TemporaryDirectory(prefix='rmux-package-credentials-failure-') as name:
            root = Path(name)
            result = self.deploy(root, clone_fails=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('does not appear to be a git repository', result.stderr)
            self.assertFalse((root / 'origin-config').exists())

unittest.main()
"#;
