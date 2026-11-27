#![cfg(unix)]

#[path = "support/executable_fixture.rs"]
mod executable_fixture;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const ARCHITECTURES: [&str; 2] = ["amd64", "arm64"];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn pinned_system_tool(name: &str) -> PathBuf {
    ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        .into_iter()
        .map(|directory| Path::new(directory).join(name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("pinned system tool is unavailable: {name}"))
}

fn fixture_root(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    repo_root()
        .join("target/tmp/test-runs")
        .join(format!("apt-safety-{label}-{}-{nonce}", std::process::id()))
}

fn install_tools(root: &Path) {
    fs::create_dir_all(root).expect("create tool directory");
    let find = root.join("find");
    executable_fixture::write_executable(
        &find,
        r#"#!/bin/sh
set -eu
if test -n "${RMUX_TEST_FAIL_FIND_ROOT:-}"; then
  for argument in "$@"; do
    if test "$argument" = "$RMUX_TEST_FAIL_FIND_ROOT"; then
      printf 'injected find failure for %s\n' "$argument" >&2
      exit 70
    fi
  done
fi
exec /usr/bin/find "$@"
"#,
        0o755,
    )
    .expect("write find fixture");

    let dpkg_deb = root.join("dpkg-deb");
    executable_fixture::write_executable(
        &dpkg_deb,
        r#"#!/bin/sh
set -eu
test "$1" = -f
case "$2" in
  *_amd64.deb) architecture=amd64 ;;
  *_arm64.deb) architecture=arm64 ;;
  *) exit 64 ;;
esac
package=rmux
case "$(/bin/cat -- "$2")" in
  *:foreign-package) package=alien-package ;;
  *:metadata-unreadable) exit 74 ;;
  *:architecture-mismatch)
    case "$architecture" in
      amd64) architecture=arm64 ;;
      arm64) architecture=amd64 ;;
    esac
    ;;
esac
case "${3:-}" in
  "") printf 'Package: %s\nVersion: 0.10.0\nArchitecture: %s\n' "$package" "$architecture" ;;
  Package) printf '%s\n' "$package" ;;
  Architecture) printf '%s\n' "$architecture" ;;
  *) exit 64 ;;
esac
"#,
        0o755,
    )
    .expect("write dpkg-deb fixture");

    let gpg = root.join("gpg");
    executable_fixture::write_executable(
        &gpg,
        r#"#!/bin/sh
set -eu
case " $* " in
  *" --list-secret-keys "*) exit 0 ;;
esac
output=
kind=signature
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output) output=$2; shift 2 ;;
    --clearsign) kind=in-release; shift ;;
    *) shift ;;
  esac
done
test -n "$output"
printf 'deterministic-%s\n' "$kind" > "$output"
"#,
        0o755,
    )
    .expect("write gpg fixture");
}

fn write_package(input: &Path, architecture: &str, generation: &str) {
    fs::create_dir_all(input).expect("create input directory");
    fs::write(
        input.join(format!("rmux_0.10.0_{architecture}.deb")),
        format!("{architecture}:{generation}\n"),
    )
    .expect("write package fixture");
}

fn write_packages(input: &Path, generation: &str) {
    for architecture in ARCHITECTURES {
        write_package(input, architecture, generation);
    }
}

fn generate(
    input: &Path,
    output: &Path,
    tools: &Path,
    previous: Option<&Path>,
    signed: bool,
) -> Output {
    generator_command(input, output, tools, previous, signed)
        .output()
        .expect("run APT generator")
}

fn generator_command(
    input: &Path,
    output: &Path,
    tools: &Path,
    previous: Option<&Path>,
    signed: bool,
) -> Command {
    let path = std::env::join_paths([
        tools,
        Path::new("/usr/bin"),
        Path::new("/bin"),
        Path::new("/usr/sbin"),
        Path::new("/sbin"),
    ])
    .expect("compose pinned PATH");
    let mut command = Command::new(repo_root().join("scripts/generate-apt-repository.sh"));
    command
        .args(["--input-dir"])
        .arg(input)
        .args(["--output-dir"])
        .arg(output)
        .args(["--suite", "stable", "--component", "main"]);
    for architecture in ARCHITECTURES {
        command.args(["--architecture", architecture]);
    }
    if let Some(previous) = previous {
        command.args(["--previous-repository-dir"]).arg(previous);
    }
    if signed {
        command.args(["--signing-key", "fixture-signing-key"]);
    }
    command
        .env("PATH", path)
        .env_remove("RMUX_APT_GPG_KEY")
        .current_dir(repo_root());
    command
}

#[test]
#[cfg(target_os = "macos")]
fn publishes_authenticated_indexes_with_native_macos_hash_tools() {
    let root = fixture_root("native-hashes");
    let tools = root.join("tools");
    let input = root.join("input");
    let first = root.join("first");
    let second = root.join("second");
    install_tools(&tools);
    write_packages(&input, "first");
    let path = std::env::join_paths([tools.as_path(), Path::new("/usr/bin"), Path::new("/bin")])
        .expect("compose native macOS PATH");
    let probe = Command::new("/bin/bash")
        .args(["-c", "! command -v md5sum && ! command -v sha256sum"])
        .env("PATH", &path)
        .status()
        .expect("check native hash tools");
    assert!(
        probe.success(),
        "native PATH must have no GNU checksum tools"
    );
    for (output, previous) in [(&first, None), (&second, Some(first.as_path()))] {
        let result = generator_command(&input, output, &tools, previous, false)
            .env("PATH", &path)
            .output()
            .expect("generate with native macOS tools");
        assert!(result.status.success(), "{}", stderr(&result));
        let suite = output.join("dists/stable");
        let release = fs::read_to_string(suite.join("Release")).expect("read Release");
        for architecture in ARCHITECTURES {
            for name in ["Packages", "Packages.gz"] {
                let relative = format!("main/binary-{architecture}/{name}");
                let index = suite.join(&relative);
                let size = fs::metadata(&index).expect("index metadata").len();
                for algorithm in ["-md5", "-sha256"] {
                    let digest = Command::new("/usr/bin/openssl")
                        .args(["dgst", algorithm])
                        .arg(&index)
                        .output()
                        .expect("hash index with native OpenSSL");
                    assert!(digest.status.success());
                    let text = String::from_utf8(digest.stdout).expect("digest is UTF-8");
                    let hash = text.split_whitespace().last().expect("digest emitted");
                    assert!(release.contains(&format!(" {hash} {size:16} {relative}\n")));
                }
            }
        }
        write_packages(&input, "second");
    }
    fs::remove_dir_all(root).expect("remove native hash fixture");
}

#[derive(Debug, Eq, PartialEq)]
enum TreeEntry {
    Directory {
        mode: u32,
        size: u64,
    },
    File {
        mode: u32,
        size: u64,
        sha256: String,
        bytes: Vec<u8>,
    },
    Symlink {
        mode: u32,
        size: u64,
        target: PathBuf,
    },
}

fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, TreeEntry> {
    fn sha256(path: &Path) -> String {
        let output = Command::new(pinned_system_tool("sha256sum"))
            .arg("--")
            .arg(path)
            .output()
            .expect("run pinned sha256sum");
        assert!(output.status.success(), "sha256sum failed for {path:?}");
        String::from_utf8(output.stdout)
            .expect("sha256sum output is UTF-8")
            .split_whitespace()
            .next()
            .expect("sha256sum emitted a digest")
            .to_owned()
    }

    fn visit(root: &Path, path: &Path, snapshot: &mut BTreeMap<PathBuf, TreeEntry>) {
        let metadata = fs::symlink_metadata(path).expect("read snapshot metadata");
        let relative = path
            .strip_prefix(root)
            .expect("snapshot path is below root");
        let relative = if relative.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            relative.to_path_buf()
        };
        let entry = if metadata.file_type().is_symlink() {
            TreeEntry::Symlink {
                mode: metadata.permissions().mode(),
                size: metadata.len(),
                target: fs::read_link(path).expect("read snapshot symlink"),
            }
        } else if metadata.is_dir() {
            TreeEntry::Directory {
                mode: metadata.permissions().mode(),
                size: metadata.len(),
            }
        } else {
            let bytes = fs::read(path).expect("read snapshot file");
            TreeEntry::File {
                mode: metadata.permissions().mode(),
                size: metadata.len(),
                sha256: sha256(path),
                bytes,
            }
        };
        snapshot.insert(relative, entry);

        if metadata.is_dir() {
            let mut entries: Vec<_> = fs::read_dir(path)
                .expect("read snapshot directory")
                .map(|entry| entry.expect("read snapshot entry").path())
                .collect();
            entries.sort();
            for child in entries {
                visit(root, &child, snapshot);
            }
        }
    }

    let mut snapshot = BTreeMap::new();
    visit(root, root, &mut snapshot);
    snapshot
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn publishes_into_a_precreated_empty_output_with_system_bash() {
    let root = fixture_root("empty-output");
    let input = root.join("input");
    let output = root.join("output");
    let tools = root.join("tools");
    install_tools(&tools);
    write_packages(&input, "first");
    fs::create_dir_all(&output).expect("precreate empty output");

    let generated = generate(&input, &output, &tools, None, false);
    let release_exists = output.join("dists/stable/Release").is_file();
    fs::remove_dir_all(&root).expect("remove empty-output fixture");

    assert!(generated.status.success(), "{}", stderr(&generated));
    assert!(release_exists, "empty output was not populated");
}

#[test]
fn bash32_resolver_handles_root_and_rejects_an_empty_operand() {
    let inventory = repo_root().join("scripts/apt-repository-inventory.sh");
    let resolve = |operand: &str| {
        Command::new("/bin/bash")
            .args([
                "-euo",
                "pipefail",
                "-c",
                concat!(
                    "die(){ printf 'error: %s\\n' \"$*\" >&2; exit 1; }; ",
                    "source \"$1\"; resolve_without_symlinks \"$2\" resolver"
                ),
                "bash",
            ])
            .arg(&inventory)
            .arg(operand)
            .current_dir(repo_root())
            .output()
            .expect("run resolver with system bash")
    };

    let root = resolve("/");
    assert!(root.status.success(), "{}", stderr(&root));
    assert_eq!(root.stdout, b"/\n");

    let empty = resolve("");
    assert!(
        !empty.status.success(),
        "empty resolver operand was accepted"
    );
    assert!(stderr(&empty).contains("resolver path is empty"));
}

#[test]
fn empty_managed_inventories_reach_their_intended_outcomes() {
    let root = fixture_root("empty-inventories");
    let input = root.join("input");
    let tools = root.join("tools");
    install_tools(&tools);
    write_packages(&input, "first");

    let previous = root.join("empty-previous");
    fs::create_dir_all(&previous).expect("create empty previous repository");
    let previous_rejected = generate(
        &input,
        &root.join("previous-output"),
        &tools,
        Some(&previous),
        false,
    );
    assert!(!previous_rejected.status.success());
    assert!(
        stderr(&previous_rejected)
            .contains("authenticated previous APT Release is missing or unsafe"),
        "{}",
        stderr(&previous_rejected)
    );

    let empty_dists = root.join("empty-dists");
    fs::create_dir_all(empty_dists.join("dists")).expect("create empty dists root");
    fs::create_dir_all(empty_dists.join("pool/main/r/rmux")).expect("create package pool shape");
    let dists_rejected = generate(&input, &empty_dists, &tools, None, false);
    assert!(!dists_rejected.status.success());
    assert!(
        stderr(&dists_rejected).contains("APT suite is missing or unsafe"),
        "{}",
        stderr(&dists_rejected)
    );

    let empty_pool = root.join("empty-pool");
    let generated = generate(&input, &empty_pool, &tools, None, false);
    assert!(generated.status.success(), "{}", stderr(&generated));
    for entry in
        fs::read_dir(empty_pool.join("pool/main/r/rmux")).expect("read generated package pool")
    {
        fs::remove_file(entry.expect("read package entry").path()).expect("remove package");
    }
    let pool_rejected = generate(&input, &empty_pool, &tools, None, false);
    assert!(!pool_rejected.status.success());
    assert!(
        stderr(&pool_rejected).contains("managed APT pool lacks architecture: amd64"),
        "{}",
        stderr(&pool_rejected)
    );

    let missing_architecture = root.join("missing-architecture");
    let generated = generate(&input, &missing_architecture, &tools, None, false);
    assert!(generated.status.success(), "{}", stderr(&generated));
    fs::remove_file(missing_architecture.join("pool/main/r/rmux/rmux_0.10.0_amd64.deb"))
        .expect("remove amd64 package");
    let architecture_rejected = generate(&input, &missing_architecture, &tools, None, false);
    assert!(!architecture_rejected.status.success());
    assert!(
        stderr(&architecture_rejected).contains("managed APT pool lacks architecture: amd64"),
        "{}",
        stderr(&architecture_rejected)
    );

    for output in [
        &previous_rejected,
        &dists_rejected,
        &pool_rejected,
        &architecture_rejected,
    ] {
        assert!(
            !stderr(output).contains("unbound variable"),
            "{}",
            stderr(output)
        );
    }
    fs::remove_dir_all(&root).expect("remove empty-inventories fixture");
}

#[test]
fn inventory_transport_ignores_tmpdir_and_rejects_sorted_record_loss() {
    let root = fixture_root("inventory-transport");
    let input = root.join("input");
    let output = root.join("output");
    let tools = root.join("tools");
    install_tools(&tools);
    write_packages(&input, "first");
    let generated = generate(&input, &output, &tools, None, false);
    assert!(generated.status.success(), "{}", stderr(&generated));

    write_packages(&input, "second");
    let regenerated = generator_command(&input, &output, &tools, None, false)
        .env("TMPDIR", output.join("dists"))
        .output()
        .expect("regenerate with TMPDIR inside managed output");
    assert!(regenerated.status.success(), "{}", stderr(&regenerated));

    let dropping_tools = root.join("dropping-tools");
    install_tools(&dropping_tools);
    let sort = dropping_tools.join("sort");
    executable_fixture::write_executable(&sort, "#!/bin/sh\n/bin/cat >/dev/null\nexit 0\n", 0o755)
        .expect("write record-dropping sort");
    let rejected = generate(
        &input,
        &root.join("dropped-output"),
        &dropping_tools,
        None,
        false,
    );
    fs::remove_dir_all(&root).expect("remove inventory-transport fixture");

    assert!(!rejected.status.success(), "record loss was accepted");
    assert!(
        stderr(&rejected).contains("cannot sort APT package pool without record loss"),
        "{}",
        stderr(&rejected)
    );
    assert!(!String::from_utf8_lossy(&rejected.stdout).contains("repository="));
}

#[test]
fn inventory_transport_rejects_cardinality_preserving_sort_corruption() {
    let root = fixture_root("inventory-sort-corruption");
    let inventory = repo_root().join("scripts/apt-repository-inventory.sh");
    let entries = root.join("entries");
    let tools = root.join("tools");
    install_tools(&tools);
    fs::create_dir_all(&entries).expect("create sort input");
    fs::write(entries.join("alpha"), b"alpha").expect("write alpha input");
    fs::write(entries.join("beta"), b"beta").expect("write beta input");

    let sort = tools.join("sort");
    executable_fixture::write_executable(
        &sort,
        r#"#!/bin/bash
set -euo pipefail
alpha=
beta=
while IFS= read -r -d '' entry; do
  case "$entry" in
    */alpha) alpha="$entry" ;;
    */beta) beta="$entry" ;;
    *) exit 65 ;;
  esac
done
[ -n "$alpha" ] && [ -n "$beta" ]
case "${RMUX_TEST_SORT_MODE:-}" in
  duplicate)
    printf '%s\0%s\0' "$alpha" "$alpha"
    ;;
  reverse)
    printf '%s\0%s\0' "$beta" "$alpha"
    ;;
  *)
    exit 64
    ;;
esac
"#,
        0o755,
    )
    .expect("write corrupting sort");

    let path = std::env::join_paths([
        tools.as_path(),
        Path::new("/usr/bin"),
        Path::new("/bin"),
        Path::new("/usr/sbin"),
        Path::new("/sbin"),
    ])
    .expect("compose corrupting sort PATH");
    let run = |mode: &str| {
        Command::new("/bin/bash")
            .args([
                "-euo",
                "pipefail",
                "-c",
                concat!(
                    "die(){ printf 'error: %s\\n' \"$*\" >&2; exit 1; }; ",
                    "source \"$1\"; ",
                    "collect_sorted_find_entries test -P \"$2\" -maxdepth 1 -type f"
                ),
                "bash",
            ])
            .arg(&inventory)
            .arg(&entries)
            .env("PATH", &path)
            .env("RMUX_TEST_SORT_MODE", mode)
            .current_dir(repo_root())
            .output()
            .expect("run corrupting sort probe")
    };

    let duplicate = run("duplicate");
    assert!(
        !duplicate.status.success(),
        "duplicate-plus-loss sort output was accepted"
    );
    assert!(
        stderr(&duplicate).contains("without preserving every record"),
        "{}",
        stderr(&duplicate)
    );

    let reverse = run("reverse");
    fs::remove_dir_all(&root).expect("remove inventory-sort-corruption fixture");
    assert!(
        !reverse.status.success(),
        "cardinality-preserving reverse sort output was accepted"
    );
    assert!(
        stderr(&reverse).contains("cannot sort test in C byte order"),
        "{}",
        stderr(&reverse)
    );
}

#[test]
fn refuses_unreadable_output_inventory_without_mutating_tree_bytes() {
    let root = fixture_root("unreadable-inventory");
    let input = root.join("input");
    let output = root.join("output");
    let tools = root.join("tools");
    install_tools(&tools);
    write_packages(&input, "first");
    let created = generate(&input, &output, &tools, None, false);
    assert!(created.status.success(), "{}", stderr(&created));
    write_packages(&input, "replacement");
    let before = snapshot_tree(&output);

    let mut permissions = fs::metadata(&output)
        .expect("read output permissions")
        .permissions();
    let original_mode = permissions.mode() & 0o777;
    permissions.set_mode(0o333);
    fs::set_permissions(&output, permissions).expect("make output unreadable but writable");
    let rejected = generate(&input, &output, &tools, None, false);
    let mode_after = fs::symlink_metadata(&output)
        .expect("read output mode after refusal")
        .permissions()
        .mode()
        & 0o777;
    let mut permissions = fs::metadata(&output)
        .expect("read unreadable output permissions")
        .permissions();
    permissions.set_mode(original_mode);
    fs::set_permissions(&output, permissions).expect("restore output traversal");
    let after = snapshot_tree(&output);
    fs::remove_dir_all(&root).expect("remove unreadable-inventory fixture");

    assert!(!rejected.status.success(), "unreadable output was replaced");
    assert!(
        stderr(&rejected).contains("cannot enumerate"),
        "{}",
        stderr(&rejected)
    );
    assert_eq!(mode_after, 0o333, "refusal changed output root mode");
    assert_eq!(after, before, "refusal changed output tree identity");
}

#[test]
fn refuses_injected_find_failure_without_mutating_tree_bytes() {
    let root = fixture_root("find-failure");
    let input = root.join("input");
    let output = root.join("output");
    let tools = root.join("tools");
    install_tools(&tools);
    write_packages(&input, "first");
    let created = generate(&input, &output, &tools, None, false);
    assert!(created.status.success(), "{}", stderr(&created));
    write_packages(&input, "replacement");
    let before = snapshot_tree(&output);

    let rejected = generator_command(&input, &output, &tools, None, false)
        .env("RMUX_TEST_FAIL_FIND_ROOT", &output)
        .output()
        .expect("run APT generator with failing find");
    let after = snapshot_tree(&output);
    fs::remove_dir_all(&root).expect("remove find-failure fixture");

    assert!(!rejected.status.success(), "find failure was ignored");
    assert!(
        stderr(&rejected).contains("cannot enumerate"),
        "{}",
        stderr(&rejected)
    );
    assert_eq!(after, before, "find failure changed output tree identity");
}

#[test]
fn refuses_nested_output_symlink_without_mutating_tree_bytes() {
    let root = fixture_root("nested-symlink");
    let input = root.join("input");
    let output = root.join("output");
    let tools = root.join("tools");
    install_tools(&tools);
    write_packages(&input, "first");
    let created = generate(&input, &output, &tools, None, false);
    assert!(created.status.success(), "{}", stderr(&created));
    let outside = root.join("outside-sentinel");
    fs::write(&outside, b"must remain outside the repository\n").expect("write outside sentinel");
    symlink(
        &outside,
        output.join("dists/stable/main/binary-amd64/nested-link"),
    )
    .expect("create nested output symlink");
    let before = snapshot_tree(&output);
    let outside_before = fs::read(&outside).expect("read outside sentinel");

    let rejected = generate(&input, &output, &tools, None, false);
    let after = snapshot_tree(&output);
    let outside_after = fs::read(&outside).expect("reread outside sentinel");
    fs::remove_dir_all(&root).expect("remove nested-symlink fixture");

    assert!(!rejected.status.success(), "nested symlink was accepted");
    assert!(stderr(&rejected).contains("symbolic links"));
    assert_eq!(
        after, before,
        "symlink refusal changed output tree identity"
    );
    assert_eq!(
        outside_after, outside_before,
        "symlink refusal changed target bytes"
    );
}

#[test]
fn refuses_foreign_package_identity_without_mutating_tree_bytes() {
    let root = fixture_root("foreign-package");
    let input = root.join("input");
    let output = root.join("output");
    let tools = root.join("tools");
    install_tools(&tools);
    write_packages(&input, "first");
    let created = generate(&input, &output, &tools, None, false);
    assert!(created.status.success(), "{}", stderr(&created));
    write_packages(&input, "replacement");
    write_package(&input, "amd64", "foreign-package");
    let before = snapshot_tree(&output);

    let rejected = generate(&input, &output, &tools, None, false);
    let after = snapshot_tree(&output);
    fs::remove_dir_all(&root).expect("remove foreign-package fixture");

    assert!(!rejected.status.success(), "foreign Package was published");
    assert!(
        stderr(&rejected).contains("Package identity"),
        "{}",
        stderr(&rejected)
    );
    assert_eq!(
        after, before,
        "foreign Package refusal changed output tree identity"
    );
}

#[test]
fn refuses_unreadable_package_metadata_without_mutating_tree_bytes() {
    let root = fixture_root("unreadable-metadata");
    let input = root.join("input");
    let output = root.join("output");
    let tools = root.join("tools");
    install_tools(&tools);
    write_packages(&input, "first");
    let created = generate(&input, &output, &tools, None, false);
    assert!(created.status.success(), "{}", stderr(&created));
    write_packages(&input, "replacement");
    write_package(&input, "amd64", "metadata-unreadable");
    let before = snapshot_tree(&output);

    let rejected = generate(&input, &output, &tools, None, false);
    let after = snapshot_tree(&output);
    fs::remove_dir_all(&root).expect("remove unreadable-metadata fixture");

    assert!(
        !rejected.status.success(),
        "unreadable metadata was published"
    );
    assert_eq!(
        after, before,
        "metadata read refusal changed output tree identity"
    );
}

#[test]
fn refuses_mismatched_package_architecture_without_mutating_tree_bytes() {
    let root = fixture_root("architecture-mismatch");
    let input = root.join("input");
    let output = root.join("output");
    let tools = root.join("tools");
    install_tools(&tools);
    write_packages(&input, "first");
    let created = generate(&input, &output, &tools, None, false);
    assert!(created.status.success(), "{}", stderr(&created));
    write_packages(&input, "replacement");
    write_package(&input, "amd64", "architecture-mismatch");
    let before = snapshot_tree(&output);

    let rejected = generate(&input, &output, &tools, None, false);
    let after = snapshot_tree(&output);
    fs::remove_dir_all(&root).expect("remove architecture-mismatch fixture");

    assert!(
        !rejected.status.success(),
        "mismatched package architecture was published"
    );
    assert!(
        stderr(&rejected).contains("Architecture metadata"),
        "{}",
        stderr(&rejected)
    );
    assert_eq!(
        after, before,
        "architecture refusal changed output tree identity"
    );
}

#[test]
fn publishes_valid_rmux_package_identity_metadata() {
    let root = fixture_root("valid-identity");
    let input = root.join("input");
    let output = root.join("output");
    let tools = root.join("tools");
    install_tools(&tools);
    write_packages(&input, "valid");

    let generated = generate(&input, &output, &tools, None, false);
    let amd64_packages = fs::read_to_string(output.join("dists/stable/main/binary-amd64/Packages"))
        .expect("read valid amd64 Packages index");
    let arm64_packages = fs::read_to_string(output.join("dists/stable/main/binary-arm64/Packages"))
        .expect("read valid arm64 Packages index");
    fs::remove_dir_all(&root).expect("remove valid-identity fixture");

    assert!(generated.status.success(), "{}", stderr(&generated));
    assert!(amd64_packages.contains("Package: rmux\n"));
    assert!(amd64_packages.contains("Architecture: amd64\n"));
    assert!(arm64_packages.contains("Package: rmux\n"));
    assert!(arm64_packages.contains("Architecture: arm64\n"));
}

#[test]
fn validates_every_input_architecture_before_replacing_owned_output() {
    let root = fixture_root("preflight");
    let input = root.join("input");
    let output = root.join("output");
    let tools = root.join("tools");
    install_tools(&tools);
    write_packages(&input, "initial");
    let created = generate(&input, &output, &tools, None, false);
    assert!(created.status.success(), "{}", stderr(&created));
    let before = snapshot_tree(&output);

    fs::remove_dir_all(&input).expect("remove populated input");
    fs::create_dir_all(&input).expect("recreate empty input");
    let empty = generate(&input, &output, &tools, None, false);
    assert!(!empty.status.success(), "empty input was accepted");
    assert!(stderr(&empty).contains("no rmux_*_amd64.deb files found"));
    assert_eq!(snapshot_tree(&output), before, "empty input mutated output");

    write_package(&input, "amd64", "arm64-missing");
    let missing = generate(&input, &output, &tools, None, false);
    assert!(
        !missing.status.success(),
        "missing architecture was accepted"
    );
    assert!(stderr(&missing).contains("no rmux_*_arm64.deb files found"));
    assert_eq!(
        snapshot_tree(&output),
        before,
        "partial architecture input mutated output"
    );

    fs::remove_dir_all(root).expect("remove preflight fixture");
}

#[test]
fn rejects_foreign_and_symlinked_outputs_without_mutating_any_bytes() {
    let root = fixture_root("foreign");
    let input = root.join("input");
    let tools = root.join("tools");
    install_tools(&tools);
    write_packages(&input, "valid");

    let foreign = root.join("foreign");
    fs::create_dir_all(foreign.join("dists/stable")).expect("create foreign dists");
    fs::create_dir_all(foreign.join("pool/main/r/rmux")).expect("create foreign pool");
    fs::write(
        foreign.join("dists/stable/Release"),
        b"FOREIGN-RELEASE-SENTINEL\n",
    )
    .expect("write foreign Release");
    fs::write(
        foreign.join("pool/main/r/rmux/foreign.deb"),
        b"FOREIGN-POOL-SENTINEL\n",
    )
    .expect("write foreign package");
    fs::write(foreign.join("foreign-root"), b"FOREIGN-ROOT-SENTINEL\n")
        .expect("write foreign root sentinel");
    let foreign_before = snapshot_tree(&foreign);
    let rejected = generate(&input, &foreign, &tools, None, false);
    assert!(!rejected.status.success(), "foreign output was accepted");
    assert_eq!(
        snapshot_tree(&foreign),
        foreign_before,
        "foreign output changed during refusal"
    );

    let target = root.join("symlink-target");
    fs::create_dir_all(target.join("dists/stable")).expect("create symlink target");
    fs::write(
        target.join("dists/stable/sentinel"),
        b"SYMLINK-TARGET-SENTINEL\n",
    )
    .expect("write symlink target sentinel");
    let target_before = snapshot_tree(&target);
    let output_link = root.join("output-link");
    symlink(&target, &output_link).expect("create output symlink");
    let rejected = generate(&input, &output_link, &tools, None, false);
    assert!(!rejected.status.success(), "symlinked output was accepted");
    assert!(stderr(&rejected).contains("must not traverse symbolic links"));
    assert_eq!(
        snapshot_tree(&target),
        target_before,
        "symlink target changed during refusal"
    );

    fs::remove_dir_all(root).expect("remove foreign-output fixture");
}

#[test]
fn rejects_input_output_overlap_in_both_directions_without_mutation() {
    let root = fixture_root("overlap");
    let tools = root.join("tools");
    install_tools(&tools);

    let outer_output = root.join("outer-output");
    let nested_input = outer_output.join("pool/main/r/rmux");
    write_packages(&nested_input, "input-inside-output");
    fs::create_dir_all(outer_output.join("dists/stable")).expect("create output sentinel path");
    fs::write(
        outer_output.join("dists/stable/sentinel"),
        b"INPUT-INSIDE-OUTPUT-SENTINEL\n",
    )
    .expect("write overlap sentinel");
    let before = snapshot_tree(&outer_output);
    let rejected = generate(&nested_input, &outer_output, &tools, None, false);
    assert!(!rejected.status.success(), "nested input was accepted");
    assert!(stderr(&rejected).contains("cannot overlap"));
    assert_eq!(
        snapshot_tree(&outer_output),
        before,
        "input-inside-output refusal changed bytes"
    );

    let outer_input = root.join("outer-input");
    let nested_output = outer_input.join("generated-repository");
    write_packages(&outer_input, "output-inside-input");
    fs::create_dir_all(&nested_output).expect("create nested output");
    fs::write(
        nested_output.join("sentinel"),
        b"OUTPUT-INSIDE-INPUT-SENTINEL\n",
    )
    .expect("write nested output sentinel");
    let before = snapshot_tree(&outer_input);
    let rejected = generate(&outer_input, &nested_output, &tools, None, false);
    assert!(!rejected.status.success(), "nested output was accepted");
    assert!(stderr(&rejected).contains("cannot overlap"));
    assert_eq!(
        snapshot_tree(&outer_input),
        before,
        "output-inside-input refusal changed bytes"
    );

    fs::remove_dir_all(root).expect("remove overlap fixture");
}

#[test]
fn regenerates_an_exact_signed_rmux_output_and_preserves_the_caller_key() {
    let root = fixture_root("owned");
    let input = root.join("input");
    let output = root.join("debian");
    let tools = root.join("tools");
    install_tools(&tools);
    write_packages(&input, "first");

    let first = generate(&input, &output, &tools, None, true);
    assert!(first.status.success(), "{}", stderr(&first));
    let public_key =
        b"-----BEGIN PGP PUBLIC KEY BLOCK-----\nRMUX\n-----END PGP PUBLIC KEY BLOCK-----\n";
    fs::write(output.join("rmux.asc"), public_key).expect("write caller-owned public key");
    write_packages(&input, "second");

    let second = generate(&input, &output, &tools, None, true);
    assert!(second.status.success(), "{}", stderr(&second));
    assert_eq!(
        fs::read(output.join("rmux.asc")).expect("read preserved key"),
        public_key
    );
    assert_eq!(
        fs::read(output.join("pool/main/r/rmux/rmux_0.10.0_amd64.deb"))
            .expect("read regenerated package"),
        b"amd64:second\n"
    );
    assert!(output.join("dists/stable/InRelease").is_file());
    assert!(output.join("dists/stable/Release.gpg").is_file());

    fs::remove_dir_all(root).expect("remove owned-output fixture");
}

#[test]
fn supports_a_new_ci_leaf_and_keeps_previous_repository_bytes_unchanged() {
    let root = fixture_root("ci-previous");
    let input = root.join("input");
    let previous = root.join("history/debian");
    let ci_parent = root.join("output");
    let current = ci_parent.join("debian");
    let tools = root.join("tools");
    install_tools(&tools);
    write_packages(&input, "previous");
    let first = generate(&input, &previous, &tools, None, false);
    assert!(first.status.success(), "{}", stderr(&first));
    let previous_before = snapshot_tree(&previous);

    write_packages(&input, "current");
    fs::create_dir_all(&ci_parent).expect("create CI output parent");
    assert!(!current.exists(), "CI leaf must begin absent");
    let second = generate(&input, &current, &tools, Some(&previous), false);
    assert!(second.status.success(), "{}", stderr(&second));
    assert!(String::from_utf8_lossy(&second.stdout).contains("by_hash_retention=retained\n"));
    assert_eq!(
        snapshot_tree(&previous),
        previous_before,
        "previous repository was mutated"
    );
    for architecture in ARCHITECTURES {
        let by_hash = current.join(format!(
            "dists/stable/main/binary-{architecture}/by-hash/SHA256"
        ));
        assert_eq!(
            fs::read_dir(by_hash)
                .expect("read retained by-hash directory")
                .count(),
            4
        );
    }

    let current_before_rejection = snapshot_tree(&current);
    let previous_by_hash = previous.join("dists/stable/main/binary-amd64/by-hash/SHA256");
    let retained = fs::read_dir(&previous_by_hash)
        .expect("read previous by-hash")
        .next()
        .expect("find previous by-hash entry")
        .expect("read previous by-hash entry")
        .path();
    fs::write(retained, b"CORRUPTED-PREVIOUS-BY-HASH\n")
        .expect("corrupt previous repository fixture");
    let rejected = generate(&input, &current, &tools, Some(&previous), false);
    assert!(!rejected.status.success(), "invalid previous was accepted");
    assert_eq!(
        snapshot_tree(&current),
        current_before_rejection,
        "invalid previous repository caused output mutation"
    );

    fs::remove_dir_all(root).expect("remove CI/previous fixture");
}
