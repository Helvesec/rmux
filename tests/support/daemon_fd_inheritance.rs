// Shared native regression for the CLI and SDK daemon constructors.
use rustix::event::{poll, PollFd, PollFlags, Timespec};
use rustix::io::{fcntl_dupfd_cloexec, fcntl_setfd, FdFlags};
use rustix::process::{getrlimit, setrlimit, Resource};
use std::os::fd::{AsFd, AsRawFd};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

/// Hard limit of the "bounded" case: a descriptor table below 20_001.
const BOUNDED_TABLE: u64 = 16_384;

pub fn check(test_name: &str, spawn: fn(&Path, &Path) -> Child) {
    if std::env::var_os("RMUX_HIGH_FD_LAUNCHER").is_none() {
        for case in ["ordinary", "lowered", "bounded"] {
            let status = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test_name, "--nocapture"])
                .env("RMUX_HIGH_FD_LAUNCHER", case)
                .status()
                .unwrap();
            assert!(status.success(), "{case} inherited-fd launch failed");
        }
        return;
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap()
                .ancestors()
                .nth(3)
                .unwrap()
                .to_path_buf()
        });
    let fixture = target.join("debug/examples/startup_owned_fixture");
    if !fixture.is_file() {
        let status = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .args([
                "build",
                "--locked",
                "--offline",
                "-j2",
                "-p",
                "rmux-sdk",
                "--example",
                "startup_owned_fixture",
            ])
            .env("CARGO_TARGET_DIR", &target)
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .status()
            .unwrap();
        assert!(status.success(), "build finite daemon fixture");
    }
    let fixture = rmux_os::build_identity::resolve(fixture.as_os_str()).unwrap();
    // This subprocess alone changes its limits and owns every descriptor below.
    let case = std::env::var("RMUX_HIGH_FD_LAUNCHER").unwrap();
    let mut limit = getrlimit(Resource::Nofile);
    if case == "bounded" {
        // A table below 20_001 on every host, as kern.maxfilesperproc imposes
        // on the macos-15 arm64 runner.
        limit.maximum = Some(limit.maximum.unwrap_or(BOUNDED_TABLE).min(BOUNDED_TABLE));
    }
    limit.current = Some(limit.maximum.unwrap_or(32_768).min(32_768));
    setrlimit(Resource::Nofile, limit).unwrap();
    let (reader, writer) = rustix::pipe::pipe().unwrap();
    let table = usable_descriptor_table(&writer);
    let high_fd = 20_000.min(table - 8);
    assert!(
        high_fd > 1_024,
        "descriptor table of {table} has no high fd"
    );
    let high = fcntl_dupfd_cloexec(&writer, high_fd).unwrap();
    assert_eq!(high.as_raw_fd(), high_fd, "F_DUPFD in a table of {table}");
    fcntl_setfd(&high, FdFlags::empty()).unwrap();
    if case == "lowered" {
        limit.current = Some(64);
        limit.maximum = Some(64);
        setrlimit(Resource::Nofile, limit).unwrap();
    }
    let root = target.join(format!("fd-inheritance-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let mut child = spawn(&fixture, &root.join("daemon.sock"));
    drop((writer, high));
    let mut fds = [PollFd::new(&reader, PollFlags::IN | PollFlags::HUP)];
    let ready = poll(
        &mut fds,
        Some(&Timespec {
            tv_sec: 1,
            tv_nsec: 0,
        }),
    )
    .unwrap();
    let mut byte = [0_u8];
    let eof = if ready == 1 {
        Some(rustix::io::read(&reader, &mut byte).unwrap())
    } else {
        None
    };
    let alive_at_eof = child.try_wait().unwrap().is_none();
    // The finite fixture sleeps for three seconds; keep the original child
    // handle and wait for its natural exit even when the regression fails.
    let status = child.wait().unwrap();
    std::fs::remove_dir_all(root).unwrap();
    assert_eq!(eof, Some(0), "high inherited writer delayed EOF");
    assert!(alive_at_eof, "EOF must precede the daemon's natural exit");
    assert!(status.success());
}

/// Darwin accepts a soft limit above kern.maxfilesperproc but bounds the
/// usable table by it, so the high number follows the measured table.
/// F_DUPFD refuses with EINVAL exactly the arguments at or above that table,
/// which the soft limit set above keeps at or below 32_768.
fn usable_descriptor_table(fd: &impl AsFd) -> i32 {
    let (mut accepted, mut refused) = (0, 32_768);
    while refused - accepted > 1 {
        let probe = accepted + (refused - accepted) / 2;
        match fcntl_dupfd_cloexec(fd, probe) {
            Ok(_) => accepted = probe,
            Err(rustix::io::Errno::INVAL) => refused = probe,
            Err(error) => panic!("F_DUPFD({probe}): {error}"),
        }
    }
    refused
}
