//! An executable fixture runs even while a fork made during its writing has
//! not reached its exec yet, and every program a test writes is such a
//! fixture.
#![cfg(unix)]

#[path = "support/executable_fixture.rs"]
mod executable_fixture;

use std::collections::BTreeMap;
use std::fs;
use std::io::{PipeWriter, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn fixture_root(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "rmux-executable-fixture-{label}-{}-{nonce}",
        std::process::id()
    ))
}

/// A child forked from this process that waits, without exec, until it is
/// released. Like the child of a `Command::spawn` from another test thread
/// before its exec, it holds every descriptor this process had open at the
/// fork, close-on-exec ones included.
struct ForkedHolder {
    pid: libc::pid_t,
    release: PipeWriter,
}

impl ForkedHolder {
    fn fork() -> Self {
        let (wait, release) = std::io::pipe().expect("create the release pipe");
        let wait_fd = wait.as_raw_fd();
        // SAFETY: between fork and `_exit` the child only calls `read` and
        // `_exit`, which are async-signal-safe.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed: {}", std::io::Error::last_os_error());
        if pid == 0 {
            let mut byte = 0_u8;
            // SAFETY: `byte` outlives the call; the child never returns.
            unsafe {
                libc::read(wait_fd, (&raw mut byte).cast(), 1);
                libc::_exit(0);
            }
        }
        drop(wait);
        Self { pid, release }
    }

    fn release(mut self) {
        self.release
            .write_all(&[1])
            .expect("release the forked holder");
        let mut status = 0;
        // SAFETY: `pid` is this process's own unreaped child.
        let reaped = unsafe { libc::waitpid(self.pid, &mut status, 0) };
        assert_eq!(reaped, self.pid, "reap the forked holder");
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "forked holder status {status}"
        );
    }
}

/// The ETXTBSY race made deterministic: the fork happens while the fixture is
/// being written, and the forked child still holds what it copied when the
/// fixture runs.
#[test]
fn a_fixture_runs_while_a_fork_made_during_its_writing_has_not_exec() {
    let root = fixture_root("fork-holder");
    fs::create_dir_all(&root).expect("create the fixture root");
    let tool = root.join("tool");
    let mut holder = None;
    executable_fixture::write_executable_while(&tool, b"#!/bin/sh\nprintf ran\n", 0o755, || {
        holder = Some(ForkedHolder::fork());
    })
    .expect("write the executable fixture");
    let holder = holder.expect("the fork happened while the fixture was written");

    let ran = Command::new(&tool).output();
    holder.release();

    let ran = ran.expect("run the fixture while the fork holds its descriptors");
    assert!(ran.status.success(), "fixture failed: {}", ran.status);
    assert_eq!(ran.stdout, b"ran");
    fs::remove_dir_all(root).expect("remove the fixture root");
}

#[test]
fn a_fixture_has_the_requested_bytes_and_mode() {
    use std::os::unix::fs::PermissionsExt;

    let root = fixture_root("bytes-mode");
    fs::create_dir_all(&root).expect("create the fixture root");
    let tool = root.join("tool with 'quote");
    fs::write(&tool, b"a longer previous body that must be truncated\n")
        .expect("write a previous body");
    executable_fixture::write_executable(&tool, b"#!/bin/sh\nexit 0\n", 0o700)
        .expect("write the executable fixture");

    assert_eq!(
        fs::read(&tool).expect("read the fixture"),
        b"#!/bin/sh\nexit 0\n"
    );
    let mode = fs::metadata(&tool)
        .expect("fixture metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    fs::remove_dir_all(root).expect("remove the fixture root");
}

/// Test sources that give an execute-bit mode literal to something no test
/// process writes and then runs, with how many such literals each holds. A
/// program that a test writes and then runs goes through
/// `executable_fixture::write_executable` instead.
const EXECUTE_BIT_MODES_THAT_RUN_NOTHING: &[(&str, usize)] = &[
    // Directories.
    ("crates/rmux-bootstrap/src/startup_unix/tests.rs", 5),
    ("crates/rmux-bootstrap/tests/startup_unix.rs", 1),
    ("crates/rmux-bootstrap/tests/startup_unix_saturated.rs", 1),
    ("crates/rmux-ipc/tests/unix_saturated_listener.rs", 1),
    ("crates/rmux-ipc/tests/unix_stream_integration.rs", 5),
    ("crates/rmux-sdk/tests/smoke_v1_full.rs", 1),
    ("crates/rmux-server/src/daemon_tests/unix.rs", 5),
    (
        "crates/rmux-server/src/handler_scripting/config_path_tests.rs",
        1,
    ),
    (
        "crates/rmux-server/src/handler_server_access_transport_tests.rs",
        2,
    ),
    ("src/cli/diagnose_tests.rs", 1),
    ("tests/apt_repository_safety_spec.rs", 1),
    ("tests/cli_surface.rs", 1),
    ("tests/common/cli.rs", 2),
    ("tests/internal_daemon.rs", 2),
    ("tests/race_bootstrap.rs", 6),
    ("tests/socket_dropin.rs", 2),
    ("tests/unix_default_config_identity.rs", 1),
    ("xtask/src/msrv/context_tests.rs", 1),
    // A file whose mode only Git reads.
    ("tests/release_proof_adversarial_spec.rs", 1),
    // Fake repository tools, not yet on the shared writer.
    ("tests/release_linux_repository_spec.rs", 1),
];

/// The calls that give a path a literal mode.
const MODE_SETTERS: [&str; 3] = ["from_mode(", "set_mode(", ".mode("];

/// A file of a `tests` directory, a `tests.rs` or `*_tests.rs` file, or a
/// file under a `*_tests` directory.
fn is_test_source(relative: &Path) -> bool {
    let mut components: Vec<String> = relative
        .iter()
        .map(|component| component.to_string_lossy().into_owned())
        .collect();
    let Some(stem) = components
        .pop()
        .and_then(|file| file.strip_suffix(".rs").map(str::to_owned))
    else {
        return false;
    };
    stem == "tests"
        || stem.ends_with("_tests")
        || components
            .iter()
            .any(|directory| directory == "tests" || directory.ends_with("_tests"))
}

fn execute_bit_mode_literals(line: &str) -> usize {
    MODE_SETTERS
        .iter()
        .map(|setter| {
            line.match_indices(setter)
                .filter(|(at, _)| {
                    let Some(literal) = line[at + setter.len()..].trim_start().strip_prefix("0o")
                    else {
                        return false;
                    };
                    let digits: String = literal
                        .chars()
                        .take_while(|character| character.is_digit(8) || *character == '_')
                        .filter(|character| *character != '_')
                        .collect();
                    u32::from_str_radix(&digits, 8).is_ok_and(|mode| mode & 0o111 != 0)
                })
                .count()
        })
        .sum()
}

/// Counts the execute-bit mode literals of every test source under `directory`,
/// skipping hidden and `target*` directories.
fn count_test_execute_bit_modes(
    root: &Path,
    directory: &Path,
    counts: &mut BTreeMap<String, usize>,
) {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()));
    for entry in entries {
        let entry = entry.expect("read a directory entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry.file_type().expect("read a directory entry type");
        let path = entry.path();
        if kind.is_dir() {
            if !name.starts_with('.') && !name.starts_with("target") {
                count_test_execute_bit_modes(root, &path, counts);
            }
            continue;
        }
        let relative = path.strip_prefix(root).expect("a path under the root");
        if !kind.is_file() || !name.ends_with(".rs") || !is_test_source(relative) {
            continue;
        }
        let source =
            fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let count: usize = String::from_utf8_lossy(&source)
            .lines()
            .map(execute_bit_mode_literals)
            .sum();
        if count > 0 {
            counts.insert(relative.to_string_lossy().into_owned(), count);
        }
    }
}

/// A test that writes a program itself and then makes it executable lets a
/// concurrent fork keep it busy. Such a site gives the program an execute-bit
/// mode literal, so every such literal in test code is either listed as
/// running nothing or is a defect.
#[test]
fn every_program_a_test_writes_goes_through_the_executable_fixture_writer() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = BTreeMap::new();
    count_test_execute_bit_modes(root, root, &mut found);
    let listed: BTreeMap<String, usize> = EXECUTE_BIT_MODES_THAT_RUN_NOTHING
        .iter()
        .map(|(file, count)| ((*file).to_owned(), *count))
        .collect();

    let mismatches: Vec<String> = found
        .keys()
        .chain(listed.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter_map(|file| {
            let (found, listed) = (found.get(file), listed.get(file));
            (found != listed).then(|| {
                format!(
                    "{file}: {} execute-bit mode literals, {} listed",
                    found.copied().unwrap_or(0),
                    listed.copied().unwrap_or(0)
                )
            })
        })
        .collect();
    assert!(
        mismatches.is_empty(),
        "write a program a test runs with executable_fixture::write_executable, \
         and list in EXECUTE_BIT_MODES_THAT_RUN_NOTHING only what runs nothing:\n{}",
        mismatches.join("\n")
    );
}
