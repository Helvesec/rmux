#![cfg(unix)]

mod common;

#[path = "tmux_compat_harness/owned_rescue.rs"]
mod owned_rescue;
use owned_rescue::{OwnedDaemonIdentity, OwnedDaemonRescue};

use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{
    FrozenTmuxBinary, TmuxCompatHarness, TmuxCompatRunConfig, DEFAULT_FROZEN_TMUX_PATH,
    DEFAULT_TMUX_COMPAT_TERM, FROZEN_TMUX_ENV, FROZEN_TMUX_REFERENCE_REL_PATH,
    PTY_SERIALIZATION_NOTE, REQUIRE_TMUX_ENV, TMUX_COMPAT_DIVERGENCE_LEDGER_REL_PATH,
    TMUX_COMPAT_PREREQUISITES_NOTE, TMUX_ORACLE_ENV,
};
use rmux_client::{connect_or_absent, ConnectResult};

#[test]
fn owned_rescue_preserves_initial_and_replacement_foreign_daemons() -> Result<(), Box<dyn Error>> {
    let mut failures = Vec::new();
    for replacement in [false, true] {
        let harness = TmuxCompatHarness::new("rescue-foreign")?;
        let root = common::unique_tmpdir("rescue-foreign");
        fs::create_dir_all(&root)?;
        let socket = common::default_socket_path_in(&root)?;
        let config = TmuxCompatRunConfig::default().with_tmpdir(root.clone());
        let mut rescue = OwnedDaemonRescue {
            socket: socket.clone(),
            identity: None,
        };
        if replacement {
            let start = harness
                .run_rmux_with(&["new-session", "-d", "-s", "owned", "sleep 60"], &config)?;
            assert_eq!(start.status_code, Some(0));
            let pid = harness.run_rmux_with(&["display-message", "-p", "#{pid}"], &config)?;
            let pid = pid.stdout_string().trim().parse::<u32>()?;
            rescue.identity = Some(OwnedDaemonIdentity::capture(pid)?);
            assert_eq!(
                harness
                    .run_rmux_with(&["kill-server"], &config)?
                    .status_code,
                Some(0)
            );
            wait_for_socket_absent(&socket, Duration::from_secs(5))?;
        }
        let binary =
            rmux_os::build_identity::resolve(std::ffi::OsStr::new(env!("CARGO_BIN_EXE_rmux")))?;
        let mut foreign = ForeignDaemonFixture(
            std::process::Command::new(binary)
                .arg(rmux_client::INTERNAL_DAEMON_FLAG)
                .env("RMUX_TMPDIR", &root)
                .env("HOME", harness.tmpdir().join("home"))
                .env("XDG_CONFIG_HOME", harness.tmpdir().join("xdg"))
                .env_remove("RMUX")
                .env_remove("TMUX")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?,
        );
        common::wait_for_socket(&socket, &mut foreign.0)?;
        assert!(
            harness.run_rmux_with(&["-V"], &config).is_err(),
            "prepare must reject a foreign occupant"
        );
        drop(rescue);
        let deadline = Instant::now() + Duration::from_secs(2);
        while foreign.0.try_wait()?.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if foreign.0.try_wait()?.is_some()
            || !matches!(connect_or_absent(&socket)?, ConnectResult::Connected(_))
        {
            failures.push(format!(
                "replacement={replacement}: rescue stopped the foreign daemon"
            ));
        }
        drop(harness);
        drop(foreign);
        fs::remove_dir_all(root)?;
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

#[test]
fn redirected_cleanup_preserves_foreign_daemons_after_version_and_replacement(
) -> Result<(), Box<dyn Error>> {
    let mut failures = Vec::new();
    for replaced in [false, true] {
        for rerun in [false, true] {
            let harness = TmuxCompatHarness::new("cleanup-foreign")?;
            let root = common::unique_tmpdir("cleanup-foreign-fixture");
            fs::create_dir_all(&root)?;
            let socket = common::default_socket_path_in(&root)?;
            let config = TmuxCompatRunConfig::default().with_tmpdir(root.clone());
            if replaced {
                let start = harness
                    .run_rmux_with(&["new-session", "-d", "-s", "owned", "sleep 60"], &config)?;
                assert_eq!(start.status_code, Some(0));
                let stop = harness.run_rmux_with(&["kill-server"], &config)?;
                assert_eq!(stop.status_code, Some(0));
                wait_for_socket_absent(&socket, Duration::from_secs(5))?;
            } else {
                let version = harness.run_rmux_with(&["-V"], &config)?;
                assert_eq!(version.status_code, Some(0));
                assert!(matches!(connect_or_absent(&socket)?, ConnectResult::Absent));
            }
            let binary =
                rmux_os::build_identity::resolve(std::ffi::OsStr::new(env!("CARGO_BIN_EXE_rmux")))?;
            let mut foreign = ForeignDaemonFixture(
                std::process::Command::new(binary)
                    .arg(rmux_client::INTERNAL_DAEMON_FLAG)
                    .env("RMUX_TMPDIR", &root)
                    .env("HOME", harness.tmpdir().join("home"))
                    .env("XDG_CONFIG_HOME", harness.tmpdir().join("xdg"))
                    .env_remove("RMUX")
                    .env_remove("TMUX")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()?,
            );
            common::wait_for_socket(&socket, &mut foreign.0)?;
            let identity = daemon_identity(foreign.0.id())?;
            assert!(!identity.is_empty());
            eprintln!("replaced={replaced} rerun={rerun} foreign={identity}");
            if rerun && harness.run_rmux_with(&["-V"], &config).is_ok() {
                failures.push(format!("replaced={replaced}: accepted a foreign occupant"));
            }
            drop(harness);
            if daemon_identity(foreign.0.id())? != identity
                || !matches!(connect_or_absent(&socket)?, ConnectResult::Connected(_))
            {
                failures.push(format!(
                    "replaced={replaced} rerun={rerun}: stopped foreign daemon"
                ));
            }
            drop(foreign);
            fs::remove_dir_all(root)?;
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

struct ForeignDaemonFixture(std::process::Child);

impl Drop for ForeignDaemonFixture {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn redirected_tmpdir_cleanup_reaps_owned_daemon_after_success_failure_and_timeout(
) -> Result<(), Box<dyn Error>> {
    for outcome in ["success", "failure", "timeout"] {
        let harness = TmuxCompatHarness::new("cleanup-redirect")?;
        let root = common::unique_tmpdir("cleanup-redirect-owned");
        fs::create_dir_all(&root)?;
        let socket = common::default_socket_path_in(&root)?;
        let mut rescue = OwnedDaemonRescue {
            socket: socket.clone(),
            identity: None,
        };
        let config = TmuxCompatRunConfig::default().with_tmpdir(root.clone());
        let start =
            harness.run_rmux_with(&["new-session", "-d", "-s", "alpha", "sleep 60"], &config)?;
        assert_eq!(start.status_code, Some(0));
        let pid = harness.run_rmux_with(&["display-message", "-p", "#{pid}"], &config)?;
        let pid = pid.stdout_string().trim().parse::<u32>()?;
        let identity = daemon_identity(pid)?;
        assert!(identity.contains(socket.to_str().ok_or("socket is not UTF-8")?));
        eprintln!(
            "outcome={outcome} socket={} identity={identity}",
            socket.display()
        );
        rescue.identity = Some(OwnedDaemonIdentity::capture(pid)?);
        assert!(
            matches!(connect_or_absent(&socket)?, ConnectResult::Connected(_)),
            "departing command clients must preserve the live detached session"
        );
        let finish = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match outcome {
                "failure" => {
                    let result = harness
                        .run_rmux_with(&["has-session", "-t", "missing"], &config)
                        .unwrap();
                    assert_eq!(result.status_code, Some(1));
                    panic!("deliberate test failure");
                }
                "timeout" => {
                    let result = harness
                        .run_rmux_with(
                            &["run-shell", "sleep 60"],
                            &config.clone().with_timeout(Duration::from_millis(100)),
                        )
                        .unwrap();
                    assert!(result.timed_out);
                }
                _ => {}
            }
            drop(harness);
        }));
        assert_eq!(finish.is_err(), outcome == "failure");
        let deadline = Instant::now() + Duration::from_secs(5);
        while daemon_identity(pid)? == identity && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_ne!(
            daemon_identity(pid)?,
            identity,
            "{outcome}: harness left its daemon alive after teardown at {}",
            socket.display()
        );
        drop(rescue);
        fs::remove_dir_all(root)?;
    }
    Ok(())
}

fn daemon_identity(pid: u32) -> Result<String, Box<dyn Error>> {
    let output = std::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "pid=,ppid=,lstart=,command="])
        .output()?;
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

#[test]
fn tmux_compat_harness_uses_distinct_socket_paths_under_one_temp_root() -> Result<(), Box<dyn Error>>
{
    let harness = TmuxCompatHarness::new("tmux-compat-sockets")?;

    assert!(harness.rmux_socket_path() != harness.tmux_socket_path());
    let tmpdir = fs::canonicalize(harness.tmpdir())?;
    let rmux_socket_dir = canonical_socket_parent(harness.rmux_socket_path())?;
    let tmux_socket_dir = canonical_socket_parent(harness.tmux_socket_path())?;
    assert!(rmux_socket_dir.starts_with(&tmpdir));
    assert!(tmux_socket_dir.starts_with(&tmpdir));
    assert!(PTY_SERIALIZATION_NOTE.contains("PTY-heavy tmux compatibility cases"));
    harness.assert_socket_dirs_clean()?;
    Ok(())
}

fn canonical_socket_parent(socket_path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let parent = socket_path
        .parent()
        .ok_or("socket path must have a parent")?;
    fs::create_dir_all(parent)?;
    Ok(fs::canonicalize(parent)?)
}

#[test]
fn tmux_compat_harness_records_oracle_prerequisites() {
    assert!(TMUX_COMPAT_PREREQUISITES_NOTE.contains("attached-client"));
    assert!(TMUX_COMPAT_PREREQUISITES_NOTE.contains("PTY"));
    assert!(TMUX_COMPAT_PREREQUISITES_NOTE.contains("80x24"));
    assert!(TMUX_COMPAT_PREREQUISITES_NOTE.contains(DEFAULT_TMUX_COMPAT_TERM));
    assert!(TMUX_COMPAT_PREREQUISITES_NOTE.contains("LC_ALL/LC_CTYPE=C.UTF-8"));
    assert!(TMUX_COMPAT_PREREQUISITES_NOTE.contains("frozen tmux 3.7b authority record"));
    assert!(TMUX_COMPAT_PREREQUISITES_NOTE.contains(TMUX_COMPAT_DIVERGENCE_LEDGER_REL_PATH));
    assert!(TMUX_COMPAT_PREREQUISITES_NOTE.contains("man"));
    assert!(DEFAULT_FROZEN_TMUX_PATH.contains("/tmux-frozen/"));
    assert!(DEFAULT_FROZEN_TMUX_PATH.contains("e802909de06012a4df6209d55e86487c56223163"));
    assert!(FROZEN_TMUX_REFERENCE_REL_PATH.ends_with("frozen_reference.yaml"));
    assert!(TMUX_COMPAT_DIVERGENCE_LEDGER_REL_PATH.ends_with("divergences.toml"));
}

#[test]
fn tmux_compat_harness_runs_same_argv_when_frozen_tmux_is_available() -> Result<(), Box<dyn Error>>
{
    let harness = TmuxCompatHarness::new("tmux-compat-version")?;
    let tmux_binary = match FrozenTmuxBinary::discover() {
        FrozenTmuxBinary::Available(path) => path,
        FrozenTmuxBinary::Unavailable {
            checked_path,
            reason,
        } => {
            eprintln!(
                "runtime skip: frozen tmux binary unavailable via {FROZEN_TMUX_ENV} or default '{}': {reason}",
                checked_path.display()
            );
            harness.assert_socket_dirs_clean()?;
            return Ok(());
        }
    };

    let config = TmuxCompatRunConfig::default()
        .with_timeout(Duration::from_secs(3))
        .with_term("xterm-256color")
        .without_tmux();
    let run = harness.run_pair_with(&tmux_binary, &["-V"], config)?;

    assert_eq!(run.tmux.requested_argv, run.rmux.requested_argv);
    assert_eq!(run.tmux.program, "tmux");
    assert_eq!(run.rmux.program, "rmux");
    assert_eq!(run.tmux.program_path, tmux_binary);
    assert_eq!(run.rmux.effective_argv, vec![OsString::from("-V")]);
    assert_eq!(run.rmux.socket_dir, harness.rmux_socket_dir());
    assert_eq!(run.tmux.socket_dir, harness.tmux_socket_dir());
    assert_eq!(run.rmux.timeout, Duration::from_secs(3));
    assert_eq!(run.tmux.timeout, Duration::from_secs(3));
    assert_eq!(
        run.rmux.environment_overrides,
        vec![
            (
                OsString::from("TMPDIR"),
                Some(harness.tmpdir().as_os_str().to_owned())
            ),
            (
                OsString::from("RMUX_TMPDIR"),
                Some(harness.tmpdir().as_os_str().to_owned())
            ),
            (
                OsString::from("TMUX_TMPDIR"),
                Some(harness.tmpdir().as_os_str().to_owned())
            ),
            (
                OsString::from("HOME"),
                Some(harness.tmpdir().join("home").into_os_string())
            ),
            (
                OsString::from("XDG_CONFIG_HOME"),
                Some(harness.tmpdir().join("xdg").into_os_string())
            ),
            (OsString::from("TMUX"), None),
            (
                OsString::from("TERM"),
                Some(OsString::from(DEFAULT_TMUX_COMPAT_TERM))
            ),
        ]
    );
    assert_eq!(
        run.tmux.environment_overrides,
        run.rmux.environment_overrides
    );
    assert_eq!(
        run.tmux.effective_argv,
        vec![
            OsString::from("-f"),
            OsString::from("/dev/null"),
            OsString::from("-S"),
            harness.tmux_socket_path().as_os_str().to_owned(),
            OsString::from("-V"),
        ]
    );
    assert_eq!(run.tmux.status_code, Some(0));
    assert_eq!(run.rmux.status_code, Some(0));
    assert!(!run.tmux.timed_out);
    assert!(!run.rmux.timed_out);
    assert!(
        run.tmux.stdout_string().starts_with("tmux "),
        "unexpected tmux stdout: {:?}",
        run.tmux.stdout_string()
    );
    assert!(
        run.rmux.stdout_string().starts_with("rmux "),
        "unexpected rmux stdout: {:?}",
        run.rmux.stdout_string()
    );
    assert!(run.tmux.stderr_string().is_empty());
    assert!(run.rmux.stderr_string().is_empty());
    harness.assert_socket_dirs_clean()?;
    Ok(())
}

#[test]
fn frozen_tmux_discovery_rejects_host_system_tmux() {
    match FrozenTmuxBinary::discover_at(PathBuf::from("/usr/bin/tmux")) {
        FrozenTmuxBinary::Available(path) => {
            panic!("system tmux must not be accepted as frozen reference: {path:?}")
        }
        FrozenTmuxBinary::Unavailable { reason, .. } => {
            assert!(reason.contains("not the frozen reference build"));
        }
    }
}

#[test]
fn frozen_tmux_discovery_rejects_unrecorded_executables() -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-unrecorded-frozen-tmux")?;
    let fake_tmux = harness.tmpdir().join("fake-discover-tmux");
    write_fake_tmux(&fake_tmux)?;

    match FrozenTmuxBinary::discover_at(fake_tmux.clone()) {
        FrozenTmuxBinary::Available(path) => {
            panic!("unrecorded executable must not be accepted as frozen reference: {path:?}")
        }
        FrozenTmuxBinary::Unavailable {
            checked_path,
            reason,
        } => {
            assert_eq!(checked_path, fake_tmux);
            assert!(
                reason.contains("frozen tmux reference") || reason.contains("recorded frozen tmux"),
                "unexpected rejection reason: {reason}"
            );
        }
    }

    Ok(())
}

#[test]
fn frozen_tmux_discovery_hard_fails_when_required_and_missing() {
    let _lock = common::env_lock().lock().expect("env lock");
    let _restore_required =
        common::EnvVarGuard::new(REQUIRE_TMUX_ENV, std::env::var_os(REQUIRE_TMUX_ENV));
    let _restore_oracle =
        common::EnvVarGuard::new(TMUX_ORACLE_ENV, std::env::var_os(TMUX_ORACLE_ENV));
    let _restore_frozen =
        common::EnvVarGuard::new(FROZEN_TMUX_ENV, std::env::var_os(FROZEN_TMUX_ENV));

    std::env::set_var(REQUIRE_TMUX_ENV, "1");
    std::env::set_var(TMUX_ORACLE_ENV, "/definitely/missing/rmux-tmux-3.7b");
    std::env::remove_var(FROZEN_TMUX_ENV);

    let panic = std::panic::catch_unwind(FrozenTmuxBinary::discover)
        .expect_err("required missing oracle must panic instead of runtime-skipping");
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("<non-string panic>");

    assert!(message.contains("tmux 3.7b oracle is required"));
    assert!(message.contains(REQUIRE_TMUX_ENV));
}

#[test]
fn tmux_compat_harness_records_effective_tmux_argv_and_env_overrides() -> Result<(), Box<dyn Error>>
{
    let harness = TmuxCompatHarness::new("tmux-compat-fake-tmux")?;
    let fake_tmux = harness.tmpdir().join("fake-tmux");
    write_fake_tmux(&fake_tmux)?;
    let override_tmpdir = harness.tmpdir().join("override-tmpdir");
    fs::create_dir_all(&override_tmpdir)?;

    let config = TmuxCompatRunConfig::default()
        .with_timeout(Duration::from_secs(3))
        .with_tmpdir(override_tmpdir.clone())
        .with_tmux("inside-tmux")
        .with_term("screen-256color")
        .with_env("RMUX_TEST_EXTRA", "set");
    let run = harness.run_pair_with(&fake_tmux, &["-V"], config)?;

    assert_eq!(run.tmux.program, "tmux");
    assert_eq!(run.tmux.program_path, fake_tmux);
    assert_eq!(run.tmux.requested_argv, vec![OsString::from("-V")]);
    assert_eq!(
        run.tmux.effective_argv,
        vec![
            OsString::from("-f"),
            OsString::from("/dev/null"),
            OsString::from("-S"),
            harness.tmux_socket_path().as_os_str().to_owned(),
            OsString::from("-V"),
        ]
    );
    assert_eq!(run.tmux.socket_dir, harness.tmux_socket_dir());
    assert_eq!(run.rmux.socket_dir, harness.rmux_socket_dir());
    assert_eq!(run.tmux.timeout, Duration::from_secs(3));
    assert_eq!(run.rmux.timeout, Duration::from_secs(3));
    assert_eq!(
        run.tmux.environment_overrides,
        vec![
            (
                OsString::from("TMPDIR"),
                Some(override_tmpdir.as_os_str().to_owned())
            ),
            (
                OsString::from("RMUX_TMPDIR"),
                Some(override_tmpdir.as_os_str().to_owned())
            ),
            (
                OsString::from("TMUX_TMPDIR"),
                Some(override_tmpdir.as_os_str().to_owned())
            ),
            (
                OsString::from("HOME"),
                Some(harness.tmpdir().join("home").into_os_string())
            ),
            (
                OsString::from("XDG_CONFIG_HOME"),
                Some(harness.tmpdir().join("xdg").into_os_string())
            ),
            (OsString::from("TMUX"), Some(OsString::from("inside-tmux"))),
            (
                OsString::from("TERM"),
                Some(OsString::from("screen-256color"))
            ),
            (
                OsString::from("RMUX_TEST_EXTRA"),
                Some(OsString::from("set"))
            ),
        ]
    );
    assert_eq!(
        run.rmux.environment_overrides,
        run.tmux.environment_overrides
    );
    assert_eq!(run.tmux.status_code, Some(0));
    assert!(!run.tmux.timed_out);

    let tmux_stdout = run.tmux.stdout_string();
    assert!(
        tmux_stdout.contains(&format!("TMPDIR={}", override_tmpdir.display())),
        "fake tmux did not receive configured TMPDIR: {tmux_stdout:?}"
    );
    assert!(
        tmux_stdout.contains(&format!("RMUX_TMPDIR={}", override_tmpdir.display())),
        "fake tmux did not receive configured RMUX_TMPDIR: {tmux_stdout:?}"
    );
    assert!(
        tmux_stdout.contains(&format!("TMUX_TMPDIR={}", override_tmpdir.display())),
        "fake tmux did not receive configured TMUX_TMPDIR: {tmux_stdout:?}"
    );
    assert!(tmux_stdout.contains("TMUX=inside-tmux"));
    assert!(tmux_stdout.contains("TERM=screen-256color"));
    assert!(tmux_stdout.contains("EXTRA=set"));
    assert!(tmux_stdout.contains(&format!(
        "ARGV=[-f][/dev/null][-S][{}][-V]",
        harness.tmux_socket_path().display()
    )));

    assert_eq!(run.rmux.requested_argv, vec![OsString::from("-V")]);
    assert_eq!(run.rmux.effective_argv, vec![OsString::from("-V")]);
    assert_eq!(run.rmux.status_code, Some(0));
    assert!(!run.rmux.timed_out);
    assert_eq!(
        run.rmux.stdout_string(),
        format!("rmux {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(run.rmux.stderr_string().is_empty());
    harness.assert_socket_dirs_clean()?;
    Ok(())
}

#[test]
fn tmux_compat_wait_for_signal_round_trip_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-wait-for")?;
    let tmux_binary = match FrozenTmuxBinary::discover() {
        FrozenTmuxBinary::Available(path) => path,
        FrozenTmuxBinary::Unavailable {
            checked_path,
            reason,
        } => {
            eprintln!(
                "runtime skip: frozen tmux binary unavailable via {FROZEN_TMUX_ENV} or default '{}': {reason}",
                checked_path.display()
            );
            harness.assert_socket_dirs_clean()?;
            return Ok(());
        }
    };

    let config = TmuxCompatRunConfig::default()
        .with_timeout(Duration::from_secs(3))
        .with_term("xterm-256color")
        .without_tmux();

    let start = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha"],
        config.clone(),
    )?;
    assert_eq!(start.tmux.status_code, Some(0));
    assert_eq!(start.rmux.status_code, Some(0));
    assert_eq!(start.tmux.stdout, start.rmux.stdout);
    assert_eq!(start.tmux.stderr, start.rmux.stderr);
    assert!(!start.tmux.timed_out);
    assert!(!start.rmux.timed_out);

    let signal =
        harness.run_pair_with(&tmux_binary, &["wait-for", "-S", "ready"], config.clone())?;
    assert_eq!(signal.tmux.status_code, signal.rmux.status_code);
    assert_eq!(signal.tmux.timed_out, signal.rmux.timed_out);
    assert_eq!(signal.tmux.stdout, signal.rmux.stdout);
    assert_eq!(signal.tmux.stderr, signal.rmux.stderr);

    let wait = harness.run_pair_with(&tmux_binary, &["wait-for", "ready"], config)?;
    assert_eq!(wait.tmux.status_code, wait.rmux.status_code);
    assert_eq!(wait.tmux.timed_out, wait.rmux.timed_out);
    assert_eq!(wait.tmux.stdout, wait.rmux.stdout);
    assert_eq!(wait.tmux.stderr, wait.rmux.stderr);

    let kill = harness.run_pair_with(
        &tmux_binary,
        &["kill-server"],
        TmuxCompatRunConfig::default()
            .with_timeout(Duration::from_secs(3))
            .with_term("xterm-256color")
            .without_tmux(),
    )?;
    assert_eq!(kill.tmux.status_code, kill.rmux.status_code);
    assert_eq!(kill.tmux.timed_out, kill.rmux.timed_out);
    assert_eq!(kill.tmux.stdout, kill.rmux.stdout);
    assert_eq!(kill.tmux.stderr, kill.rmux.stderr);
    wait_for_socket_absent(harness.tmux_socket_path(), Duration::from_secs(3))?;
    wait_for_socket_absent(harness.rmux_socket_path(), Duration::from_secs(3))?;
    let _ = fs::remove_file(harness.tmux_socket_path());
    let _ = fs::remove_file(harness.rmux_socket_path());
    harness.wait_for_socket_dirs_clean(Duration::from_secs(3))?;
    Ok(())
}

#[test]
fn tmux_compat_wait_for_lock_and_unlock_round_trip_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-wait-for-lock")?;
    let tmux_binary = match FrozenTmuxBinary::discover() {
        FrozenTmuxBinary::Available(path) => path,
        FrozenTmuxBinary::Unavailable {
            checked_path,
            reason,
        } => {
            eprintln!(
                "runtime skip: frozen tmux binary unavailable via {FROZEN_TMUX_ENV} or default '{}': {reason}",
                checked_path.display()
            );
            harness.assert_socket_dirs_clean()?;
            return Ok(());
        }
    };

    let config = TmuxCompatRunConfig::default()
        .with_timeout(Duration::from_secs(3))
        .with_term("xterm-256color")
        .without_tmux();

    let start = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha"],
        config.clone(),
    )?;
    assert_eq!(start.tmux.status_code, Some(0));
    assert_eq!(start.rmux.status_code, Some(0));
    assert_eq!(start.tmux.stdout, start.rmux.stdout);
    assert_eq!(start.tmux.stderr, start.rmux.stderr);
    assert!(!start.tmux.timed_out);
    assert!(!start.rmux.timed_out);

    let lock = harness.run_pair_with(&tmux_binary, &["wait-for", "-L", "check"], config.clone())?;
    assert_eq!(lock.tmux.status_code, lock.rmux.status_code);
    assert_eq!(lock.tmux.timed_out, lock.rmux.timed_out);
    assert_eq!(lock.tmux.stdout, lock.rmux.stdout);
    assert_eq!(lock.tmux.stderr, lock.rmux.stderr);

    let unlock =
        harness.run_pair_with(&tmux_binary, &["wait-for", "-U", "check"], config.clone())?;
    assert_eq!(unlock.tmux.status_code, unlock.rmux.status_code);
    assert_eq!(unlock.tmux.timed_out, unlock.rmux.timed_out);
    assert_eq!(unlock.tmux.stdout, unlock.rmux.stdout);
    assert_eq!(unlock.tmux.stderr, unlock.rmux.stderr);

    let alias_lock =
        harness.run_pair_with(&tmux_binary, &["wait", "-L", "check"], config.clone())?;
    assert_eq!(alias_lock.tmux.status_code, alias_lock.rmux.status_code);
    assert_eq!(alias_lock.tmux.timed_out, alias_lock.rmux.timed_out);
    assert_eq!(alias_lock.tmux.stdout, alias_lock.rmux.stdout);
    assert_eq!(alias_lock.tmux.stderr, alias_lock.rmux.stderr);

    let alias_unlock = harness.run_pair_with(&tmux_binary, &["wait", "-U", "check"], config)?;
    assert_eq!(alias_unlock.tmux.status_code, alias_unlock.rmux.status_code);
    assert_eq!(alias_unlock.tmux.timed_out, alias_unlock.rmux.timed_out);
    assert_eq!(alias_unlock.tmux.stdout, alias_unlock.rmux.stdout);
    assert_eq!(alias_unlock.tmux.stderr, alias_unlock.rmux.stderr);

    let kill = harness.run_pair_with(
        &tmux_binary,
        &["kill-server"],
        TmuxCompatRunConfig::default()
            .with_timeout(Duration::from_secs(3))
            .with_term("xterm-256color")
            .without_tmux(),
    )?;
    assert_eq!(kill.tmux.status_code, kill.rmux.status_code);
    assert_eq!(kill.tmux.timed_out, kill.rmux.timed_out);
    assert_eq!(kill.tmux.stdout, kill.rmux.stdout);
    assert_eq!(kill.tmux.stderr, kill.rmux.stderr);
    wait_for_socket_absent(harness.tmux_socket_path(), Duration::from_secs(3))?;
    wait_for_socket_absent(harness.rmux_socket_path(), Duration::from_secs(3))?;
    let _ = fs::remove_file(harness.tmux_socket_path());
    let _ = fs::remove_file(harness.rmux_socket_path());
    harness.wait_for_socket_dirs_clean(Duration::from_secs(3))?;
    Ok(())
}

#[test]
fn tmux_compat_kill_session_tears_down_last_session_server_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-kill-session-last-session-exit")?;
    let tmux_binary = match FrozenTmuxBinary::discover() {
        FrozenTmuxBinary::Available(path) => path,
        FrozenTmuxBinary::Unavailable {
            checked_path,
            reason,
        } => {
            eprintln!(
                "runtime skip: frozen tmux binary unavailable via {FROZEN_TMUX_ENV} or default '{}': {reason}",
                checked_path.display()
            );
            harness.assert_socket_dirs_clean()?;
            return Ok(());
        }
    };

    let config = tmux_compat_config_with_clean_homes(&harness)?;

    // Starting the fixture may exceed three seconds under CPU contention.
    // Keep the three-second budget for kill-session and teardown below.
    let start = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha"],
        config.clone().with_timeout(Duration::from_secs(30)),
    )?;
    assert_eq!(start.tmux.status_code, Some(0));
    assert_eq!(start.rmux.status_code, Some(0));
    assert_eq!(start.tmux.stdout, start.rmux.stdout);
    assert_eq!(start.tmux.stderr, start.rmux.stderr);
    assert!(!start.tmux.timed_out);
    assert!(!start.rmux.timed_out);

    let kill = harness.run_pair_with(&tmux_binary, &["kill-session", "-t", "alpha"], config)?;
    assert_eq!(kill.tmux.status_code, kill.rmux.status_code);
    assert_eq!(kill.tmux.timed_out, kill.rmux.timed_out);
    assert_eq!(kill.tmux.stdout, kill.rmux.stdout);
    assert_eq!(kill.tmux.stderr, kill.rmux.stderr);
    assert_eq!(kill.tmux.status_code, Some(0));
    assert_eq!(kill.rmux.status_code, Some(0));

    wait_for_socket_absent(harness.tmux_socket_path(), Duration::from_secs(3))?;
    wait_for_socket_absent(harness.rmux_socket_path(), Duration::from_secs(3))?;

    let _ = fs::remove_file(harness.tmux_socket_path());
    let _ = fs::remove_file(harness.rmux_socket_path());
    harness.wait_for_socket_dirs_clean(Duration::from_secs(3))?;
    Ok(())
}

#[test]
fn tmux_compat_window_ids_remain_global_after_killing_another_session_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-window-ids-global")?;
    let tmux_binary = match FrozenTmuxBinary::discover() {
        FrozenTmuxBinary::Available(path) => path,
        FrozenTmuxBinary::Unavailable {
            checked_path,
            reason,
        } => {
            eprintln!(
                "runtime skip: frozen tmux binary unavailable via {FROZEN_TMUX_ENV} or default '{}': {reason}",
                checked_path.display()
            );
            harness.assert_socket_dirs_clean()?;
            return Ok(());
        }
    };

    let config = tmux_compat_config_with_clean_homes(&harness)?;

    let create_alpha = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha"],
        config.clone(),
    )?;
    assert_eq!(create_alpha.tmux.status_code, Some(0));
    assert_eq!(create_alpha.rmux.status_code, Some(0));
    assert_eq!(create_alpha.tmux.stdout, create_alpha.rmux.stdout);
    assert_eq!(create_alpha.tmux.stderr, create_alpha.rmux.stderr);

    let create_beta = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "beta"],
        config.clone(),
    )?;
    assert_eq!(create_beta.tmux.status_code, Some(0));
    assert_eq!(create_beta.rmux.status_code, Some(0));
    assert_eq!(create_beta.tmux.stdout, create_beta.rmux.stdout);
    assert_eq!(create_beta.tmux.stderr, create_beta.rmux.stderr);

    let alpha_window = harness.run_pair_with(
        &tmux_binary,
        &["display-message", "-p", "-t", "alpha", "#{window_id}"],
        config.clone(),
    )?;
    assert_eq!(alpha_window.tmux.status_code, Some(0));
    assert_eq!(alpha_window.rmux.status_code, Some(0));
    assert_eq!(alpha_window.tmux.stdout, alpha_window.rmux.stdout);
    assert_eq!(alpha_window.tmux.stderr, alpha_window.rmux.stderr);

    let beta_window = harness.run_pair_with(
        &tmux_binary,
        &["display-message", "-p", "-t", "beta", "#{window_id}"],
        config.clone(),
    )?;
    assert_eq!(beta_window.tmux.status_code, Some(0));
    assert_eq!(beta_window.rmux.status_code, Some(0));
    assert_eq!(beta_window.tmux.stdout, beta_window.rmux.stdout);
    assert_eq!(beta_window.tmux.stderr, beta_window.rmux.stderr);

    let alpha_window_id = parse_window_id(&alpha_window.tmux.stdout_string())?;
    let beta_window_id = parse_window_id(&beta_window.tmux.stdout_string())?;
    assert!(beta_window_id > alpha_window_id);

    let kill_alpha = harness.run_pair_with(
        &tmux_binary,
        &["kill-session", "-t", "alpha"],
        config.clone(),
    )?;
    assert_eq!(kill_alpha.tmux.status_code, kill_alpha.rmux.status_code);
    assert_eq!(kill_alpha.tmux.stdout, kill_alpha.rmux.stdout);
    assert_eq!(kill_alpha.tmux.stderr, kill_alpha.rmux.stderr);

    let create_beta_window = harness.run_pair_with(
        &tmux_binary,
        &["new-window", "-d", "-t", "beta"],
        config.clone(),
    )?;
    assert_eq!(create_beta_window.tmux.status_code, Some(0));
    assert_eq!(create_beta_window.rmux.status_code, Some(0));
    assert_eq!(
        create_beta_window.tmux.stdout,
        create_beta_window.rmux.stdout
    );
    assert_eq!(
        create_beta_window.tmux.stderr,
        create_beta_window.rmux.stderr
    );

    let beta_new_window = harness.run_pair_with(
        &tmux_binary,
        &["display-message", "-p", "-t", "beta:1", "#{window_id}"],
        config.clone(),
    )?;
    assert_eq!(beta_new_window.tmux.status_code, Some(0));
    assert_eq!(beta_new_window.rmux.status_code, Some(0));
    assert_eq!(beta_new_window.tmux.stdout, beta_new_window.rmux.stdout);
    assert_eq!(beta_new_window.tmux.stderr, beta_new_window.rmux.stderr);

    let beta_new_window_id = parse_window_id(&beta_new_window.tmux.stdout_string())?;
    assert!(beta_new_window_id > beta_window_id);

    let kill = harness.run_pair_with(
        &tmux_binary,
        &["kill-server"],
        TmuxCompatRunConfig::default()
            .with_timeout(Duration::from_secs(3))
            .with_term("xterm-256color")
            .without_tmux(),
    )?;
    assert_eq!(kill.tmux.status_code, kill.rmux.status_code);
    assert_eq!(kill.tmux.timed_out, kill.rmux.timed_out);
    assert_eq!(kill.tmux.stdout, kill.rmux.stdout);
    assert_eq!(kill.tmux.stderr, kill.rmux.stderr);
    wait_for_socket_absent(harness.tmux_socket_path(), Duration::from_secs(3))?;
    wait_for_socket_absent(harness.rmux_socket_path(), Duration::from_secs(3))?;
    let _ = fs::remove_file(harness.tmux_socket_path());
    let _ = fs::remove_file(harness.rmux_socket_path());
    harness.wait_for_socket_dirs_clean(Duration::from_secs(3))?;
    Ok(())
}

#[test]
fn tmux_compat_window_ids_do_not_reuse_after_kill_window_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-window-ids-kill-window")?;
    let tmux_binary = match FrozenTmuxBinary::discover() {
        FrozenTmuxBinary::Available(path) => path,
        FrozenTmuxBinary::Unavailable {
            checked_path,
            reason,
        } => {
            eprintln!(
                "runtime skip: frozen tmux binary unavailable via {FROZEN_TMUX_ENV} or default '{}': {reason}",
                checked_path.display()
            );
            harness.assert_socket_dirs_clean()?;
            return Ok(());
        }
    };

    let config = tmux_compat_config_with_clean_homes(&harness)?;

    let create_alpha = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha"],
        config.clone(),
    )?;
    assert_eq!(create_alpha.tmux.status_code, Some(0));
    assert_eq!(create_alpha.rmux.status_code, Some(0));
    assert_eq!(create_alpha.tmux.stdout, create_alpha.rmux.stdout);
    assert_eq!(create_alpha.tmux.stderr, create_alpha.rmux.stderr);

    let initial_window = harness.run_pair_with(
        &tmux_binary,
        &["display-message", "-p", "-t", "alpha:0", "#{window_id}"],
        config.clone(),
    )?;
    assert_eq!(initial_window.tmux.stdout, initial_window.rmux.stdout);
    let initial_id = parse_window_id(&initial_window.tmux.stdout_string())?;

    let create_second = harness.run_pair_with(
        &tmux_binary,
        &["new-window", "-d", "-t", "alpha"],
        config.clone(),
    )?;
    assert_eq!(create_second.tmux.status_code, Some(0));
    assert_eq!(create_second.rmux.status_code, Some(0));
    assert_eq!(create_second.tmux.stdout, create_second.rmux.stdout);

    let second_window = harness.run_pair_with(
        &tmux_binary,
        &["display-message", "-p", "-t", "alpha:1", "#{window_id}"],
        config.clone(),
    )?;
    assert_eq!(second_window.tmux.stdout, second_window.rmux.stdout);
    let second_id = parse_window_id(&second_window.tmux.stdout_string())?;
    assert!(second_id > initial_id);

    let kill_second = harness.run_pair_with(
        &tmux_binary,
        &["kill-window", "-t", "alpha:1"],
        config.clone(),
    )?;
    assert_eq!(kill_second.tmux.status_code, kill_second.rmux.status_code);
    assert_eq!(kill_second.tmux.stdout, kill_second.rmux.stdout);
    assert_eq!(kill_second.tmux.stderr, kill_second.rmux.stderr);

    let create_third = harness.run_pair_with(
        &tmux_binary,
        &["new-window", "-d", "-t", "alpha"],
        config.clone(),
    )?;
    assert_eq!(create_third.tmux.status_code, Some(0));
    assert_eq!(create_third.rmux.status_code, Some(0));
    assert_eq!(create_third.tmux.stdout, create_third.rmux.stdout);

    let third_window = harness.run_pair_with(
        &tmux_binary,
        &["display-message", "-p", "-t", "alpha:1", "#{window_id}"],
        config.clone(),
    )?;
    assert_eq!(third_window.tmux.stdout, third_window.rmux.stdout);
    let third_id = parse_window_id(&third_window.tmux.stdout_string())?;
    assert!(third_id > second_id);

    let kill = harness.run_pair_with(
        &tmux_binary,
        &["kill-server"],
        TmuxCompatRunConfig::default()
            .with_timeout(Duration::from_secs(3))
            .with_term("xterm-256color")
            .without_tmux(),
    )?;
    assert_eq!(kill.tmux.status_code, kill.rmux.status_code);
    assert_eq!(kill.tmux.timed_out, kill.rmux.timed_out);
    assert_eq!(kill.tmux.stdout, kill.rmux.stdout);
    assert_eq!(kill.tmux.stderr, kill.rmux.stderr);
    wait_for_socket_absent(harness.tmux_socket_path(), Duration::from_secs(3))?;
    wait_for_socket_absent(harness.rmux_socket_path(), Duration::from_secs(3))?;
    let _ = fs::remove_file(harness.tmux_socket_path());
    let _ = fs::remove_file(harness.rmux_socket_path());
    harness.wait_for_socket_dirs_clean(Duration::from_secs(3))?;
    Ok(())
}

#[test]
fn tmux_compat_link_and_unlink_window_round_trip_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-link-unlink-window")?;
    let tmux_binary = match FrozenTmuxBinary::discover() {
        FrozenTmuxBinary::Available(path) => path,
        FrozenTmuxBinary::Unavailable {
            checked_path,
            reason,
        } => {
            eprintln!(
                "runtime skip: frozen tmux binary unavailable via {FROZEN_TMUX_ENV} or default '{}': {reason}",
                checked_path.display()
            );
            harness.assert_socket_dirs_clean()?;
            return Ok(());
        }
    };

    let config = TmuxCompatRunConfig::default()
        .with_timeout(Duration::from_secs(3))
        .with_term("xterm-256color")
        .without_tmux();

    let start_alpha = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha"],
        config.clone(),
    )?;
    assert_eq!(start_alpha.tmux.status_code, Some(0));
    assert_eq!(start_alpha.rmux.status_code, Some(0));
    assert_eq!(start_alpha.tmux.stdout, start_alpha.rmux.stdout);
    assert_eq!(start_alpha.tmux.stderr, start_alpha.rmux.stderr);

    let unlink_error = harness.run_pair_with(
        &tmux_binary,
        &["unlink-window", "-t", "alpha:0"],
        config.clone(),
    )?;
    assert_eq!(unlink_error.tmux.status_code, unlink_error.rmux.status_code);
    assert_eq!(unlink_error.tmux.timed_out, unlink_error.rmux.timed_out);
    assert_eq!(unlink_error.tmux.stdout, unlink_error.rmux.stdout);
    assert_eq!(unlink_error.tmux.stderr, unlink_error.rmux.stderr);

    let start_beta = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "beta"],
        config.clone(),
    )?;
    assert_eq!(start_beta.tmux.status_code, Some(0));
    assert_eq!(start_beta.rmux.status_code, Some(0));
    assert_eq!(start_beta.tmux.stdout, start_beta.rmux.stdout);
    assert_eq!(start_beta.tmux.stderr, start_beta.rmux.stderr);

    let link = harness.run_pair_with(
        &tmux_binary,
        &["link-window", "-s", "alpha:0", "-t", "beta:1"],
        config.clone(),
    )?;
    assert_eq!(link.tmux.status_code, link.rmux.status_code);
    assert_eq!(link.tmux.timed_out, link.rmux.timed_out);
    assert_eq!(link.tmux.stdout, link.rmux.stdout);
    assert_eq!(link.tmux.stderr, link.rmux.stderr);

    let unlink = harness.run_pair_with(
        &tmux_binary,
        &["unlink-window", "-t", "beta:1"],
        config.clone(),
    )?;
    assert_eq!(unlink.tmux.status_code, unlink.rmux.status_code);
    assert_eq!(unlink.tmux.timed_out, unlink.rmux.timed_out);
    assert_eq!(unlink.tmux.stdout, unlink.rmux.stdout);
    assert_eq!(unlink.tmux.stderr, unlink.rmux.stderr);

    let start_gamma = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "gamma"],
        config.clone(),
    )?;
    assert_eq!(start_gamma.tmux.status_code, Some(0));
    assert_eq!(start_gamma.rmux.status_code, Some(0));
    assert_eq!(start_gamma.tmux.stdout, start_gamma.rmux.stdout);
    assert_eq!(start_gamma.tmux.stderr, start_gamma.rmux.stderr);

    let alias_link = harness.run_pair_with(
        &tmux_binary,
        &["link", "-s", "alpha:0", "-t", "gamma:1"],
        config.clone(),
    )?;
    assert_eq!(alias_link.tmux.status_code, alias_link.rmux.status_code);
    assert_eq!(alias_link.tmux.timed_out, alias_link.rmux.timed_out);
    assert_eq!(alias_link.tmux.stdout, alias_link.rmux.stdout);
    assert_eq!(alias_link.tmux.stderr, alias_link.rmux.stderr);

    let start_delta = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "delta"],
        config.clone(),
    )?;
    assert_eq!(start_delta.tmux.status_code, Some(0));
    assert_eq!(start_delta.rmux.status_code, Some(0));
    assert_eq!(start_delta.tmux.stdout, start_delta.rmux.stdout);
    assert_eq!(start_delta.tmux.stderr, start_delta.rmux.stderr);

    let alias_linkw = harness.run_pair_with(
        &tmux_binary,
        &["linkw", "-s", "alpha:0", "-t", "delta:1"],
        config.clone(),
    )?;
    assert_eq!(alias_linkw.tmux.status_code, alias_linkw.rmux.status_code);
    assert_eq!(alias_linkw.tmux.timed_out, alias_linkw.rmux.timed_out);
    assert_eq!(alias_linkw.tmux.stdout, alias_linkw.rmux.stdout);
    assert_eq!(alias_linkw.tmux.stderr, alias_linkw.rmux.stderr);

    let alias_unlink =
        harness.run_pair_with(&tmux_binary, &["unlinkw", "-t", "gamma:1"], config.clone())?;
    assert_eq!(alias_unlink.tmux.status_code, alias_unlink.rmux.status_code);
    assert_eq!(alias_unlink.tmux.timed_out, alias_unlink.rmux.timed_out);
    assert_eq!(alias_unlink.tmux.stdout, alias_unlink.rmux.stdout);
    assert_eq!(alias_unlink.tmux.stderr, alias_unlink.rmux.stderr);

    let alias_unlinkw =
        harness.run_pair_with(&tmux_binary, &["unlinkw", "-t", "delta:1"], config.clone())?;
    assert_eq!(
        alias_unlinkw.tmux.status_code,
        alias_unlinkw.rmux.status_code
    );
    assert_eq!(alias_unlinkw.tmux.timed_out, alias_unlinkw.rmux.timed_out);
    assert_eq!(alias_unlinkw.tmux.stdout, alias_unlinkw.rmux.stdout);
    assert_eq!(alias_unlinkw.tmux.stderr, alias_unlinkw.rmux.stderr);

    let kill = harness.run_pair_with(
        &tmux_binary,
        &["kill-server"],
        TmuxCompatRunConfig::default()
            .with_timeout(Duration::from_secs(3))
            .with_term("xterm-256color")
            .without_tmux(),
    )?;
    assert_eq!(kill.tmux.status_code, kill.rmux.status_code);
    assert_eq!(kill.tmux.timed_out, kill.rmux.timed_out);
    assert_eq!(kill.tmux.stdout, kill.rmux.stdout);
    assert_eq!(kill.tmux.stderr, kill.rmux.stderr);
    wait_for_socket_absent(harness.tmux_socket_path(), Duration::from_secs(3))?;
    wait_for_socket_absent(harness.rmux_socket_path(), Duration::from_secs(3))?;
    let _ = fs::remove_file(harness.tmux_socket_path());
    let _ = fs::remove_file(harness.rmux_socket_path());
    harness.wait_for_socket_dirs_clean(Duration::from_secs(3))?;
    Ok(())
}

fn write_fake_tmux(path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    common::executable_fixture::write_executable(
        path,
        "#!/bin/sh\nprintf 'TMPDIR=%s\\n' \"$TMPDIR\"\nprintf 'RMUX_TMPDIR=%s\\n' \"$RMUX_TMPDIR\"\nprintf 'TMUX_TMPDIR=%s\\n' \"$TMUX_TMPDIR\"\nprintf 'TMUX=%s\\n' \"${TMUX-unset}\"\nprintf 'TERM=%s\\n' \"$TERM\"\nprintf 'EXTRA=%s\\n' \"$RMUX_TEST_EXTRA\"\nprintf 'ARGV='\nfor arg in \"$@\"; do printf '[%s]' \"$arg\"; done\nprintf '\\n'\n",
        0o755,
    )?;

    Ok(())
}

fn tmux_compat_config_with_clean_homes(
    harness: &TmuxCompatHarness,
) -> Result<TmuxCompatRunConfig, Box<dyn Error>> {
    let home = harness.tmpdir().join("home");
    let xdg = harness.tmpdir().join("xdg");
    fs::create_dir_all(&home)?;
    fs::create_dir_all(&xdg)?;

    Ok(TmuxCompatRunConfig::default()
        .with_timeout(Duration::from_secs(3))
        .with_term("xterm-256color")
        .without_tmux())
}

fn wait_for_socket_absent(socket_path: &Path, timeout: Duration) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        if matches!(connect_or_absent(socket_path)?, ConnectResult::Absent) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for inactive socket '{}'",
                socket_path.display()
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn parse_window_id(output: &str) -> Result<u32, Box<dyn Error>> {
    output
        .trim()
        .strip_prefix('@')
        .ok_or_else(|| {
            std::io::Error::other(format!("expected tmux window id, got {:?}", output)).into()
        })
        .and_then(|value| value.parse::<u32>().map_err(Into::into))
}
