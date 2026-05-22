use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::RequestHandler;
use crate::pane_io::AttachControl;
use crate::pane_terminals::PaneLifecycleProcessState;
use rmux_core::LifecycleEvent;
use rmux_proto::{
    BreakPaneRequest, DisplayPanesRequest, KillPaneRequest, ListPanesRequest, ListWindowsRequest,
    MovePaneRequest, NewSessionExtRequest, NewSessionRequest, OptionName, PaneSnapshotRequest,
    PaneTarget, PipePaneRequest, ProcessCommand, RenameWindowRequest, Request, RespawnPaneRequest,
    ScopeSelector, SelectPaneRequest, SendKeysRequest, SessionName, SetOptionMode,
    SetOptionRequest, SplitDirection, SplitWindowExtRequest, SplitWindowRequest, SplitWindowTarget,
    TerminalSize, WindowTarget,
};
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn unique_temp_path(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "rmux-pane-command-{label}-{}-{unique}",
        std::process::id()
    ))
}

#[cfg(unix)]
fn shell_quote(path: &Path) -> String {
    crate::test_shell::sh_quote_path(path)
}

#[cfg(windows)]
fn pipe_to_file_command(path: &Path) -> String {
    crate::test_shell::powershell_encoded_command(&format!(
        "$out=[System.IO.File]::Open({}, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write, [System.IO.FileShare]::ReadWrite); try {{ $buf=New-Object byte[] 4096; $inputStream=[Console]::OpenStandardInput(); while (($n=$inputStream.Read($buf,0,$buf.Length)) -gt 0) {{ $out.Write($buf,0,$n); $out.Flush() }} }} finally {{ $out.Dispose() }}",
        crate::test_shell::powershell_quote_path(path)
    ))
}

#[cfg(unix)]
fn pipe_to_file_command(path: &Path) -> String {
    format!("cat > {}", shell_quote(path))
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

#[cfg(unix)]
fn respawn_probe_command(output: &Path) -> String {
    format!(
        "printf '%s:%s' \"$(pwd)\" \"$RMUX_RESPAWN\" > {}",
        shell_quote(output)
    )
}

#[cfg(windows)]
fn respawn_probe_command(output: &Path) -> String {
    crate::test_shell::powershell_encoded_command(&format!(
        "[System.IO.File]::WriteAllText({}, ((Get-Location).Path + ':' + $env:RMUX_RESPAWN))",
        crate::test_shell::powershell_quote_path(output)
    ))
}

#[cfg(unix)]
fn cwd_probe_command(output: &Path) -> String {
    format!("printf '%s' \"$(pwd)\" > {}", shell_quote(output))
}

#[cfg(windows)]
fn cwd_probe_command(output: &Path) -> String {
    crate::test_shell::powershell_encoded_command(&format!(
        "[System.IO.File]::WriteAllText({}, (Get-Location).Path)",
        crate::test_shell::powershell_quote_path(output)
    ))
}

#[cfg(windows)]
fn expected_spawn_cwd(path: &Path) -> String {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let rendered = canonical.display().to_string();
    if let Some(rest) = rendered.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        rendered
            .strip_prefix(r"\\?\")
            .unwrap_or(&rendered)
            .to_owned()
    }
}

#[cfg(unix)]
fn expected_spawn_cwd(path: &Path) -> String {
    path.display().to_string()
}

async fn create_session(handler: &RequestHandler, session_name: &SessionName) {
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, rmux_proto::Response::NewSession(_)));
}

async fn wait_for_file_contents(path: &Path, expected: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match fs::read_to_string(path) {
            Ok(contents) if contents == expected => return,
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
                "timed out waiting for {} to exist with {:?}: {error}",
                path.display(),
                expected
            ),
        }
    }
}

async fn wait_for_file_contains(path: &Path, expected: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
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

async fn wait_for_dead_pane(
    handler: &RequestHandler,
    session_name: &SessionName,
    window_index: u32,
    pane_index: u32,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let exited = {
            let mut state = handler.state.lock().await;
            state
                .clone_pane_master_if_alive(session_name, window_index, pane_index)
                .is_err()
        };
        if exited {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for pane {session_name}:{window_index}.{pane_index} to exit"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_lifecycle_exit(
    handler: &RequestHandler,
    pane_id: rmux_core::PaneId,
    expected_status: i32,
) -> (u64, u64) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let observed = {
            let state = handler.state.lock().await;
            state.pane_lifecycle(pane_id).and_then(|lifecycle| {
                lifecycle
                    .exit_state
                    .map(|exit| (lifecycle.generation, lifecycle.output_sequence, exit))
            })
        };
        if let Some((generation, output_sequence, exit)) = observed {
            assert_eq!(exit.status, Some(expected_status));
            assert_eq!(exit.signal, None);
            return (generation, output_sequence);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for pane {} lifecycle exit state",
            pane_id.as_u32()
        );
        sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn sticky_lifecycle_state_is_id_keyed_and_redacts_spawn_env() {
    let handler = RequestHandler::new();
    let alpha = session_name("sticky");
    let initial_cwd = unique_temp_path("sticky-initial-cwd");
    let respawn_cwd = unique_temp_path("sticky-respawn-cwd");
    fs::create_dir_all(&initial_cwd).expect("initial cwd");
    fs::create_dir_all(&respawn_cwd).expect("respawn cwd");
    let initial_command = pipe_discard_command();
    let split_command = pipe_discard_command();
    let respawn_command = pipe_discard_command();
    let initial_secret = "RMUX_PRIVATE_INITIAL=alpha-secret".to_owned();
    let split_secret = "RMUX_PRIVATE_SPLIT=beta-secret".to_owned();
    let respawn_secret = "RMUX_PRIVATE_RESPAWN=gamma-secret".to_owned();

    let created = handler
        .handle(Request::NewSessionExt(NewSessionExtRequest {
            session_name: Some(alpha.clone()),
            working_directory: Some(initial_cwd.to_string_lossy().into_owned()),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: Some(vec![initial_secret.clone()]),
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(vec![initial_command.clone()]),
            process_command: None,
        }))
        .await;
    assert!(matches!(created, rmux_proto::Response::NewSession(_)));

    let (session_id, window_id, initial_pane_id, initial_output_sequence) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        let window = session.window_at(0).expect("window exists");
        let pane = window.pane(0).expect("pane exists");
        let lifecycle = state
            .pane_lifecycle(pane.id())
            .expect("initial lifecycle exists");
        assert_eq!(lifecycle.session_id, session.id());
        assert_eq!(lifecycle.window_id, window.id());
        assert_eq!(lifecycle.pane_id, pane.id());
        assert_eq!(
            lifecycle.command(),
            Some(std::slice::from_ref(&initial_command))
        );
        assert_eq!(lifecycle.working_directory(), Some(initial_cwd.as_path()));
        assert_eq!(
            lifecycle.private_environment(),
            std::slice::from_ref(&initial_secret)
        );
        assert!(lifecycle.tags().is_empty());
        assert_eq!(lifecycle.dimensions(), TerminalSize { cols: 80, rows: 24 });
        assert!(matches!(
            lifecycle.process,
            PaneLifecycleProcessState::Running { .. }
        ));
        assert!(lifecycle.generation >= 1);
        assert!(lifecycle.revision >= 1);
        assert!(lifecycle.output_sequence >= 1);
        assert!(lifecycle.exit_state.is_none());
        (
            session.id(),
            window.id(),
            pane.id(),
            lifecycle.output_sequence,
        )
    };

    let split = handler
        .handle(Request::SplitWindowExt(SplitWindowExtRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: Some(vec![split_secret.clone()]),
            command: Some(vec![split_command.clone()]),
            process_command: None,
            start_directory: None,
            keep_alive_on_exit: None,
        }))
        .await;
    let split_target = match split {
        rmux_proto::Response::SplitWindow(response) => response.pane,
        response => panic!("expected split-window success, got {response:?}"),
    };
    let split_pane_id = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        let window = session.window_at(0).expect("window exists");
        let pane = window
            .pane(split_target.pane_index())
            .expect("split pane exists");
        let lifecycle = state
            .pane_lifecycle(pane.id())
            .expect("split lifecycle exists");
        assert_eq!(lifecycle.session_id, session_id);
        assert_eq!(lifecycle.window_id, window_id);
        assert_eq!(
            lifecycle.command(),
            Some(std::slice::from_ref(&split_command))
        );
        assert_eq!(
            lifecycle.private_environment(),
            std::slice::from_ref(&split_secret)
        );
        assert!(lifecycle.dimensions().cols > 0);
        assert!(lifecycle.dimensions().rows > 0);
        assert!(lifecycle.output_sequence >= 1);
        assert!(pane.id().as_u32() > initial_pane_id.as_u32());
        pane.id()
    };

    let list_format = concat!(
        "#{pane_id}\t#{pane_start_command}\t#{pane_start_path}\t",
        "#{pane_lifecycle_generation}\t#{pane_output_sequence}\t",
        "#{RMUX_PRIVATE_INITIAL}\t#{RMUX_PRIVATE_SPLIT}\t#{RMUX_PRIVATE_RESPAWN}"
    )
    .to_owned();
    let listed = handler
        .handle(Request::ListPanes(ListPanesRequest {
            target: alpha.clone(),
            target_window_index: None,
            format: Some(list_format.clone()),
        }))
        .await;
    let list_stdout = match listed {
        rmux_proto::Response::ListPanes(response) => {
            String::from_utf8(response.output.stdout).expect("list-panes utf8")
        }
        response => panic!("expected list-panes success, got {response:?}"),
    };
    assert!(list_stdout.contains(&initial_pane_id.to_string()));
    assert!(list_stdout.contains(&split_pane_id.to_string()));
    assert!(!list_stdout.contains(&initial_secret));
    assert!(!list_stdout.contains(&split_secret));

    let windows = handler
        .handle(Request::ListWindows(ListWindowsRequest {
            target: alpha.clone(),
            format: Some(list_format),
        }))
        .await;
    let windows_stdout = match windows {
        rmux_proto::Response::ListWindows(response) => {
            assert_eq!(response.windows.len(), 1);
            String::from_utf8(response.output.stdout).expect("list-windows utf8")
        }
        response => panic!("expected list-windows success, got {response:?}"),
    };
    assert!(!windows_stdout.contains(&initial_secret));
    assert!(!windows_stdout.contains(&split_secret));

    let killed = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: split_target,
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(killed, rmux_proto::Response::KillPane(_)));
    {
        let state = handler.state.lock().await;
        assert!(
            state.pane_lifecycle(split_pane_id).is_none(),
            "closed pane lifecycle state must be removed by pane id"
        );
    }

    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Pane(PaneTarget::with_window(alpha.clone(), 0, 0)),
                option: OptionName::RemainOnExit,
                value: "on".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        rmux_proto::Response::SetOption(_)
    ));
    let dead_respawn = handler
        .handle(Request::RespawnPane(RespawnPaneRequest {
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(vec!["exit 7".to_owned()]),
            process_command: None,
        }))
        .await;
    assert!(matches!(dead_respawn, rmux_proto::Response::RespawnPane(_)));
    wait_for_dead_pane(&handler, &alpha, 0, 0).await;
    let (dead_generation, dead_output_sequence) =
        wait_for_lifecycle_exit(&handler, initial_pane_id, 7).await;

    let respawned = handler
        .handle(Request::RespawnPane(RespawnPaneRequest {
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            kill: true,
            start_directory: Some(respawn_cwd.clone()),
            environment: Some(vec![respawn_secret.clone()]),
            command: Some(vec![respawn_command.clone()]),
            process_command: None,
        }))
        .await;
    assert!(matches!(respawned, rmux_proto::Response::RespawnPane(_)));
    {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        let pane = session
            .window_at(0)
            .and_then(|window| window.pane(0))
            .expect("respawned pane exists");
        assert_eq!(pane.id(), initial_pane_id);
        let lifecycle = state
            .pane_lifecycle(initial_pane_id)
            .expect("respawn lifecycle exists");
        assert_eq!(
            lifecycle.command(),
            Some(std::slice::from_ref(&respawn_command))
        );
        assert_eq!(lifecycle.working_directory(), Some(respawn_cwd.as_path()));
        assert_eq!(
            lifecycle.private_environment(),
            std::slice::from_ref(&respawn_secret)
        );
        assert!(!lifecycle.private_environment().contains(&initial_secret));
        assert!(matches!(
            lifecycle.process,
            PaneLifecycleProcessState::Running { .. }
        ));
        assert!(lifecycle.exit_state.is_none());
        assert!(lifecycle.generation > dead_generation);
        assert!(lifecycle.output_sequence > dead_output_sequence);
        assert!(lifecycle.output_sequence > initial_output_sequence);
    }

    let relisted = handler
        .handle(Request::ListPanes(ListPanesRequest {
            target: alpha,
            target_window_index: Some(0),
            format: Some(
                concat!(
                    "#{pane_id}\t#{pane_start_command}\t#{pane_start_path}\t",
                    "#{pane_lifecycle_generation}\t#{pane_output_sequence}\t",
                    "dead=#{pane_dead_status}\t#{RMUX_PRIVATE_INITIAL}\t",
                    "#{RMUX_PRIVATE_SPLIT}\t#{RMUX_PRIVATE_RESPAWN}"
                )
                .to_owned(),
            ),
        }))
        .await;
    let relisted_stdout = match relisted {
        rmux_proto::Response::ListPanes(response) => {
            String::from_utf8(response.output.stdout).expect("list-panes utf8")
        }
        response => panic!("expected list-panes success, got {response:?}"),
    };
    assert!(relisted_stdout.contains(&initial_pane_id.to_string()));
    assert!(!relisted_stdout.contains(&initial_secret));
    assert!(!relisted_stdout.contains(&split_secret));
    assert!(!relisted_stdout.contains(&respawn_secret));
    assert!(!relisted_stdout.contains("dead=7"));
    let _ = fs::remove_dir_all(initial_cwd);
    let _ = fs::remove_dir_all(respawn_cwd);
}

#[tokio::test]
async fn split_window_ext_applies_start_directory_to_spawned_process() {
    let handler = RequestHandler::new();
    let alpha = session_name("split-cwd");
    let cwd = unique_temp_path("split-cwd");
    let output = unique_temp_path("split-cwd-output");
    fs::create_dir_all(&cwd).expect("split cwd");
    create_session(&handler, &alpha).await;

    let response = handler
        .handle(Request::SplitWindowExt(SplitWindowExtRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
            command: Some(vec![cwd_probe_command(&output)]),
            process_command: None,
            start_directory: Some(cwd.clone()),
            keep_alive_on_exit: None,
        }))
        .await;
    let _split_target = match response {
        rmux_proto::Response::SplitWindow(response) => response.pane,
        response => panic!("expected split-window success, got {response:?}"),
    };

    let expected_cwd = expected_spawn_cwd(&cwd);
    wait_for_file_contents(&output, &expected_cwd).await;

    let _ = fs::remove_file(output);
    let _ = fs::remove_dir_all(cwd);
}

#[tokio::test]
async fn split_window_rolls_back_session_when_spawn_fails() {
    let handler = RequestHandler::new();
    let alpha = session_name("split-spawn-fails");
    create_session(&handler, &alpha).await;
    let missing_program = unique_temp_path("missing-program");

    let response = handler
        .handle(Request::SplitWindowExt(SplitWindowExtRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
            command: None,
            process_command: Some(ProcessCommand::Argv(vec![missing_program
                .to_string_lossy()
                .into_owned()])),
            start_directory: None,
            keep_alive_on_exit: None,
        }))
        .await;

    assert!(
        matches!(&response, rmux_proto::Response::Error(error) if error.error.to_string().contains("failed to spawn pane shell")),
        "expected spawn failure, got {response:?}"
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(session.window_at(0).expect("window exists").pane_count(), 1);
}

#[tokio::test]
async fn pane_output_sequence_advances_when_transcript_changes() {
    let handler = RequestHandler::new();
    let alpha = session_name("sequence");
    let created = handler
        .handle(Request::NewSessionExt(NewSessionExtRequest {
            session_name: Some(alpha.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(vec![pipe_discard_command()]),
            process_command: None,
        }))
        .await;
    assert!(matches!(created, rmux_proto::Response::NewSession(_)));

    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("initial pane exists")
    };
    let before = listed_output_sequence(&handler, &alpha).await;
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_runtime_pane_transcript(&alpha, pane_id, b"transcript output")
            .expect("append to runtime transcript");
    }
    let after = listed_output_sequence(&handler, &alpha).await;

    assert!(
        after > before,
        "pane_output_sequence should advance after pane output, before={before}, after={after}"
    );
}

async fn listed_output_sequence(handler: &RequestHandler, session_name: &SessionName) -> u64 {
    let listed = handler
        .handle(Request::ListPanes(ListPanesRequest {
            target: session_name.clone(),
            target_window_index: Some(0),
            format: Some("#{pane_output_sequence}".to_owned()),
        }))
        .await;
    let stdout = match listed {
        rmux_proto::Response::ListPanes(response) => {
            String::from_utf8(response.output.stdout).expect("list-panes utf8")
        }
        response => panic!("expected list-panes success, got {response:?}"),
    };
    stdout
        .trim()
        .parse::<u64>()
        .expect("pane_output_sequence is numeric")
}

#[tokio::test]
async fn move_pane_routes_through_join_semantics() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, &alpha).await;

    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        rmux_proto::Response::SplitWindow(_)
    ));
    {
        let mut state = handler.state.lock().await;
        let pane_id = state.sessions.allocate_pane_id();
        state
            .sessions
            .session_mut(&alpha)
            .expect("session exists")
            .insert_window_with_initial_pane_with_id(
                1,
                TerminalSize { cols: 80, rows: 24 },
                pane_id,
            )
            .expect("window insert succeeds");
        state
            .insert_window_terminal(
                &alpha,
                1,
                crate::pane_terminals::WindowSpawnOptions {
                    start_directory: None,
                    command: None,
                    socket_path: Path::new("/tmp/rmux-test.sock"),
                    environment_overrides: None,
                    pane_alert_callback: None,
                    pane_exit_callback: None,
                },
            )
            .expect("window terminal insert succeeds");
    }

    let response = handler
        .handle(Request::MovePane(MovePaneRequest {
            source: PaneTarget::with_window(alpha.clone(), 0, 1),
            target: PaneTarget::with_window(alpha.clone(), 1, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: true,
            full_size: false,
            size: Some(rmux_proto::PaneSplitSize::Absolute(12)),
        }))
        .await;

    assert_eq!(
        response,
        rmux_proto::Response::MovePane(rmux_proto::MovePaneResponse {
            target: PaneTarget::with_window(alpha.clone(), 1, 0),
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(
        session
            .window_at(1)
            .expect("destination window exists")
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[tokio::test]
async fn break_pane_print_target_uses_custom_format() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, &alpha).await;

    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        rmux_proto::Response::SplitWindow(_)
    ));

    let response = handler
        .handle(Request::BreakPane(BreakPaneRequest {
            source: PaneTarget::with_window(alpha.clone(), 0, 1),
            target: Some(WindowTarget::with_window(alpha.clone(), 1)),
            name: None,
            detached: true,
            after: false,
            before: false,
            print_target: true,
            format: Some("#{window_index}.#{pane_index}".to_owned()),
        }))
        .await;

    let rmux_proto::Response::BreakPane(success) = response else {
        panic!("expected break-pane response");
    };
    let output = success.command_output().expect("break-pane -P output");
    assert_eq!(output.stdout(), b"1.0\n");
}

#[tokio::test]
async fn pipe_pane_once_keeps_the_existing_pipe() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let first_output = unique_temp_path("pipe-once-first");
    let second_output = unique_temp_path("pipe-once-second");
    create_session(&handler, &alpha).await;

    let first = handler
        .handle(Request::PipePane(PipePaneRequest {
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            stdin: false,
            stdout: true,
            once: false,
            command: Some(pipe_to_file_command(&first_output)),
        }))
        .await;
    assert!(matches!(first, rmux_proto::Response::PipePane(_)));

    let second = handler
        .handle(Request::PipePane(PipePaneRequest {
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            stdin: false,
            stdout: true,
            once: true,
            command: Some(pipe_to_file_command(&second_output)),
        }))
        .await;
    assert!(matches!(second, rmux_proto::Response::PipePane(_)));

    let sent = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            keys: vec![pane_print_command("pipe-once-test"), "Enter".to_owned()],
        }))
        .await;
    assert!(matches!(sent, rmux_proto::Response::SendKeys(_)));

    wait_for_file_contains(&first_output, "pipe-once-test").await;
    sleep(Duration::from_millis(150)).await;
    assert!(
        !second_output.exists(),
        "toggle-once should not replace the existing pipe"
    );

    let _ = handler
        .handle(Request::PipePane(PipePaneRequest {
            target: PaneTarget::with_window(alpha, 0, 0),
            stdin: false,
            stdout: true,
            once: false,
            command: None,
        }))
        .await;
    let _ = fs::remove_file(first_output);
}

#[tokio::test]
async fn pipe_pane_rejects_dead_panes() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, &alpha).await;
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Pane(PaneTarget::with_window(alpha.clone(), 0, 0)),
                option: OptionName::RemainOnExit,
                value: "on".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        rmux_proto::Response::SetOption(_)
    ));

    let respawned = handler
        .handle(Request::RespawnPane(RespawnPaneRequest {
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(vec!["exit 0".to_owned()]),
            process_command: None,
        }))
        .await;
    assert!(matches!(respawned, rmux_proto::Response::RespawnPane(_)));
    wait_for_dead_pane(&handler, &alpha, 0, 0).await;

    let response = handler
        .handle(Request::PipePane(PipePaneRequest {
            target: PaneTarget::with_window(alpha, 0, 0),
            stdin: false,
            stdout: true,
            once: false,
            command: Some(pipe_discard_command()),
        }))
        .await;

    assert!(
        matches!(&response, rmux_proto::Response::Error(error) if error.error.to_string().contains("target pane has exited")),
        "expected dead-pane error, got {response:?}"
    );
}

#[tokio::test]
async fn respawn_pane_rejects_active_pane_without_kill_flag() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, &alpha).await;

    let response = handler
        .handle(Request::RespawnPane(RespawnPaneRequest {
            target: PaneTarget::with_window(alpha, 0, 0),
            kill: false,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
        }))
        .await;

    assert!(
        matches!(&response, rmux_proto::Response::Error(error) if error.error.to_string().contains("still active")),
        "expected still-active error, got {response:?}"
    );
}

#[tokio::test]
async fn respawn_pane_with_kill_flag_applies_directory_environment_and_command() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let cwd = unique_temp_path("respawn-pane-cwd");
    let output = unique_temp_path("respawn-pane-output");
    fs::create_dir_all(&cwd).expect("respawn pane cwd");
    create_session(&handler, &alpha).await;

    let response = handler
        .handle(Request::RespawnPane(RespawnPaneRequest {
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            kill: true,
            start_directory: Some(cwd.clone()),
            environment: Some(vec!["RMUX_RESPAWN=ready".to_owned()]),
            command: Some(vec![respawn_probe_command(&output)]),
            process_command: None,
        }))
        .await;

    assert_eq!(
        response,
        rmux_proto::Response::RespawnPane(rmux_proto::RespawnPaneResponse {
            target: PaneTarget::with_window(alpha, 0, 0),
        })
    );
    let expected_cwd = expected_spawn_cwd(&cwd);
    wait_for_file_contents(&output, &format!("{expected_cwd}:ready")).await;
    let _ = fs::remove_file(output);
    let _ = fs::remove_dir_all(cwd);
}

#[tokio::test]
async fn respawn_pane_with_kill_flag_emits_replaced_pane_exit() {
    let handler = RequestHandler::new();
    let alpha = session_name("respawn-exit");
    create_session(&handler, &alpha).await;
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    let (pane_id, previous_generation) = {
        let state = handler.state.lock().await;
        let pane = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .expect("initial pane exists");
        let lifecycle = state
            .pane_lifecycle(pane.id())
            .expect("initial lifecycle exists");
        (pane.id(), lifecycle.generation)
    };
    let mut lifecycle_events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::RespawnPane(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(vec![pipe_discard_command()]),
            process_command: None,
        }))
        .await;
    assert!(matches!(response, rmux_proto::Response::RespawnPane(_)));

    let queued = timeout(Duration::from_millis(500), lifecycle_events.recv())
        .await
        .expect("forced respawn should emit pane-exited")
        .expect("lifecycle channel should stay open");
    match queued.event {
        LifecycleEvent::PaneExited {
            target: event_target,
            pane_id: Some(event_pane_id),
            window_id: Some(_),
            ..
        } => {
            assert_eq!(event_target, target);
            assert_eq!(event_pane_id, pane_id.as_u32());
        }
        event => panic!("expected pane-exited for replaced process, got {event:?}"),
    }

    let state = handler.state.lock().await;
    let pane = state
        .sessions
        .session(&alpha)
        .and_then(|session| session.window_at(0))
        .and_then(|window| window.pane(0))
        .expect("respawned pane exists");
    assert_eq!(pane.id(), pane_id);
    let lifecycle = state
        .pane_lifecycle(pane_id)
        .expect("respawned lifecycle exists");
    assert!(lifecycle.generation > previous_generation);
    assert!(matches!(
        lifecycle.process,
        PaneLifecycleProcessState::Running { .. }
    ));
    assert!(lifecycle.exit_state.is_none());
}

#[tokio::test]
async fn respawn_pane_preserves_id_and_clears_parser_state_before_new_output() {
    let handler = RequestHandler::new();
    let alpha = session_name("respawn-reset");
    create_session(&handler, &alpha).await;
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("initial pane exists")
    };

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_runtime_pane_transcript(&alpha, pane_id, b"OLD_MARKER")
            .expect("append old output");
    }
    let before = snapshot_response(&handler, target.clone()).await;
    assert!(all_visible_text(&before).contains("OLD_MARKER"));
    let (previous_generation, previous_revision, previous_output_sequence) = {
        let state = handler.state.lock().await;
        let lifecycle = state
            .pane_lifecycle(pane_id)
            .expect("initial lifecycle exists");
        (
            lifecycle.generation,
            lifecycle.revision,
            lifecycle.output_sequence,
        )
    };

    let response = handler
        .handle(Request::RespawnPane(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(vec![pipe_discard_command()]),
            process_command: None,
        }))
        .await;
    assert!(matches!(response, rmux_proto::Response::RespawnPane(_)));

    let after = snapshot_response(&handler, target).await;
    assert!(
        !all_visible_text(&after).contains("OLD_MARKER"),
        "respawn must discard the old transcript and parser screen before fresh output"
    );
    let state = handler.state.lock().await;
    let pane = state
        .sessions
        .session(&alpha)
        .and_then(|session| session.window_at(0))
        .and_then(|window| window.pane(0))
        .expect("respawned pane exists");
    assert_eq!(pane.id(), pane_id);
    let lifecycle = state
        .pane_lifecycle(pane_id)
        .expect("respawned lifecycle exists");
    assert!(lifecycle.generation > previous_generation);
    assert!(lifecycle.revision > previous_revision);
    assert!(lifecycle.output_sequence > previous_output_sequence);
}

#[tokio::test]
async fn display_panes_uses_the_default_select_pane_template() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = 42_u32;
    create_session(&handler, &alpha).await;

    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        rmux_proto::Response::SplitWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SelectPane(SelectPaneRequest {
                target: PaneTarget::with_window(alpha.clone(), 0, 0),
                title: None,
            }))
            .await,
        rmux_proto::Response::SelectPane(_)
    ));

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let response = handler
        .handle(Request::DisplayPanes(DisplayPanesRequest {
            target: alpha.clone(),
            duration_ms: Some(5_000),
            non_blocking: true,
            no_command: false,
            template: None,
        }))
        .await;
    assert!(matches!(response, rmux_proto::Response::DisplayPanes(_)));
    let _overlay = control_rx.recv().await.expect("display-panes overlay");

    handler
        .handle_attached_live_input_for_test(requester_pid, b"1")
        .await
        .expect("display-panes select input");
    let _clear = control_rx
        .recv()
        .await
        .expect("display-panes clear overlay");

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(session.active_pane_index(), 1);
}

#[tokio::test]
async fn display_panes_without_a_command_keeps_the_active_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = 43_u32;
    create_session(&handler, &alpha).await;

    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        rmux_proto::Response::SplitWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SelectPane(SelectPaneRequest {
                target: PaneTarget::with_window(alpha.clone(), 0, 0),
                title: None,
            }))
            .await,
        rmux_proto::Response::SelectPane(_)
    ));

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let response = handler
        .handle(Request::DisplayPanes(DisplayPanesRequest {
            target: alpha.clone(),
            duration_ms: Some(5_000),
            non_blocking: true,
            no_command: true,
            template: None,
        }))
        .await;
    assert!(matches!(response, rmux_proto::Response::DisplayPanes(_)));
    let _overlay = control_rx.recv().await.expect("display-panes overlay");

    handler
        .handle_attached_live_input_for_test(requester_pid, b"1")
        .await
        .expect("display-panes close input");
    let _clear = control_rx
        .recv()
        .await
        .expect("display-panes clear overlay");

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(session.active_pane_index(), 0);
}

#[tokio::test]
async fn display_panes_uses_the_session_option_duration_by_default() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = 44_u32;
    create_session(&handler, &alpha).await;

    {
        let mut state = handler.state.lock().await;
        state
            .options
            .set(
                ScopeSelector::Session(alpha.clone()),
                OptionName::DisplayPanesTime,
                "25".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("set display-panes-time");
    }

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let response = handler
        .handle(Request::DisplayPanes(DisplayPanesRequest {
            target: alpha.clone(),
            duration_ms: None,
            non_blocking: true,
            no_command: true,
            template: None,
        }))
        .await;
    assert!(matches!(response, rmux_proto::Response::DisplayPanes(_)));
    let _overlay = control_rx.recv().await.expect("display-panes overlay");

    timeout(Duration::from_millis(250), async {
        loop {
            let cleared = {
                let active_attach = handler.active_attach.lock().await;
                active_attach
                    .by_pid
                    .get(&requester_pid)
                    .and_then(|active| active.display_panes.as_ref())
                    .is_none()
            };
            if cleared {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("display-panes state should clear with option duration");
}

#[tokio::test]
async fn display_panes_timeout_emits_a_clear_overlay_to_the_attached_client() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = 45_u32;
    create_session(&handler, &alpha).await;

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let response = handler
        .handle(Request::DisplayPanes(DisplayPanesRequest {
            target: alpha.clone(),
            duration_ms: Some(25),
            non_blocking: true,
            no_command: true,
            template: None,
        }))
        .await;
    assert!(matches!(response, rmux_proto::Response::DisplayPanes(_)));

    let first = timeout(Duration::from_secs(1), control_rx.recv())
        .await
        .expect("overlay should arrive")
        .expect("overlay command");
    assert!(matches!(first, AttachControl::Overlay(_)));

    let mut seen = Vec::new();
    let clear = timeout(Duration::from_secs(1), async {
        loop {
            let next = control_rx.recv().await.expect("follow-up control");
            match next {
                AttachControl::Overlay(clear) => break clear,
                other => seen.push(format!("{other:?}")),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("clear overlay should arrive; saw {seen:?}"));
    assert!(
        !clear.frame.is_empty(),
        "display-panes clear overlay should repaint the client"
    );
}

#[tokio::test]
async fn join_pane_rejects_same_source_and_target() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, &alpha).await;

    let response = handler
        .handle(Request::JoinPane(rmux_proto::JoinPaneRequest {
            source: PaneTarget::with_window(alpha.clone(), 0, 0),
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            direction: SplitDirection::Vertical,
            detached: false,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;

    assert!(
        matches!(&response, rmux_proto::Response::Error(error) if error.error.to_string().contains("must be different")),
        "expected same-pane error, got {response:?}"
    );
}

#[tokio::test]
async fn move_pane_rejects_same_source_and_target() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, &alpha).await;

    let response = handler
        .handle(Request::MovePane(MovePaneRequest {
            source: PaneTarget::with_window(alpha.clone(), 0, 0),
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            direction: SplitDirection::Vertical,
            detached: false,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;

    assert!(
        matches!(&response, rmux_proto::Response::Error(error) if error.error.to_string().contains("must be different")),
        "expected same-pane error, got {response:?}"
    );
}

#[tokio::test]
async fn swap_pane_self_swap_is_a_no_op() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, &alpha).await;

    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        rmux_proto::Response::SplitWindow(_)
    ));

    let response = handler
        .handle(Request::SwapPane(rmux_proto::SwapPaneRequest {
            source: PaneTarget::with_window(alpha.clone(), 0, 0),
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            direction: None,
            detached: false,
            preserve_zoom: false,
        }))
        .await;

    assert!(
        matches!(response, rmux_proto::Response::SwapPane(_)),
        "self-swap should succeed as a no-op, got {response:?}"
    );
}

#[tokio::test]
async fn respawn_pane_dead_pane_succeeds_without_kill_flag() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    create_session(&handler, &alpha).await;

    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Pane(target.clone()),
                option: OptionName::RemainOnExit,
                value: "on".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        rmux_proto::Response::SetOption(_)
    ));

    let respawned = handler
        .handle(Request::RespawnPane(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(vec!["exit 0".to_owned()]),
            process_command: None,
        }))
        .await;
    assert!(matches!(respawned, rmux_proto::Response::RespawnPane(_)));
    wait_for_dead_pane(&handler, &alpha, 0, 0).await;

    let response = handler
        .handle(Request::RespawnPane(RespawnPaneRequest {
            target,
            kill: false,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
        }))
        .await;

    assert!(
        matches!(response, rmux_proto::Response::RespawnPane(_)),
        "respawning a dead pane without -k should succeed, got {response:?}"
    );
}

#[tokio::test]
async fn remain_on_exit_keeps_the_existing_window_name() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    create_session(&handler, &alpha).await;

    assert!(matches!(
        handler
            .handle(Request::RenameWindow(RenameWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 0),
                name: "custom".to_owned(),
            }))
            .await,
        rmux_proto::Response::RenameWindow(_)
    ));

    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Pane(target.clone()),
                option: OptionName::RemainOnExit,
                value: "on".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        rmux_proto::Response::SetOption(_)
    ));

    let expected_window_name = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.name())
            .expect("renamed window keeps its explicit name")
            .to_owned()
    };

    let response = handler
        .handle(Request::RespawnPane(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(vec!["exit 0".to_owned()]),
            process_command: None,
        }))
        .await;
    assert!(matches!(response, rmux_proto::Response::RespawnPane(_)));
    wait_for_dead_pane(&handler, &alpha, 0, 0).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (ready, observation) = {
            let state = handler.state.lock().await;
            match state
                .sessions
                .session(&alpha)
                .and_then(|session| session.window_at(0))
                .and_then(|window| {
                    window
                        .pane(0)
                        .map(|pane| (window.name().map(str::to_owned), pane.id()))
                }) {
                Some((window_name, pane_id)) => {
                    let dead = state.pane_is_dead(&alpha, pane_id);
                    (
                        window_name.as_deref() == Some(expected_window_name.as_str()) && dead,
                        format!(
                            "last_window_name={window_name:?} last_dead={dead:?} last_pane_id={:?}",
                            pane_id.as_u32()
                        ),
                    )
                }
                None => (
                    false,
                    "last_window_name=None last_dead=None last_pane_id=None".to_owned(),
                ),
            }
        };
        if ready {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for remain-on-exit window name to stay at {expected_window_name:?}; {observation}"
            );
        }
        sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn remain_on_exit_auto_named_window_gets_tmux_dead_suffix_when_unattached() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    create_session(&handler, &alpha).await;

    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Pane(target.clone()),
                option: OptionName::RemainOnExit,
                value: "on".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        rmux_proto::Response::SetOption(_)
    ));

    let expected_window_name = "exit[dead]".to_owned();

    let response = handler
        .handle(Request::RespawnPane(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(vec!["exit 0".to_owned()]),
            process_command: None,
        }))
        .await;
    assert!(matches!(response, rmux_proto::Response::RespawnPane(_)));
    wait_for_dead_pane(&handler, &alpha, 0, 0).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (ready, observation) = {
            let state = handler.state.lock().await;
            match state
                .sessions
                .session(&alpha)
                .and_then(|session| session.window_at(0))
                .and_then(|window| {
                    window
                        .pane(0)
                        .map(|pane| (window.name().map(str::to_owned), pane.id()))
                }) {
                Some((window_name, pane_id)) => {
                    let dead = state.pane_is_dead(&alpha, pane_id);
                    (
                        window_name.as_deref() == Some(expected_window_name.as_str()) && dead,
                        format!(
                            "last_window_name={window_name:?} last_dead={dead:?} last_pane_id={:?}",
                            pane_id.as_u32()
                        ),
                    )
                }
                None => (
                    false,
                    "last_window_name=None last_dead=None last_pane_id=None".to_owned(),
                ),
            }
        };
        if ready {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for remain-on-exit automatic dead name {expected_window_name:?}; {observation}"
            );
        }
        sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn pipe_pane_close_on_nonexistent_pipe_is_a_no_op() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, &alpha).await;

    let response = handler
        .handle(Request::PipePane(PipePaneRequest {
            target: PaneTarget::with_window(alpha, 0, 0),
            stdin: false,
            stdout: true,
            once: false,
            command: None,
        }))
        .await;

    assert!(
        matches!(response, rmux_proto::Response::PipePane(_)),
        "closing a non-existent pipe should succeed, got {response:?}"
    );
}

#[tokio::test]
async fn pipe_pane_empty_command_closes_existing_pipe() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, &alpha).await;

    let open = handler
        .handle(Request::PipePane(PipePaneRequest {
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            stdin: false,
            stdout: true,
            once: false,
            command: Some(pipe_discard_command()),
        }))
        .await;
    assert!(matches!(open, rmux_proto::Response::PipePane(_)));

    let close = handler
        .handle(Request::PipePane(PipePaneRequest {
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            stdin: false,
            stdout: true,
            once: false,
            command: Some(String::new()),
        }))
        .await;
    assert!(
        matches!(close, rmux_proto::Response::PipePane(_)),
        "empty command should close existing pipe, got {close:?}"
    );

    // Opening a new pipe after an empty-command close should succeed, confirming the previous
    // pipe was cleaned up.
    let reopen = handler
        .handle(Request::PipePane(PipePaneRequest {
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            stdin: false,
            stdout: true,
            once: true,
            command: Some(pipe_discard_command()),
        }))
        .await;
    assert!(
        matches!(reopen, rmux_proto::Response::PipePane(_)),
        "reopening after close should succeed"
    );

    let _ = handler
        .handle(Request::PipePane(PipePaneRequest {
            target: PaneTarget::with_window(alpha, 0, 0),
            stdin: false,
            stdout: true,
            once: false,
            command: None,
        }))
        .await;
}

async fn snapshot_response(
    handler: &RequestHandler,
    target: PaneTarget,
) -> rmux_proto::PaneSnapshotResponse {
    match handler
        .handle(Request::PaneSnapshot(PaneSnapshotRequest { target }))
        .await
    {
        rmux_proto::Response::PaneSnapshot(response) => response,
        other => panic!("expected pane-snapshot response, got {other:?}"),
    }
}

fn collect_visible_text(response: &rmux_proto::PaneSnapshotResponse, row: usize) -> String {
    let cols = usize::from(response.cols);
    let start = row.saturating_mul(cols);
    let end = start.saturating_add(cols).min(response.cells.len());
    response.cells[start..end]
        .iter()
        .filter(|cell| !cell.padding)
        .map(|cell| cell.text.as_str())
        .collect::<String>()
        .trim_end_matches(' ')
        .to_owned()
}

fn all_visible_text(response: &rmux_proto::PaneSnapshotResponse) -> String {
    (0..usize::from(response.rows))
        .map(|row| collect_visible_text(response, row))
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn pane_snapshot_returns_live_screen_built_via_terminal_parser() {
    // The hardening contract for the snapshot endpoint: cells must come from
    // the live `Screen` fed by rmux-core's crate-private terminal parser, and
    // not from a `String::from_utf8_lossy(capture-pane -p)` reconstruction.
    // Feeding raw PTY-style bytes through the transcript parser and then
    // observing the structured cells exercises that exact path end-to-end.
    let handler = RequestHandler::new();
    let alpha = session_name("snapshot-live");
    let created = handler
        .handle(Request::NewSessionExt(NewSessionExtRequest {
            session_name: Some(alpha.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 12, rows: 4 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(vec![pipe_discard_command()]),
            process_command: None,
        }))
        .await;
    assert!(matches!(created, rmux_proto::Response::NewSession(_)));

    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("initial pane exists")
    };

    let target = PaneTarget::with_window(alpha.clone(), 0, 0);

    let baseline = snapshot_response(&handler, target.clone()).await;
    assert_eq!(baseline.cols, 12);
    assert_eq!(baseline.rows, 4);
    assert_eq!(baseline.cells.len(), 48);
    assert_ne!(
        baseline.revision, 0,
        "live panes must carry a non-zero revision"
    );

    // Feed bytes that include a wide glyph and an SGR escape into the
    // transcript. Both must reach the structured cells, since the parser is
    // the only producer of the screen state behind the snapshot endpoint.
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_runtime_pane_transcript(
                &alpha,
                pane_id,
                "hi界\x1b[31mZ\x1b[0m".as_bytes(),
            )
            .expect("append bytes through parser");
    }

    let after = snapshot_response(&handler, target.clone()).await;
    assert_eq!(after.cols, 12);
    assert_eq!(after.rows, 4);
    assert_eq!(after.cells.len(), 48);
    assert_ne!(
        after.revision, baseline.revision,
        "fed bytes must change the snapshot revision",
    );

    // The first row must contain the parsed glyphs in column order, with a
    // padding cell for the second column of the wide glyph.
    let row0 = &after.cells[0..12];
    assert_eq!(row0[0].text, "h");
    assert_eq!(row0[0].width, 1);
    assert!(!row0[0].padding);
    assert_eq!(row0[1].text, "i");
    assert_eq!(row0[2].text, "界");
    assert_eq!(row0[2].width, 2);
    assert!(!row0[2].padding);
    assert!(
        row0[3].padding,
        "the column following a wide glyph must be padding"
    );
    assert_eq!(row0[3].width, 0);
    assert_eq!(row0[4].text, "Z");
    // The SGR sequence must paint the foreground colour onto the Z cell, not
    // pollute the cell text with literal escape bytes.
    assert!(
        !row0[4].text.contains('\x1b'),
        "raw escape bytes must never leak into cell text"
    );
    assert_ne!(
        row0[4].fg, baseline.cells[4].fg,
        "the parsed SGR must change the foreground colour for the Z cell"
    );
    assert_eq!(
        collect_visible_text(&after, 0),
        "hi界Z",
        "padding-skipped row text must reflect the parsed glyphs"
    );

    // A subsequent capture without further bytes must yield the same cells
    // and revision, confirming determinism for unchanged screen state.
    let again = snapshot_response(&handler, target).await;
    assert_eq!(again.revision, after.revision);
    assert_eq!(again.cells, after.cells);
}

#[tokio::test]
async fn pane_snapshot_invalid_target_returns_error_response() {
    let handler = RequestHandler::new();
    let alpha = session_name("snapshot-missing");
    let response = handler
        .handle(Request::PaneSnapshot(PaneSnapshotRequest {
            target: PaneTarget::with_window(alpha, 0, 0),
        }))
        .await;
    match response {
        rmux_proto::Response::Error(_) => {}
        other => panic!("expected error response for missing session, got {other:?}"),
    }
}

#[tokio::test]
async fn pane_snapshot_folds_invalid_utf8_through_parser_not_raw_bytes() {
    // Invalid UTF-8 bytes must be folded into U+FFFD by the rmux-core
    // terminal parser *before* they reach the structured snapshot cells.
    // The endpoint must not leak raw invalid bytes into `cell.text`, since
    // there is no `String::from_utf8_lossy(capture-pane -p)` salvage step
    // to clean them up later.
    let handler = RequestHandler::new();
    let alpha = session_name("snapshot-bad-utf8");
    let created = handler
        .handle(Request::NewSessionExt(NewSessionExtRequest {
            session_name: Some(alpha.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 8, rows: 2 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(vec![pipe_discard_command()]),
            process_command: None,
        }))
        .await;
    assert!(matches!(created, rmux_proto::Response::NewSession(_)));

    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("initial pane exists")
    };

    {
        let mut state = handler.state.lock().await;
        // 0xFF is invalid as a UTF-8 leading byte, and 0xC3 0x28 is an
        // invalid 2-byte sequence (continuation byte 0x28 is not 0x80..=0xBF).
        // The parser must absorb both and emit replacement cells instead of
        // leaking the raw bytes.
        state
            .append_bytes_to_runtime_pane_transcript(&alpha, pane_id, b"a\xFFb\xC3\x28c")
            .expect("append invalid utf-8 through parser");
    }

    let target = PaneTarget::with_window(alpha, 0, 0);
    let response = snapshot_response(&handler, target).await;
    let row0 = &response.cells[0..usize::from(response.cols)];
    for (col, cell) in row0.iter().enumerate() {
        // Every cell text must be valid UTF-8 (a Vec<u8> from `text` is
        // already constrained, but assert no cell carries raw bytes that
        // happen to look like an escape or NUL).
        assert!(
            !cell.text.contains('\u{0000}'),
            "col {col} text {:?} leaks NUL",
            cell.text,
        );
        assert!(
            cell.text.chars().all(|ch| ch != '\u{001B}'),
            "col {col} text {:?} leaks escape byte",
            cell.text,
        );
    }
    let visible: String = row0
        .iter()
        .filter(|cell| !cell.padding)
        .map(|cell| cell.text.as_str())
        .collect::<String>()
        .trim_end_matches(' ')
        .to_owned();
    assert!(
        visible.contains('a') && visible.contains('b') && visible.contains('c'),
        "valid bytes around the invalid sequences must survive: {visible:?}",
    );
    assert!(
        visible.contains('\u{FFFD}'),
        "invalid utf-8 must be folded by the parser into U+FFFD, got {visible:?}",
    );
}

#[tokio::test]
async fn pane_snapshot_revision_changes_after_clear_history() {
    // Clearing scrollback is observable: history_size and history_bytes drop
    // to zero. The revision must change so SDK consumers don't treat the
    // post-clear screen as identical to the pre-clear one.
    let handler = RequestHandler::new();
    let alpha = session_name("snapshot-clear");
    let created = handler
        .handle(Request::NewSessionExt(NewSessionExtRequest {
            session_name: Some(alpha.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 4, rows: 2 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(vec![pipe_discard_command()]),
            process_command: None,
        }))
        .await;
    assert!(matches!(created, rmux_proto::Response::NewSession(_)));

    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("initial pane exists")
    };

    {
        let mut state = handler.state.lock().await;
        // Pump enough lines to push older content into scrollback history.
        state
            .append_bytes_to_runtime_pane_transcript(&alpha, pane_id, b"L1\r\nL2\r\nL3\r\nL4\r\n")
            .expect("append lines through parser");
    }

    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    let before = snapshot_response(&handler, target.clone()).await;

    let cleared = handler
        .handle(Request::ClearHistory(rmux_proto::ClearHistoryRequest {
            target: target.clone(),
            reset_hyperlinks: false,
        }))
        .await;
    assert!(matches!(cleared, rmux_proto::Response::ClearHistory(_)));

    let after = snapshot_response(&handler, target).await;
    assert_ne!(
        before.revision, after.revision,
        "clearing scrollback must change the snapshot revision",
    );
}
