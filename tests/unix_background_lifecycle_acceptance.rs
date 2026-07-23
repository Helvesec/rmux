#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use common::{assert_success, stderr, stdout, CliHarness, DaemonGuard};

const BACKGROUND_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const SLOW_HOOK_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(7);

#[test]
fn background_run_shell_command_survives_originating_client_exit() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("bg-run-shell-cli-lifecycle")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "run-shell",
        "-b",
        "-d",
        "0.15",
        "-C",
        "set-buffer -b bg-run-shell-cli ok",
    ])?);

    wait_for_buffer(&harness, "bg-run-shell-cli", "ok")?;
    Ok(())
}

#[test]
fn background_run_shell_shell_job_survives_originating_client_exit() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("bg-run-shell-shell-lifecycle")?;
    let _daemon = harness.start_hidden_daemon()?;
    let marker = harness.tmpdir().join("run-shell-shell-survived.txt");
    let command = format!("sleep 0.15; printf ok > {}", shell_quote(&marker));

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["run-shell", "-b", &command])?);

    wait_for_file_contents(&marker, "ok")?;
    Ok(())
}

#[test]
fn background_if_shell_command_survives_originating_client_exit() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("bg-if-shell-cli-lifecycle")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "if-shell",
        "-b",
        "sleep 0.15; true",
        "set-buffer -b bg-if-shell-cli ok",
    ])?);

    wait_for_buffer(&harness, "bg-if-shell-cli", "ok")?;
    Ok(())
}

#[test]
fn source_file_background_if_shell_keeps_write_access_after_client_exit(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("bg-source-if-shell-cli-lifecycle")?;
    let _daemon = harness.start_hidden_daemon()?;
    let source_path = harness.tmpdir().join("source-if-shell.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    fs::write(
        &source_path,
        "if-shell -b 'sleep 0.15; true' 'set-buffer -b bg-source-if-shell-cli ok'\n",
    )?;
    assert_success(&harness.run(&[
        "source-file",
        source_path.to_str().ok_or("source path is not UTF-8")?,
    ])?);

    wait_for_buffer(&harness, "bg-source-if-shell-cli", "ok")?;
    Ok(())
}

#[test]
fn kill_server_terminates_direct_background_run_shell_tree_product_divergence(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("bg-run-shell-kill-server")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let probe = ShellShutdownProbe::new(&harness, "direct-run-shell");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["run-shell", "-b", &probe.command()])?);
    probe.wait_until_started()?;

    assert_success(&harness.run(&["kill-server"])?);
    wait_for_daemon_exit(&mut daemon)?;

    probe.assert_terminated()?;
    Ok(())
}

#[test]
fn kill_server_terminates_queued_background_run_shell_tree_product_divergence(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("bg-queued-run-shell-kill-server")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let probe = ShellShutdownProbe::new(&harness, "queued-run-shell");
    let source_path = harness.tmpdir().join("queued-run-shell.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    fs::write(
        &source_path,
        format!("run-shell -b {}\n", shell_quote_str(&probe.command())),
    )?;
    assert_success(&harness.run(&[
        "source-file",
        source_path.to_str().ok_or("source path is not UTF-8")?,
    ])?);
    probe.wait_until_started()?;

    assert_success(&harness.run(&["kill-server"])?);
    wait_for_daemon_exit(&mut daemon)?;

    probe.assert_terminated()?;
    Ok(())
}

#[test]
fn kill_server_terminates_background_if_shell_condition_tree() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("bg-if-shell-kill-server")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let probe = ShellShutdownProbe::new(&harness, "if-shell");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "if-shell",
        "-b",
        &probe.command(),
        "set-buffer -b bg-if-shell-kill-server unexpected",
    ])?);
    probe.wait_until_started()?;

    assert_success(&harness.run(&["kill-server"])?);
    wait_for_daemon_exit(&mut daemon)?;

    probe.assert_terminated()?;
    Ok(())
}

#[test]
fn kill_server_bounds_slow_session_closed_hook_tree_product_divergence(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("session-closed-hook-kill-server")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let probe = ShellShutdownProbe::new(&harness, "session-closed-hook");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-g",
        "session-closed",
        &format!("run-shell {}", shell_quote_str(&probe.command())),
    ])?);

    let shutdown_started = Instant::now();
    assert_success(&harness.run(&["kill-server"])?);
    assert!(
        shutdown_started.elapsed() >= Duration::from_secs(4),
        "kill-server returned before the bounded lifecycle hook was cancelled"
    );
    probe.wait_until_started()?;
    wait_for_daemon_exit_with_timeout(&mut daemon, SLOW_HOOK_SHUTDOWN_TIMEOUT)?;

    probe.assert_terminated()?;
    Ok(())
}

#[test]
fn kill_server_joins_background_session_closed_hook_trees_product_divergence(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("session-closed-background-hook-kill-server")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let probe = HookShutdownBurst::new(&harness)?;
    let accepted_path = harness.tmpdir().join("background-hooks-accepted.log");

    for index in 0..32 {
        assert_success(&harness.run(&[
            "new-session",
            "-d",
            "-s",
            &format!("closing-{index:02}"),
        ])?);
    }
    assert_success(&harness.run(&["set-hook", "-g", "session-closed", &probe.hook_command(0)])?);
    assert_success(&harness.run(&["set-hook", "-ag", "session-closed", &probe.hook_command(1)])?);
    let accepted_command = format!("printf 'accepted\\n' >> {}", shell_quote(&accepted_path));
    assert_success(&harness.run(&[
        "set-hook",
        "-ag",
        "session-closed",
        &format!("run-shell {}", shell_quote_str(&accepted_command)),
    ])?);

    assert_success(&harness.run(&["kill-server"])?);
    wait_for_daemon_exit(&mut daemon)?;

    assert_eq!(
        fs::read_to_string(&accepted_path)?.lines().count(),
        32,
        "every accepted session-closed hook must drain before shutdown"
    );
    probe.assert_started_and_terminated()?;
    Ok(())
}

fn wait_for_buffer(
    harness: &CliHarness,
    buffer_name: &str,
    expected: &str,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + BACKGROUND_TIMEOUT;
    let mut last_output: Option<Output> = None;

    while Instant::now() < deadline {
        let output = harness.run(&["show-buffer", "-b", buffer_name])?;
        if output.status.success() && stdout(&output) == expected && stderr(&output).is_empty() {
            return Ok(());
        }
        last_output = Some(output);
        thread::sleep(Duration::from_millis(25));
    }

    let diagnostic = last_output
        .map(|output| {
            format!(
                "status={:?}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                stdout(&output),
                stderr(&output)
            )
        })
        .unwrap_or_else(|| "show-buffer was never attempted".to_owned());
    Err(format!(
        "timed out waiting for buffer {buffer_name:?} to become {expected:?}; last output:\n{diagnostic}"
    )
    .into())
}

fn wait_for_file_contents(path: &Path, expected: &str) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + BACKGROUND_TIMEOUT;
    let mut last_contents = None;

    while Instant::now() < deadline {
        match fs::read_to_string(path) {
            Ok(contents) if contents == expected => return Ok(()),
            Ok(contents) => last_contents = Some(contents),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        thread::sleep(Duration::from_millis(25));
    }

    Err(format!(
        "timed out waiting for '{}' to contain {expected:?}; last contents: {last_contents:?}",
        path.display()
    )
    .into())
}

fn wait_for_path(path: &Path) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + BACKGROUND_TIMEOUT;
    while Instant::now() < deadline {
        if path.exists() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(format!("timed out waiting for '{}'", path.display()).into())
}

fn wait_for_daemon_exit(daemon: &mut DaemonGuard) -> Result<(), Box<dyn Error>> {
    wait_for_daemon_exit_with_timeout(daemon, SHUTDOWN_TIMEOUT)
}

fn wait_for_daemon_exit_with_timeout(
    daemon: &mut DaemonGuard,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        if daemon.child_mut().try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("timed out waiting for daemon process to exit after kill-server".into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_pid_exit(pid: u32) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
    while Instant::now() < deadline {
        if !rmux_os::process::is_live(pid) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(format!("process {pid} remained live after kill-server").into())
}

struct ShellShutdownProbe {
    parent_pid_path: PathBuf,
    child_pid_path: PathBuf,
    ready_path: PathBuf,
    survived_path: PathBuf,
}

impl ShellShutdownProbe {
    fn new(harness: &CliHarness, label: &str) -> Self {
        let root = harness.tmpdir().join(label);
        Self {
            parent_pid_path: root.with_extension("parent.pid"),
            child_pid_path: root.with_extension("child.pid"),
            ready_path: root.with_extension("ready"),
            survived_path: root.with_extension("survived"),
        }
    }

    fn command(&self) -> String {
        format!(
            "printf '%s\\n' \"$$\" > {}; \
             (trap '' TERM; touch {}; sleep 30) & \
             child=$!; printf '%s\\n' \"$child\" > {}; \
             wait \"$child\"; printf survived > {}",
            shell_quote(&self.parent_pid_path),
            shell_quote(&self.ready_path),
            shell_quote(&self.child_pid_path),
            shell_quote(&self.survived_path),
        )
    }

    fn wait_until_started(&self) -> Result<(), Box<dyn Error>> {
        wait_for_path(&self.ready_path)?;
        let _ = read_pid(&self.parent_pid_path)?;
        let _ = read_pid(&self.child_pid_path)?;
        Ok(())
    }

    fn assert_terminated(&self) -> Result<(), Box<dyn Error>> {
        let parent_pid = read_pid(&self.parent_pid_path)?;
        let child_pid = read_pid(&self.child_pid_path)?;
        wait_for_pid_exit(parent_pid)?;
        wait_for_pid_exit(child_pid)?;
        thread::sleep(Duration::from_millis(100));
        assert!(
            !self.survived_path.exists(),
            "background shell continued after kill-server and wrote '{}'",
            self.survived_path.display()
        );
        Ok(())
    }
}

impl Drop for ShellShutdownProbe {
    fn drop(&mut self) {
        kill_pid_file(&self.child_pid_path);
        kill_pid_file(&self.parent_pid_path);
    }
}

struct HookShutdownBurst {
    scripts: Vec<PathBuf>,
    state_dirs: Vec<PathBuf>,
}

impl HookShutdownBurst {
    fn new(harness: &CliHarness) -> Result<Self, Box<dyn Error>> {
        let mut scripts = Vec::new();
        let mut state_dirs = Vec::new();
        for (label, chatter) in [("silent", false), ("chatty", true)] {
            let state_dir = harness.tmpdir().join(format!("hook-{label}-state"));
            fs::create_dir_all(&state_dir)?;
            let script = harness.tmpdir().join(format!("hook-{label}.sh"));
            let child_loop = if chatter {
                "while :; do printf 'hook-noise\\n'; sleep 0.05; done"
            } else {
                "while :; do sleep 1; done"
            };
            let child_redirect = if chatter {
                ""
            } else {
                " </dev/null >/dev/null 2>&1"
            };
            fs::write(
                &script,
                format!(
                    "#!/bin/sh\n\
                     set -eu\n\
                     parent=$$\n\
                     printf '%s\\n' \"$parent\" > {state}/$parent.parent\n\
                     (trap '' HUP TERM PIPE; {child_loop}){child_redirect} &\n\
                     child=$!\n\
                     printf '%s\\n' \"$child\" > {state}/$parent.child\n\
                     trap '' HUP TERM PIPE\n\
                     wait \"$child\"\n",
                    state = shell_quote(&state_dir),
                ),
            )?;
            let mut permissions = fs::metadata(&script)?.permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(&script, permissions)?;
            scripts.push(script);
            state_dirs.push(state_dir);
        }
        Ok(Self {
            scripts,
            state_dirs,
        })
    }

    fn hook_command(&self, index: usize) -> String {
        format!("run-shell -b {}", shell_quote(&self.scripts[index]))
    }

    fn assert_started_and_terminated(&self) -> Result<(), Box<dyn Error>> {
        for state_dir in &self.state_dirs {
            let pid_files = fs::read_dir(state_dir)?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .map(|entry| entry.path())
                .filter(|path| {
                    matches!(
                        path.extension().and_then(|extension| extension.to_str()),
                        Some("parent" | "child")
                    )
                })
                .collect::<Vec<_>>();
            let child_count = pid_files
                .iter()
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "child")
                })
                .count();
            assert!(
                child_count > 0,
                "no background hook descendant started for '{}'",
                state_dir.display()
            );
            for pid_file in pid_files {
                wait_for_pid_exit(read_pid(&pid_file)?)?;
            }
        }
        Ok(())
    }
}

impl Drop for HookShutdownBurst {
    fn drop(&mut self) {
        for state_dir in &self.state_dirs {
            let Ok(entries) = fs::read_dir(state_dir) else {
                continue;
            };
            for entry in entries.flatten() {
                kill_pid_file(&entry.path());
            }
        }
    }
}

fn read_pid(path: &Path) -> Result<u32, Box<dyn Error>> {
    Ok(fs::read_to_string(path)?
        .trim()
        .parse()
        .map_err(|error| format!("invalid pid in '{}': {error}", path.display()))?)
}

fn kill_pid_file(path: &Path) {
    let Ok(pid) = read_pid(path) else {
        return;
    };
    if !rmux_os::process::is_live(pid) {
        return;
    }
    let _ = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status();
    let _ = Command::new("kill")
        .arg("-KILL")
        .arg(pid.to_string())
        .status();
}

fn shell_quote(path: &Path) -> String {
    shell_quote_str(&path.display().to_string())
}

fn shell_quote_str(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
