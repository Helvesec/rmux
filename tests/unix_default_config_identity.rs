#![cfg(unix)]

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::{symlink, DirBuilderExt, MetadataExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{SystemTime, SystemTimeError, UNIX_EPOCH};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;
const OPTION: &str = "@rmux-default-config-identity";
const APPEND_X: &str = "set -ga @rmux-default-config-identity X\n";
static NEXT_FIXTURE_SERIAL: AtomicU64 = AtomicU64::new(0);

#[test]
fn back_to_back_fixture_names_are_unique_across_threads() -> TestResult {
    // Tight loops call the clock faster than its resolution, which is what
    // happens when parallel tests build their fixtures at the same moment.
    const THREADS: usize = 4;
    const NAMES_PER_THREAD: usize = 1024;
    let workers: Vec<_> = (0..THREADS)
        .map(|_| {
            thread::spawn(|| {
                (0..NAMES_PER_THREAD)
                    .map(|_| unique_fixture_name("d"))
                    .collect::<Result<Vec<_>, _>>()
            })
        })
        .collect();
    let mut names = BTreeSet::new();
    for worker in workers {
        for name in worker
            .join()
            .map_err(|_| "fixture naming thread panicked")??
        {
            assert!(names.insert(name.clone()), "fixture name reused: {name}");
        }
    }
    assert_eq!(names.len(), THREADS * NAMES_PER_THREAD);
    Ok(())
}

#[test]
fn long_config_root_keeps_sockets_usable_and_cleans_up() -> TestResult {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("long-config-root".repeat(8))
        .join(unique_fixture_name("p")?);
    let sockets;
    {
        let fixture = Fixture::in_root(root.clone())?;
        sockets = fixture.sockets.clone();
        // Prove transport usability independently of the product first.
        for socket in &sockets {
            let listener = UnixListener::bind(socket)?;
            drop(listener);
            fs::remove_file(socket)?;
        }
        fixture.link_config(true)?;
        for socket in &sockets {
            for _ in 0..2 {
                fixture.start(socket, &[])?;
                fixture.assert_value(socket, "X")?;
                fixture.assert_value(socket, "X")?;
                assert_eq!(fixture.selection_output(socket)?, fixture.selection(false));
                fixture.run(socket, ["kill-server"])?;
            }
        }
    }
    assert!(!root.exists(), "configuration fixture must be removed");
    for socket in sockets {
        assert!(!socket.exists(), "socket must be removed");
        assert!(
            !socket.parent().unwrap().exists(),
            "socket directory must be removed"
        );
    }
    Ok(())
}

#[test]
fn automatic_symlinks_load_once_across_connections_and_restarts() -> TestResult {
    assert_automatic_alias(false)
}

#[test]
fn automatic_hardlinks_load_once_across_connections_and_restarts() -> TestResult {
    assert_automatic_alias(true)
}

fn assert_automatic_alias(hardlink: bool) -> TestResult {
    let fixture = Fixture::new()?;
    fixture.link_config(hardlink)?;
    let expected_paths = fixture.selection(false);
    let original = fs::read(&fixture.first)?;
    for socket in &fixture.sockets {
        fixture.start(socket, &[])?;
        fixture.assert_value(socket, "X")?;
        // Each CLI call is a new client connection to the same live daemon.
        fixture.assert_value(socket, "X")?;
        assert_eq!(fixture.selection_output(socket)?, expected_paths);
        fixture.run(socket, ["set-option", "-g", OPTION, "runtime-only"])?;
        fixture.assert_value(socket, "runtime-only")?;
        fixture.run(socket, ["kill-server"])?;
        assert_eq!(fs::read(&fixture.first)?, original);
        assert_eq!(fs::read(&fixture.second)?, original);
    }
    Ok(())
}

#[test]
fn separate_files_with_identical_contents_both_load() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(&fixture.first, APPEND_X)?;
    fs::write(&fixture.second, APPEND_X)?;
    assert_ne!(identity(&fixture.first)?, identity(&fixture.second)?);
    let socket = &fixture.sockets[0];
    fixture.start(socket, &[])?;
    fixture.assert_value(socket, "XX")?;
    assert_eq!(fixture.selection_output(socket)?, fixture.selection(true));
    assert!(
        !fixture.third.exists(),
        "missing discovery path stays reported"
    );
    Ok(())
}

#[test]
fn distinct_automatic_files_keep_discovery_order() -> TestResult {
    let fixture = Fixture::new()?;
    for (path, value) in [
        (&fixture.first, "A"),
        (&fixture.second, "B"),
        (&fixture.third, "C"),
    ] {
        fs::write(path, format!("set -ga {OPTION} {value}\n"))?;
    }
    let socket = &fixture.sockets[0];
    fixture.start(socket, &[])?;
    fixture.assert_value(socket, "ABC")?;
    assert_eq!(fixture.selection_output(socket)?, fixture.selection(true));
    Ok(())
}

#[test]
fn explicit_aliases_repeat_for_startup_source_and_nested_imports() -> TestResult {
    for hardlink in [false, true] {
        for entry in ["startup", "source", "nested"] {
            let fixture = Fixture::new()?;
            fixture.link_config(hardlink)?;
            let socket = &fixture.sockets[0];
            if entry == "startup" {
                fixture.start(socket, &[&fixture.first, &fixture.second])?;
            } else {
                fixture.start(socket, &[Path::new("/dev/null")])?;
                if entry == "source" {
                    fixture.run(
                        socket,
                        [
                            OsStr::new("source-file"),
                            fixture.first.as_os_str(),
                            fixture.second.as_os_str(),
                        ],
                    )?;
                } else {
                    let outer = fixture.root.join("imports.conf");
                    fs::write(
                        &outer,
                        format!(
                            "source-file '{}'\nsource-file '{}'\n",
                            config_quote(&fixture.first),
                            config_quote(&fixture.second),
                        ),
                    )?;
                    fixture.run(socket, [OsStr::new("source-file"), outer.as_os_str()])?;
                }
            }
            fixture.assert_value(socket, "XX")?;
            assert_eq!(fs::read_to_string(&fixture.first)?, APPEND_X);
            assert_eq!(fs::read_to_string(&fixture.second)?, APPEND_X);
        }
    }
    Ok(())
}

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    xdg: PathBuf,
    first: PathBuf,
    second: PathBuf,
    third: PathBuf,
    socket_root: PathBuf,
    sockets: [PathBuf; 2],
}

impl Fixture {
    fn new() -> TestResult<Self> {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(unique_fixture_name("d")?);
        Self::in_root(root)
    }

    fn in_root(root: PathBuf) -> TestResult<Self> {
        if Path::new("/etc/rmux.conf").exists() {
            return Err("default-discovery proof requires an absent /etc/rmux.conf".into());
        }
        // Exclusive creation: a shared root would let one test's files and
        // cleanup leak into another test running in parallel.
        fs::create_dir_all(root.parent().ok_or("fixture root has no parent")?)?;
        fs::create_dir(&root)?;
        let home = root.join("home");
        let xdg = root.join("xdg");
        fs::create_dir_all(home.join(".config/rmux"))?;
        fs::create_dir_all(xdg.join("rmux"))?;
        // Cargo's private target paths can exceed AF_UNIX's pathname limit.
        let socket_root = PathBuf::from("/tmp").join(unique_fixture_name("rmux-cfg-")?);
        fs::DirBuilder::new().mode(0o700).create(&socket_root)?;
        Ok(Self {
            first: home.join(".rmux.conf"),
            second: xdg.join("rmux/rmux.conf"),
            third: home.join(".config/rmux/rmux.conf"),
            sockets: [socket_root.join("0.sock"), socket_root.join("1.sock")],
            socket_root,
            root,
            home,
            xdg,
        })
    }

    fn link_config(&self, hardlink: bool) -> TestResult {
        fs::write(&self.first, APPEND_X)?;
        if hardlink {
            fs::hard_link(&self.first, &self.second)?;
        } else {
            symlink(&self.first, &self.second)?;
        }
        assert_eq!(identity(&self.first)?, identity(&self.second)?);
        Ok(())
    }

    fn start(&self, socket: &Path, configs: &[&Path]) -> TestResult {
        let mut args = Vec::new();
        for path in configs {
            args.extend([OsStr::new("-f"), path.as_os_str()]);
        }
        args.extend(["new-session", "-d", "-s", "cfg", "/bin/sleep", "120"].map(OsStr::new));
        self.run(socket, args)?;
        Ok(())
    }

    fn run<I, S>(&self, socket: &Path, args: I) -> TestResult<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = Command::new(rmux_os::build_identity::cargo_binary(env!(
            "CARGO_BIN_EXE_rmux"
        )))
        .env("HOME", &self.home)
        .env("XDG_CONFIG_HOME", &self.xdg)
        .env("RMUX_DISABLE_TMUX_FALLBACK", "1")
        .env_remove("TMUX")
        .env_remove("RMUX")
        .arg("-S")
        .arg(socket)
        .args(args)
        .output()?;
        if !output.status.success() {
            return Err(format!(
                "rmux exited {}: {}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            )
            .into());
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }

    fn assert_value(&self, socket: &Path, expected: &str) -> TestResult {
        assert_eq!(
            self.run(socket, ["show-options", "-gqv", OPTION])?,
            expected
        );
        Ok(())
    }

    fn selection_output(&self, socket: &Path) -> TestResult<String> {
        self.run(socket, ["display-message", "-p", "#{config_files}"])
    }

    fn selection(&self, include_second: bool) -> String {
        let mut paths = vec![Path::new("/etc/rmux.conf"), &self.first];
        if include_second {
            paths.push(&self.second);
        }
        paths.push(&self.third);
        paths
            .iter()
            .map(|p| p.to_string_lossy())
            .collect::<Vec<_>>()
            .join(",")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for socket in &self.sockets {
            let _ = self.run(socket, ["kill-server"]);
        }
        let _ = fs::remove_dir_all(&self.root);
        let _ = fs::remove_dir_all(&self.socket_root);
    }
}

/// Names one fixture directory, unique across the parallel tests of this
/// process. The clock alone is not enough: Darwin's realtime clock has only
/// microsecond resolution, so simultaneous tests could share a fixture. The
/// timestamp still separates runs that reuse a process id.
fn unique_fixture_name(prefix: &str) -> Result<String, SystemTimeError> {
    let serial = NEXT_FIXTURE_SERIAL.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(format!(
        "{prefix}{:x}-{serial:x}-{nanos:x}",
        std::process::id()
    ))
}

fn identity(path: &Path) -> std::io::Result<(u64, u64)> {
    let metadata = fs::metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

fn config_quote(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "'\\''")
}
