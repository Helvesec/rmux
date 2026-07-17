use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::RequestHandler;
use rmux_proto::{
    DisplayMessageRequest, NewSessionRequest, PaneTarget, PipePaneRequest, Request, Response,
    SendKeysRequest, SessionName, Target, TerminalSize,
};
#[cfg(unix)]
use rustix::process::{kill_process, test_kill_process, Pid, Signal};
use tokio::time::sleep;

const PANE_PIPE_TEST_TIMEOUT: Duration = Duration::from_secs(15);

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn unique_temp_path(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "rmux-pane-pipe-{label}-{}-{unique}",
        std::process::id()
    ))
}

#[cfg(unix)]
fn pipe_to_file_command(path: &Path) -> String {
    format!("cat > {}", crate::test_shell::sh_quote_path(path))
}

#[cfg(windows)]
fn pipe_to_file_command(path: &Path) -> String {
    crate::test_shell::powershell_encoded_command(&format!(
        "$out=[System.IO.File]::Open({}, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write, [System.IO.FileShare]::ReadWrite); try {{ $buf=New-Object byte[] 4096; $inputStream=[Console]::OpenStandardInput(); while (($n=$inputStream.Read($buf,0,$buf.Length)) -gt 0) {{ $out.Write($buf,0,$n); $out.Flush() }} }} finally {{ $out.Dispose() }}",
        crate::test_shell::powershell_quote_path(path)
    ))
}

fn pipe_discard_command() -> String {
    crate::test_shell::stdin_discard_command()
}

#[cfg(unix)]
fn pane_print_command(text: &str) -> String {
    format!("printf '{}\\n'", text.replace('\'', r"'\''"))
}

#[cfg(windows)]
fn pane_print_command(text: &str) -> String {
    format!("echo {text}")
}

async fn create_session(handler: &RequestHandler, name: &str) {
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name(name),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)));
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::new(session_name(name), 0))
        .await;
}

async fn display_pane_format(
    handler: &RequestHandler,
    target: PaneTarget,
    message: &str,
) -> String {
    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(target)),
            print: true,
            message: Some(message.to_owned()),
            empty_target_context: false,
        }))
        .await;
    let Response::DisplayMessage(response) = response else {
        panic!("expected display-message response");
    };
    let output = response
        .command_output()
        .expect("display-message -p returns output");
    String::from_utf8_lossy(output.stdout())
        .trim_end()
        .to_owned()
}

async fn wait_for_pane_process(handler: &RequestHandler, target: PaneTarget) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let last = display_pane_format(handler, target.clone(), "#{pane_current_command}").await;
        if !last.is_empty() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for pane process; last command={last:?}"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

async fn pipe_pane(
    handler: &RequestHandler,
    target: PaneTarget,
    once: bool,
    command: Option<String>,
) {
    let response = handler
        .handle(Request::PipePane(PipePaneRequest {
            target,
            stdin: false,
            stdout: true,
            once,
            command,
        }))
        .await;
    assert!(matches!(response, Response::PipePane(_)));
}

async fn wait_for_file_contains(path: &Path, expected: &str) {
    let deadline = tokio::time::Instant::now() + PANE_PIPE_TEST_TIMEOUT;
    loop {
        match fs::read_to_string(path) {
            Ok(contents) if contents.contains(expected) => return,
            Ok(_) | Err(_) if tokio::time::Instant::now() < deadline => {
                sleep(Duration::from_millis(25)).await;
            }
            Ok(contents) => panic!(
                "timed out waiting for {} to contain {:?}, got {:?}",
                path.display(),
                expected,
                contents
            ),
            Err(error) => panic!(
                "timed out waiting for {} to exist containing {:?}: {error}",
                path.display(),
                expected
            ),
        }
    }
}

async fn wait_for_pipe_child_count_to_return_to(baseline: usize) {
    let deadline = tokio::time::Instant::now() + PANE_PIPE_TEST_TIMEOUT;
    loop {
        let active = crate::pane_terminals::active_pipe_child_count_for_test();
        if active <= baseline {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for pipe-pane children to stop; baseline={baseline}, active={active}"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(unix)]
async fn wait_for_pipe_descendant_pid(path: &Path) -> Pid {
    let deadline = tokio::time::Instant::now() + PANE_PIPE_TEST_TIMEOUT;
    loop {
        if let Ok(contents) = fs::read_to_string(path) {
            if let Ok(raw_pid) = contents.trim().parse::<i32>() {
                if let Some(pid) = Pid::from_raw(raw_pid) {
                    return pid;
                }
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for pipe-pane descendant pid"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(windows)]
async fn wait_for_windows_pipe_descendant_pid(path: &Path) -> u32 {
    let deadline = tokio::time::Instant::now() + PANE_PIPE_TEST_TIMEOUT;
    loop {
        if let Ok(contents) = fs::read_to_string(path) {
            if let Ok(pid) = contents.trim().parse::<u32>() {
                return pid;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for Windows pipe-pane descendant pid"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(unix)]
async fn wait_for_process_to_exit(pid: Pid) {
    let deadline = tokio::time::Instant::now() + PANE_PIPE_TEST_TIMEOUT;
    loop {
        match test_kill_process(pid) {
            Err(error) if error == rustix::io::Errno::SRCH => return,
            Err(error) => panic!("failed to query pipe-pane descendant {pid:?}: {error}"),
            Ok(()) => {}
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "pipe-pane descendant {pid:?} survived pipe shutdown"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(windows)]
async fn wait_for_windows_process_to_exit(pid: u32) {
    let deadline = tokio::time::Instant::now() + PANE_PIPE_TEST_TIMEOUT;
    while rmux_os::process::is_live(pid) && tokio::time::Instant::now() < deadline {
        sleep(Duration::from_millis(25)).await;
    }
    assert!(
        !rmux_os::process::is_live(pid),
        "Windows pipe-pane descendant {pid} survived pipe shutdown"
    );
}

#[cfg(unix)]
struct PipeDescendantCleanup(Option<Pid>);

#[cfg(unix)]
impl Drop for PipeDescendantCleanup {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            let _ = kill_process(pid, Signal::KILL);
        }
    }
}

#[cfg(windows)]
struct WindowsPipeDescendantCleanup(Option<u32>);

#[cfg(windows)]
impl Drop for WindowsPipeDescendantCleanup {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            let _ = std::process::Command::new("taskkill.exe")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .output();
        }
    }
}

#[tokio::test]
async fn pipe_pane_once_closes_existing_pipe_without_reopening() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    let first_output = unique_temp_path("once-first");
    let second_output = unique_temp_path("once-second");
    let pipe_child_baseline = crate::pane_terminals::active_pipe_child_count_for_test();
    create_session(&handler, "alpha").await;
    wait_for_pane_process(&handler, target.clone()).await;

    pipe_pane(
        &handler,
        target.clone(),
        false,
        Some(pipe_to_file_command(&first_output)),
    )
    .await;
    let sent = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: target.clone(),
            keys: vec![pane_print_command("pipe-one"), "Enter".to_owned()],
        }))
        .await;
    assert!(matches!(sent, Response::SendKeys(_)));
    wait_for_file_contains(&first_output, "pipe-one").await;

    pipe_pane(
        &handler,
        target.clone(),
        true,
        Some(pipe_to_file_command(&second_output)),
    )
    .await;
    let sent = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: target.clone(),
            keys: vec![pane_print_command("pipe-two"), "Enter".to_owned()],
        }))
        .await;
    assert!(matches!(sent, Response::SendKeys(_)));
    wait_for_pipe_child_count_to_return_to(pipe_child_baseline).await;

    let first_contents = fs::read_to_string(&first_output).expect("first pipe output exists");
    assert!(first_contents.contains("pipe-one"));
    assert!(!first_contents.contains("pipe-two"));
    assert!(!second_output.exists() || fs::read_to_string(&second_output).unwrap().is_empty());

    let _ = fs::remove_file(first_output);
    let _ = fs::remove_file(second_output);
}

#[cfg(unix)]
#[tokio::test]
async fn stopping_pipe_pane_terminates_descendants_after_the_shell_exits() {
    let handler = RequestHandler::new();
    let target = PaneTarget::with_window(session_name("pipe-process-group"), 0, 0);
    let pid_file = unique_temp_path("descendant-pid");
    let pipe_child_baseline = crate::pane_terminals::active_pipe_child_count_for_test();
    create_session(&handler, "pipe-process-group").await;
    let command = format!(
        "sleep 60 & child=$!; printf '%s\\n' \"$child\" > {}",
        crate::test_shell::sh_quote_path(&pid_file)
    );

    let response = handler
        .handle(Request::PipePane(PipePaneRequest {
            target: target.clone(),
            stdin: true,
            stdout: false,
            once: false,
            command: Some(command),
        }))
        .await;
    assert!(matches!(response, Response::PipePane(_)));
    let descendant = wait_for_pipe_descendant_pid(&pid_file).await;
    let mut cleanup = PipeDescendantCleanup(Some(descendant));
    wait_for_pipe_child_count_to_return_to(pipe_child_baseline).await;

    pipe_pane(&handler, target, false, None).await;
    wait_for_process_to_exit(descendant).await;
    cleanup.0 = None;
    let _ = fs::remove_file(pid_file);
}

#[cfg(windows)]
#[tokio::test]
async fn stopping_pipe_pane_terminates_fast_descendants_after_the_shell_exits() {
    let handler = RequestHandler::new();
    let target = PaneTarget::with_window(session_name("pipe-windows-job"), 0, 0);
    let pid_file = unique_temp_path("windows-descendant-pid");
    let pipe_child_baseline = crate::pane_terminals::active_pipe_child_count_for_test();
    create_session(&handler, "pipe-windows-job").await;
    let command = crate::test_shell::powershell_encoded_command(&format!(
        "$child=Start-Process -FilePath ($PSHOME + '\\powershell.exe') -ArgumentList '-NoLogo','-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 60' -WindowStyle Hidden -PassThru; [System.IO.File]::WriteAllText({}, [string]$child.Id)",
        crate::test_shell::powershell_quote_path(&pid_file)
    ));

    let response = handler
        .handle(Request::PipePane(PipePaneRequest {
            target: target.clone(),
            stdin: true,
            stdout: false,
            once: false,
            command: Some(command),
        }))
        .await;
    assert!(matches!(response, Response::PipePane(_)));
    let descendant = wait_for_windows_pipe_descendant_pid(&pid_file).await;
    let mut cleanup = WindowsPipeDescendantCleanup(Some(descendant));
    wait_for_pipe_child_count_to_return_to(pipe_child_baseline).await;

    pipe_pane(&handler, target, false, None).await;
    wait_for_windows_process_to_exit(descendant).await;
    cleanup.0 = None;
    let _ = fs::remove_file(pid_file);
}

#[cfg(unix)]
#[tokio::test]
async fn late_pipe_stop_does_not_reuse_a_disarmed_process_group_id() {
    let handler = RequestHandler::new();
    let target = PaneTarget::with_window(session_name("pipe-late-stop"), 0, 0);
    create_session(&handler, "pipe-late-stop").await;

    let response = handler
        .handle(Request::PipePane(PipePaneRequest {
            target: target.clone(),
            stdin: true,
            stdout: false,
            once: false,
            command: Some("printf done".to_owned()),
        }))
        .await;
    assert!(matches!(response, Response::PipePane(_)));
    let probe = handler
        .state
        .lock()
        .await
        .pane_pipe_process_group_probe_for_test(&target)
        .expect("active pipe process group probe");

    let deadline = tokio::time::Instant::now() + PANE_PIPE_TEST_TIMEOUT;
    while probe.is_armed() || probe.termination_count() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "completed pipe process group stayed armed"
        );
        sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(probe.termination_count(), 1);

    pipe_pane(&handler, target, false, None).await;
    assert_eq!(
        probe.termination_count(),
        1,
        "a late stop must not signal a recycled numeric process group"
    );
}

#[tokio::test]
async fn pane_pipe_format_reports_active_pipe_state() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    let pipe_child_baseline = crate::pane_terminals::active_pipe_child_count_for_test();
    create_session(&handler, "alpha").await;

    assert_eq!(
        display_pane_format(&handler, target.clone(), "#{pane_pipe}").await,
        "0"
    );
    pipe_pane(
        &handler,
        target.clone(),
        false,
        Some(pipe_discard_command()),
    )
    .await;
    assert_eq!(
        display_pane_format(&handler, target.clone(), "#{pane_pipe}").await,
        "1"
    );
    pipe_pane(&handler, target.clone(), false, None).await;
    assert_eq!(
        display_pane_format(&handler, target, "#{pane_pipe}").await,
        "0"
    );
    wait_for_pipe_child_count_to_return_to(pipe_child_baseline).await;
}
