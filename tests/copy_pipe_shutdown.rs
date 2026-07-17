#![cfg(target_os = "linux")]

mod common;

use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use common::{assert_success, stderr, stdout, CliHarness};

const WAIT_TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn kill_server_terminates_long_copy_pipe_helper_and_allows_restart() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("copy-pipe-shutdown")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let daemon_pid = daemon.pid();

    let normal_needle = "needle-copy-pipe-normal";
    let normal_target = prepare_copy_mode_line_selection(&harness, "copy-normal", normal_needle)?;
    let normal_output_path = harness.tmpdir().join("normal-copy-pipe-output.txt");
    let normal_helper_path = harness.tmpdir().join("normal-copy-pipe-helper.sh");
    write_normal_helper(&normal_helper_path, &normal_output_path)?;
    let normal_helper_command = sh_quote_path(&normal_helper_path);

    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        &normal_target,
        "-X",
        "copy-pipe-and-cancel",
        &normal_helper_command,
    ])?);
    let copied = fs::read_to_string(&normal_output_path)?;
    assert!(
        copied.contains(normal_needle),
        "normal copy-pipe helper did not receive the selected line: {copied:?}"
    );

    let blocked_needle = "needle-copy-pipe-blocked";
    let blocked_target =
        prepare_copy_mode_line_selection(&harness, "copy-blocked", blocked_needle)?;
    let resistant = write_resistant_helper(harness.tmpdir())?;
    let resistant_command = sh_quote_path(&resistant.script);
    let mut copy_client = harness.base_command();
    copy_client
        .arg("send-keys")
        .arg("-t")
        .arg(&blocked_target)
        .arg("-X")
        .arg("copy-pipe-and-cancel")
        .arg(&resistant_command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let copy_client = copy_client.spawn()?;

    wait_for_path(&resistant.ready, WAIT_TIMEOUT)?;
    let helper_pid = read_pid(&resistant.helper_pid)?;
    let descendant_pid = read_pid(&resistant.descendant_pid)?;
    assert!(
        pid_exists(helper_pid)?,
        "helper pid {helper_pid} exited too early"
    );
    assert!(
        pid_exists(descendant_pid)?,
        "helper descendant pid {descendant_pid} exited too early"
    );

    assert_success(&harness.run(&["kill-server"])?);
    let status = wait_for_child_exit(daemon.child_mut(), WAIT_TIMEOUT)?;
    assert!(status.success(), "daemon exited with {status}");
    assert!(
        !pid_exists(daemon_pid)?,
        "old daemon pid {daemon_pid} still exists after kill-server"
    );
    wait_for_pid_gone(helper_pid, WAIT_TIMEOUT)?;
    wait_for_pid_gone(descendant_pid, WAIT_TIMEOUT)?;
    let _ = wait_for_output(copy_client, WAIT_TIMEOUT)?;

    drop(daemon);

    let _restarted = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-s",
        "copy-pipe-restarted",
        "sleep 30",
    ])?);
    assert_success(&harness.run(&["has-session", "-t", "copy-pipe-restarted"])?);

    Ok(())
}

struct ResistantHelper {
    script: PathBuf,
    helper_pid: PathBuf,
    descendant_pid: PathBuf,
    ready: PathBuf,
}

fn prepare_copy_mode_line_selection(
    harness: &CliHarness,
    session: &str,
    needle: &str,
) -> Result<String, Box<dyn Error>> {
    let target = format!("{session}:0.0");
    let command = format!("printf 'alpha\\n{needle}\\nomega\\n'; sleep 60");
    assert_success(&harness.run(&["new-session", "-d", "-s", session, &command])?);
    wait_for_capture_contains(harness, &target, needle)?;
    assert_success(&harness.run(&["copy-mode", "-t", &target])?);
    assert_success(&harness.run(&["send-keys", "-t", &target, "-X", "search-backward", needle])?);
    assert_success(&harness.run(&["send-keys", "-t", &target, "-X", "select-line"])?);
    Ok(target)
}

fn wait_for_capture_contains(
    harness: &CliHarness,
    target: &str,
    needle: &str,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    let mut last = String::new();

    loop {
        if Instant::now() >= deadline {
            return Err(format!(
                "capture-pane -p -t {target} never contained {needle:?}; last output: {last:?}"
            )
            .into());
        }

        let output = harness.run(&["capture-pane", "-p", "-t", target])?;
        if output.status.success() {
            last = stdout(&output);
            if last.contains(needle) {
                return Ok(());
            }
        } else {
            last = stderr(&output);
        }

        std::thread::sleep(Duration::from_millis(20));
    }
}

fn write_normal_helper(script: &Path, output: &Path) -> Result<(), Box<dyn Error>> {
    write_executable_script(
        script,
        &format!("#!/bin/sh\ncat > {}\n", sh_quote_path(output)),
    )
}

fn write_resistant_helper(root: &Path) -> Result<ResistantHelper, Box<dyn Error>> {
    let script = root.join("resistant-copy-pipe-helper.sh");
    let helper_pid = root.join("resistant-helper.pid");
    let descendant_pid = root.join("resistant-helper-descendant.pid");
    let ready = root.join("resistant-helper.ready");
    write_executable_script(
        &script,
        &format!(
            "#!/bin/sh\n\
             trap '' TERM HUP INT\n\
             (\n\
             trap '' TERM HUP INT\n\
             while :; do\n\
             sleep 1\n\
             done\n\
             ) &\n\
             printf '%s\\n' \"$$\" > {}\n\
             printf '%s\\n' \"$!\" > {}\n\
             touch {}\n\
             cat >/dev/null\n\
             wait\n",
            sh_quote_path(&helper_pid),
            sh_quote_path(&descendant_pid),
            sh_quote_path(&ready),
        ),
    )?;
    Ok(ResistantHelper {
        script,
        helper_pid,
        descendant_pid,
        ready,
    })
}

fn write_executable_script(path: &Path, contents: &str) -> Result<(), Box<dyn Error>> {
    fs::write(path, contents)?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn wait_for_path(path: &Path, timeout: Duration) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    while !path.exists() {
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for {}", path.display()).into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn read_pid(path: &Path) -> Result<u32, Box<dyn Error>> {
    Ok(fs::read_to_string(path)?.trim().parse()?)
}

fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> Result<ExitStatus, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for child {}", child.id()).into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_output(mut child: Child, timeout: Duration) -> Result<Output, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        if Instant::now() >= deadline {
            let pid = child.id();
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("timed out waiting for client {pid}").into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_pid_gone(pid: u32, timeout: Duration) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    while pid_exists(pid)? {
        if Instant::now() >= deadline {
            return Err(format!("pid {pid} still exists").into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn pid_exists(pid: u32) -> Result<bool, Box<dyn Error>> {
    match fs::metadata(format!("/proc/{pid}")) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn sh_quote_path(path: &Path) -> String {
    sh_quote(&path.to_string_lossy())
}

fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
