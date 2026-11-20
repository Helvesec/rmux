#[cfg(test)]
use crate::session_fixture::SessionSetupExt as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::RequestHandler;
use crate::handler::test_support::SERVER_SIGNAL_LIVENESS;
use crate::pane_io::AttachControl;
use crate::pane_terminals::PaneLifecycleProcessState;
use crate::test_shell::final_sink::FinalSinkSlot;
use rmux_proto::{
    BreakPaneRequest, DisplayPanesRequest, KillPaneRequest, ListPanesRequest, ListWindowsRequest,
    MovePaneRequest, NewSessionExtRequest, NewSessionRequest, NewWindowRequest, OptionName,
    PaneInputRequest, PaneKillRequest, PaneRespawnRequest, PaneSnapshotRequest, PaneTarget,
    PaneTargetRef, PipePaneRequest, ProcessCommand, RenameWindowRequest, Request,
    ResizePaneAdjustment, ResizePaneRequest, RespawnPaneRequest, Response, ScopeSelector,
    SelectPaneRequest, SessionName, SetHookMutationRequest, SetOptionMode, SetOptionRequest,
    SplitDirection, SplitWindowExtRequest, SplitWindowRequest, SplitWindowTarget, TerminalSize,
    WindowTarget,
};
use rmux_proto::{HookLifecycle, HookName};
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};

#[path = "handler_pane_command_tests/split_empty.rs"]
mod split_empty;

#[cfg(windows)]
const PROCESS_OUTPUT_FILE_TIMEOUT: Duration = Duration::from_secs(20);
#[cfg(not(windows))]
const PROCESS_OUTPUT_FILE_TIMEOUT: Duration = Duration::from_secs(5);

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

fn pipe_discard_command() -> String {
    crate::test_shell::stdin_discard_command()
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

#[cfg(unix)]
fn respawn_replay_script(output: &Path, tag: &str) -> String {
    format!(
        "printf '%s:%s:{tag}\\n' \"$(pwd)\" \"$RMUX_RESPAWN\" >> {}; sleep 60",
        shell_quote(output)
    )
}

#[cfg(windows)]
fn respawn_replay_script(output: &Path, tag: &str) -> String {
    format!(
        "[System.IO.File]::AppendAllText({}, ((Get-Location).Path + ':' + $env:RMUX_RESPAWN + ':{}' + [char]10)); Start-Sleep -Seconds 60",
        crate::test_shell::powershell_quote_path(output),
        tag
    )
}

#[cfg(unix)]
fn respawn_argv_probe_command(output: &Path, tag: &str) -> Vec<String> {
    vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        respawn_replay_script(output, tag),
    ]
}

#[cfg(windows)]
fn respawn_argv_probe_command(output: &Path, tag: &str) -> Vec<String> {
    vec![
        "powershell.exe".to_owned(),
        "-NoProfile".to_owned(),
        "-NonInteractive".to_owned(),
        "-Command".to_owned(),
        respawn_replay_script(output, tag),
    ]
}

#[cfg(unix)]
fn respawn_shell_probe_command(output: &Path, tag: &str) -> String {
    respawn_replay_script(output, tag)
}

#[cfg(unix)]
fn respawn_shell_identity_command(output: &Path, tag: &str) -> String {
    format!(
        "printf '%s:%s:{tag}\\n' \"${{0##*/}}\" \"$SHELL\" >> {}; sleep 60",
        shell_quote(output)
    )
}

#[cfg(unix)]
async fn set_global_default_shell(handler: &RequestHandler, shell: &str) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::DefaultShell,
            value: shell.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

#[cfg(windows)]
fn respawn_shell_probe_command(output: &Path, tag: &str) -> String {
    crate::test_shell::powershell_encoded_command(&respawn_replay_script(output, tag))
}

fn respawn_probe_line(cwd: &str, environment: &str, tag: &str) -> String {
    format!("{cwd}:{environment}:{tag}\n")
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
    crate::session_fixture::assert_new_session(&created);
}

async fn create_grouped_session(
    handler: &RequestHandler,
    session_name: &SessionName,
    group_target: &SessionName,
) {
    let created = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session_name.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: Some(group_target.clone()),
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    crate::session_fixture::assert_new_session(&created);
}

async fn create_three_pane_window(handler: &RequestHandler, session_name: &SessionName) {
    create_session(handler, session_name).await;
    for _ in 0..2 {
        let split = handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session_name.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await;
        assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
    }
}

async fn active_and_last_pane_ids(
    handler: &RequestHandler,
    session_name: &SessionName,
) -> (rmux_core::PaneId, rmux_core::PaneId) {
    let state = handler.state.lock().await;
    let window = state
        .sessions
        .session(session_name)
        .expect("session exists")
        .window_at(0)
        .expect("window exists");
    let active_id = window.active_pane().expect("active pane exists").id();
    let last_id = window
        .last_pane_index()
        .and_then(|pane_index| window.pane(pane_index))
        .expect("last pane exists")
        .id();
    (active_id, last_id)
}

fn list_stdout(response: Response) -> String {
    match response {
        Response::ListPanes(response) => {
            String::from_utf8(response.output.stdout).expect("list-panes stdout is utf8")
        }
        response => panic!("expected list-panes success, got {response:?}"),
    }
}

async fn attach_to_existing_session(
    handler: &RequestHandler,
    requester_pid: u32,
    session_name: &SessionName,
) -> mpsc::UnboundedReceiver<AttachControl> {
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, session_name.clone(), control_tx)
        .await;
    control_rx
}

async fn wait_for_attached_session_exit(control_rx: &mut mpsc::UnboundedReceiver<AttachControl>) {
    timeout(Duration::from_secs(5), async {
        loop {
            match control_rx.recv().await {
                Some(AttachControl::Exited) => break,
                Some(_) => {}
                None => panic!("attach control channel closed before session exit"),
            }
        }
    })
    .await
    .expect("timed out waiting for source session attach exit");
}

async fn wait_for_file_contents(path: &Path, expected: &str) {
    let deadline = tokio::time::Instant::now() + PROCESS_OUTPUT_FILE_TIMEOUT;
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

async fn pipe_process_group_probe(
    handler: &RequestHandler,
    target: &PaneTarget,
) -> crate::pane_terminals::PipeProcessGroupProbe {
    handler
        .state
        .lock()
        .await
        .pane_pipe_process_group_probe_for_test(target)
        .expect("active pipe process group probe")
}

async fn wait_for_pipe_child_to_stop(probe: &crate::pane_terminals::PipeProcessGroupProbe) {
    let deadline = tokio::time::Instant::now() + PROCESS_OUTPUT_FILE_TIMEOUT;
    loop {
        if !probe.child_wait_pending() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the pipe-pane child to stop"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn list_panes_activity_sort_follows_selection_counter_like_tmux() {
    // Oracle probes 2026-07-09 (pinned tmux 3.7b): pane "activity" ordering
    // is the active_point selection counter, ascending. Scenario A: fresh
    // window, detached splits, no selections -> index order, identical when
    // reversed. Scenario B: select pane 1 then pane 0 -> "2 1 0", reversed
    // "0 1 2". Output/alert activity does not reorder anything.
    let handler = RequestHandler::new();
    let session = session_name("list-panes-activity-sort");
    create_session(&handler, &session).await;
    for _ in 0..2 {
        let split = handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session.clone()),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
            }))
            .await;
        assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
    }

    let list = |reversed: bool| {
        let handler = handler.clone();
        let session = session.clone();
        async move {
            list_stdout(
                handler
                    .handle(Request::ListPanes(Box::new(ListPanesRequest {
                        target: session,
                        target_window_index: Some(0),
                        format: Some("#{pane_index}".to_owned()),
                        filter: None,
                        sort_order: Some("activity".to_owned()),
                        reversed,
                    })))
                    .await,
            )
        }
    };

    let select = |pane_index: u32| {
        let handler = handler.clone();
        let session = session.clone();
        async move {
            let response = handler
                .handle(Request::SelectPane(Box::new(SelectPaneRequest {
                    target: PaneTarget::with_window(session, 0, pane_index),
                    title: None,
                    input_disabled: None,
                    preserve_zoom: false,
                    style: None,
                })))
                .await;
            assert!(matches!(response, Response::SelectPane(_)), "{response:?}");
        }
    };

    // Scenario A: no explicit selection yet -> index order.
    assert_eq!(list(false).await, "0\n1\n2\n");

    // Scenario B: selections define the order.
    select(1).await;
    select(0).await;
    assert_eq!(list(false).await, "2\n1\n0\n");
    assert_eq!(list(true).await, "0\n1\n2\n");
}

#[tokio::test]
async fn list_panes_size_sort_uses_area_in_both_directions() {
    let handler = RequestHandler::new();
    let session = session_name("list-panes-area-sort");
    create_session(&handler, &session).await;

    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Pane(PaneTarget::with_window(session.clone(), 0, 0)),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::ResizePane(ResizePaneRequest {
                target: PaneTarget::with_window(session.clone(), 0, 0),
                adjustment: ResizePaneAdjustment::AbsoluteWidth { columns: 59 },
            }))
            .await,
        Response::ResizePane(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Pane(PaneTarget::with_window(session.clone(), 0, 0)),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::ResizePane(ResizePaneRequest {
                target: PaneTarget::with_window(session.clone(), 0, 0),
                adjustment: ResizePaneAdjustment::AbsoluteHeight { rows: 5 },
            }))
            .await,
        Response::ResizePane(_)
    ));

    let list = |reversed| {
        Request::ListPanes(Box::new(ListPanesRequest {
            target: session.clone(),
            target_window_index: Some(0),
            format: Some("#{pane_index}:#{pane_width}x#{pane_height}".to_owned()),
            filter: None,
            sort_order: Some("size".to_owned()),
            reversed,
        }))
    };

    assert_eq!(
        list_stdout(handler.handle(list(false)).await),
        "0:59x5\n2:20x24\n1:59x18\n"
    );
    assert_eq!(
        list_stdout(handler.handle(list(true)).await),
        "1:59x18\n2:20x24\n0:59x5\n"
    );
}

async fn wait_for_dead_pane(
    handler: &RequestHandler,
    session_name: &SessionName,
    window_index: u32,
    pane_index: u32,
) {
    let deadline = tokio::time::Instant::now() + SERVER_SIGNAL_LIVENESS;
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
    let deadline = tokio::time::Instant::now() + SERVER_SIGNAL_LIVENESS;
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
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
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
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    crate::session_fixture::assert_new_session(&created);
    handler
        .wait_for_pane_terminal_for_test(&PaneTarget::new(alpha.clone(), 0))
        .await;

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
        .handle(Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: Some(vec![split_secret.clone()]),
            command: Some(vec![split_command.clone()]),
            process_command: None,
            start_directory: None,
            keep_alive_on_exit: None,
            detached: false,
            size: None,
            preserve_zoom: false,
            full_size: false,
            stdin_payload: None,
        })))
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
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: alpha.clone(),
            target_window_index: None,
            format: Some(list_format.clone()),
            filter: None,
            sort_order: None,
            reversed: false,
        })))
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
        .handle(Request::ListWindows(Box::new(ListWindowsRequest {
            target: alpha.clone(),
            format: Some(list_format),
            filter: None,
            sort_order: None,
            reversed: false,
        })))
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
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(vec!["exit 7".to_owned()]),
            process_command: None,
        })))
        .await;
    assert!(matches!(dead_respawn, rmux_proto::Response::RespawnPane(_)));
    wait_for_dead_pane(&handler, &alpha, 0, 0).await;
    let (dead_generation, dead_output_sequence) =
        wait_for_lifecycle_exit(&handler, initial_pane_id, 7).await;

    let respawned = handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            kill: true,
            start_directory: Some(respawn_cwd.clone()),
            environment: Some(vec![respawn_secret.clone()]),
            command: Some(vec![respawn_command.clone()]),
            process_command: None,
        })))
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
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
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
            filter: None,
            sort_order: None,
            reversed: false,
        })))
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
        .handle(Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
            command: Some(vec![cwd_probe_command(&output)]),
            process_command: None,
            start_directory: Some(cwd.clone()),
            keep_alive_on_exit: None,
            detached: false,
            size: None,
            preserve_zoom: false,
            full_size: false,
            stdin_payload: None,
        })))
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
async fn detached_split_with_zoom_keeps_original_pane_zoomed() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha-detached-split-zoom");
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );

    let response = handler
        .handle(Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
            target: SplitWindowTarget::Pane(PaneTarget::with_window(alpha.clone(), 0, 0)),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
            command: None,
            process_command: None,
            start_directory: None,
            keep_alive_on_exit: None,
            detached: true,
            size: None,
            preserve_zoom: true,
            full_size: false,
            stdin_payload: None,
        })))
        .await;
    let new_pane = match response {
        rmux_proto::Response::SplitWindow(response) => response.pane,
        other => panic!("expected split-window success, got {other:?}"),
    };

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    let window = session.window_at(0).expect("window exists");
    assert!(window.is_zoomed());
    assert_eq!(window.active_pane_index(), 0);
    assert_eq!(new_pane.pane_index(), 1);
}

#[tokio::test]
async fn split_window_stdin_synchronized_output_discard_allows_later_fail_open_wake() {
    let handler = RequestHandler::new();
    let alpha = session_name("split-stdin-sync-timer-rearm");
    create_session(&handler, &alpha).await;
    let response = handler
        .handle(Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
            command: Some(vec![pipe_discard_command()]),
            process_command: None,
            start_directory: None,
            keep_alive_on_exit: None,
            detached: true,
            size: None,
            preserve_zoom: false,
            full_size: false,
            stdin_payload: Some(b"\x1b[?2026h\x1b[2J\x1b[HSPLIT_WINDOW_STDIN_PARTIAL".to_vec()),
        })))
        .await;
    let split_target = match response {
        Response::SplitWindow(response) => response.pane,
        other => panic!("expected split-window success, got {other:?}"),
    };
    handler
        .wait_for_pane_startup_to_finish_for_test(&split_target)
        .await;
    let (transcript, output) = {
        let state = handler.state.lock().await;
        (
            state
                .transcript_handle(&split_target)
                .expect("split pane transcript"),
            state
                .pane_output_for_target(
                    split_target.session_name(),
                    split_target.window_index(),
                    split_target.pane_index(),
                )
                .expect("split pane output"),
        )
    };
    let mut observer = output.subscribe();

    crate::pane_io::publish_pane_bytes_for_test(
        &transcript,
        &output,
        b"\x1b[?2026l\x1b[?2026h\x1b[2J\x1b[HLATER_CHILD_PARTIAL".to_vec(),
    );

    let invalidation = timeout(Duration::from_secs(3), async {
        loop {
            if let crate::pane_io::PaneObservationItem::Invalidated(invalidation) =
                observer.recv_observed().await
            {
                if invalidation.reason == crate::pane_io::PaneInvalidationReason::ParserStateExpired
                {
                    break invalidation;
                }
            }
        }
    })
    .await
    .expect("later synchronized admission must arm a one-second fail-open wake");
    assert_eq!(
        invalidation.reason,
        crate::pane_io::PaneInvalidationReason::ParserStateExpired
    );
    assert_eq!(
        transcript.lock().mode() & rmux_core::input::mode::MODE_SYNC,
        0,
        "the timed wake must release synchronized presentation"
    );
}

#[tokio::test]
async fn detached_split_targeting_inactive_pane_preserves_active_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("detached-split-preserves-active-pane");
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
            .handle(Request::SelectPane(Box::new(SelectPaneRequest {
                target: PaneTarget::with_window(alpha.clone(), 0, 0),
                title: None,
                style: None,
                input_disabled: None,
                preserve_zoom: false,
            })))
            .await,
        rmux_proto::Response::SelectPane(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
                target: SplitWindowTarget::Pane(PaneTarget::with_window(alpha.clone(), 0, 1)),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
                command: None,
                process_command: None,
                start_directory: None,
                keep_alive_on_exit: None,
                detached: true,
                size: None,
                preserve_zoom: false,
                full_size: false,
                stdin_payload: None,
            })))
            .await,
        rmux_proto::Response::SplitWindow(_)
    ));

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(session.active_pane_index(), 0);
}

#[tokio::test]
async fn split_window_rolls_back_session_when_spawn_fails() {
    let handler = RequestHandler::new();
    let alpha = session_name("split-spawn-fails");
    create_session(&handler, &alpha).await;
    let missing_program = unique_temp_path("missing-program");

    let response = handler
        .handle(Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
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
            detached: false,
            size: None,
            preserve_zoom: false,
            full_size: false,
            stdin_payload: None,
        })))
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
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
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
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    crate::session_fixture::assert_new_session(&created);

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
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: session_name.clone(),
            target_window_index: Some(0),
            format: Some("#{pane_output_sequence}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        })))
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
async fn move_pane_same_window_keeps_last_when_moving_the_active_pane_like_tmux() {
    // Oracle probe 2026-07-10, pinned tmux 3.7b: moving active %2 next to
    // %0 keeps %2 active and the pre-move %1 as last-pane.
    let handler = RequestHandler::new();
    let alpha = session_name("move-active-keeps-last");
    create_three_pane_window(&handler, &alpha).await;
    let selection_before = active_and_last_pane_ids(&handler, &alpha).await;

    let response = handler
        .handle(Request::MovePane(MovePaneRequest {
            source: PaneTarget::with_window(alpha.clone(), 0, 2),
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            direction: SplitDirection::Vertical,
            detached: false,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;

    assert!(matches!(response, Response::MovePane(_)), "{response:?}");
    assert_eq!(
        active_and_last_pane_ids(&handler, &alpha).await,
        selection_before
    );
}

#[tokio::test]
async fn join_pane_same_window_keeps_last_when_moving_the_active_pane_like_tmux() {
    // Same oracle matrix as move-pane: join-pane keeps active %2 and last %1.
    let handler = RequestHandler::new();
    let alpha = session_name("join-active-keeps-last");
    create_three_pane_window(&handler, &alpha).await;
    let selection_before = active_and_last_pane_ids(&handler, &alpha).await;

    let response = handler
        .handle(Request::JoinPane(rmux_proto::JoinPaneRequest {
            source: PaneTarget::with_window(alpha.clone(), 0, 2),
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            direction: SplitDirection::Vertical,
            detached: false,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;

    assert!(matches!(response, Response::JoinPane(_)), "{response:?}");
    assert_eq!(
        active_and_last_pane_ids(&handler, &alpha).await,
        selection_before
    );
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
        state
            .sessions
            .session_setup(&alpha)
            .expect("session exists")
            .insert_window_with_initial_pane(1, TerminalSize { cols: 80, rows: 24 })
            .expect("window insert succeeds");
        state
            .insert_window_terminal(
                &alpha,
                1,
                crate::pane_terminals::WindowSpawnOptions {
                    start_directory: None,
                    command: None,
                    socket_path: Path::new("/tmp/rmux-test.sock"),
                    spawn_environment: None,
                    environment_overrides: None,
                    respawn_shell: None,
                    respawn_environment: None,
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

    // FMT1 R2: `-b` picks the side of the split, never the pane list index.
    // tmux reinserts the moved pane right after its destination, so it lands at
    // index 1 behind the destination's 0. Receipt:
    // `fmt1/oracle-linux/tiled-source-cross-window-before`.
    assert_eq!(
        response,
        rmux_proto::Response::MovePane(rmux_proto::MovePaneResponse {
            target: PaneTarget::with_window(alpha.clone(), 1, 1),
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
async fn join_pane_exits_attached_source_session_when_source_is_removed() {
    let handler = RequestHandler::new();
    let alpha = session_name("join-remove-alpha");
    let beta = session_name("join-remove-beta");
    create_session(&handler, &alpha).await;
    create_session(&handler, &beta).await;
    let mut control_rx = attach_to_existing_session(&handler, 70_001, &alpha).await;

    let response = handler
        .handle(Request::JoinPane(rmux_proto::JoinPaneRequest {
            source: PaneTarget::with_window(alpha.clone(), 0, 0),
            target: PaneTarget::with_window(beta.clone(), 0, 0),
            direction: SplitDirection::Vertical,
            detached: false,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;

    assert!(
        matches!(response, Response::JoinPane(_)),
        "expected successful join-pane, got {response:?}"
    );
    wait_for_attached_session_exit(&mut control_rx).await;
}

#[tokio::test]
async fn break_pane_exits_attached_source_session_when_source_is_removed() {
    let handler = RequestHandler::new();
    let alpha = session_name("break-remove-alpha");
    let beta = session_name("break-remove-beta");
    create_session(&handler, &alpha).await;
    create_session(&handler, &beta).await;
    let mut control_rx = attach_to_existing_session(&handler, 70_002, &alpha).await;

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(alpha.clone(), 0, 0),
            target: Some(WindowTarget::with_window(beta.clone(), 1)),
            name: None,
            detached: false,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;

    assert!(
        matches!(response, Response::BreakPane(_)),
        "expected successful break-pane, got {response:?}"
    );
    wait_for_attached_session_exit(&mut control_rx).await;
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
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(std::process::id(), alpha.clone(), control_tx)
        .await;

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(alpha.clone(), 0, 1),
            target: Some(WindowTarget::with_window(alpha.clone(), 1)),
            name: None,
            detached: true,
            after: false,
            before: false,
            print_target: true,
            format: Some("#{window_index}.#{pane_index}:#{L:#{client_name}}".to_owned()),
        })))
        .await;

    let rmux_proto::Response::BreakPane(success) = response else {
        panic!("expected break-pane response");
    };
    let output = success.command_output().expect("break-pane -P output");
    assert_eq!(
        output.stdout(),
        format!(
            "1.0:{}\n",
            crate::client_names::attached_client_name(std::process::id())
        )
        .as_bytes()
    );
}

#[tokio::test]
async fn break_pane_implicit_destination_starts_at_base_index() {
    let handler = RequestHandler::new();
    let alpha = session_name("break-base-index");

    let set_base_index = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::BaseIndex,
            value: "1".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_base_index, Response::SetOption(_)));
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
        Response::SplitWindow(_)
    ));

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(alpha.clone(), 1, 1),
            target: None,
            name: None,
            detached: true,
            after: false,
            before: false,
            print_target: true,
            format: Some("#{window_index}.#{pane_index}".to_owned()),
        })))
        .await;

    let Response::BreakPane(success) = response else {
        panic!("expected break-pane response, got {response:?}");
    };
    assert_eq!(
        success
            .command_output()
            .expect("break-pane -P output")
            .stdout(),
        b"2.0\n"
    );
    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session survives");
    assert_eq!(
        session
            .windows()
            .map(|(index, _)| index)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[tokio::test]
async fn break_pane_print_target_refreshes_automatic_window_name() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha-break-name");
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
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(alpha.clone(), 0, 1),
            target: Some(WindowTarget::with_window(alpha.clone(), 1)),
            name: None,
            detached: true,
            after: false,
            before: false,
            print_target: true,
            format: Some("#{window_name}:#{pane_current_command}".to_owned()),
        })))
        .await;

    let rmux_proto::Response::BreakPane(success) = response else {
        panic!("expected break-pane response");
    };
    let output = success.command_output().expect("break-pane -P output");
    let output = String::from_utf8(output.stdout().to_vec()).expect("utf-8 output");
    let (window_name, current_command) = output
        .trim_end()
        .split_once(':')
        .expect("window name and current command");
    assert!(
        !window_name.is_empty(),
        "break-pane -P should expose a tmux-style automatic window name, got {output:?}"
    );
    assert!(
        !current_command.is_empty(),
        "pane_current_command sanity check, got {output:?}"
    );
}

#[tokio::test]
async fn attached_input_to_dead_kept_pane_does_not_fail_liveness_probe() {
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
        Response::SetOption(_)
    ));

    let respawned = handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(vec!["exit 0".to_owned()]),
            process_command: None,
        })))
        .await;
    assert!(matches!(respawned, Response::RespawnPane(_)));
    wait_for_dead_pane(&handler, &alpha, 0, 0).await;

    let requester_pid = 44_200_u32;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let result = handler
        .handle_attached_live_input_for_test(requester_pid, b"x")
        .await;
    assert!(
        !matches!(&result, Err(error) if error.to_string().contains("target pane has exited")),
        "attached input must not use child-process liveness as a pane liveness gate: {result:?}"
    );
}

#[tokio::test]
async fn respawn_pane_without_command_restarts_dead_remain_on_exit_workload() {
    let handler = RequestHandler::new();
    let alpha = session_name("respawn-dead-provenance");
    create_session(&handler, &alpha).await;
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Pane(target.clone()),
                option: OptionName::RemainOnExit,
                value: "on".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));

    let explicit = handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: Some(ProcessCommand::Shell("exit 7".to_owned())),
        })))
        .await;
    assert!(matches!(explicit, Response::RespawnPane(_)), "{explicit:?}");
    wait_for_dead_pane(&handler, &alpha, 0, 0).await;
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.pane_id_in_window(0, 0))
            .expect("dead pane exists")
    };
    let (first_generation, _) = wait_for_lifecycle_exit(&handler, pane_id, 7).await;

    let inherited = handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target,
            kill: false,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
        })))
        .await;
    assert!(
        matches!(inherited, Response::RespawnPane(_)),
        "{inherited:?}"
    );
    wait_for_dead_pane(&handler, &alpha, 0, 0).await;
    let (second_generation, _) = wait_for_lifecycle_exit(&handler, pane_id, 7).await;
    assert!(
        second_generation > first_generation,
        "command-less respawn must run and exit the original workload again"
    );
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
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(vec!["exit 0".to_owned()]),
            process_command: None,
        })))
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
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: PaneTarget::with_window(alpha, 0, 0),
            kill: false,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
        })))
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
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            kill: true,
            start_directory: Some(cwd.clone()),
            environment: Some(vec!["RMUX_RESPAWN=ready".to_owned()]),
            command: Some(vec![respawn_probe_command(&output)]),
            process_command: None,
        })))
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

const NUL_ENVIRONMENT: &str = "RMUX_NUL_WITNESS=bad\0value";
const VALID_ENVIRONMENT: &str = "RMUX_NUL_WITNESS=valid";

/// Asserts the refusal is the spawn boundary rejecting a NUL, not any error.
///
/// The two platforms word it differently - std says "nul byte found in provided
/// data", the Windows backend says "interior NUL byte in process configuration"
/// - so the shared, causal part is the typed variant plus the mention of NUL.
fn assert_nul_spawn_refusal(response: &Response, context: &str) {
    let Response::Error(error) = response else {
        panic!("{context}: expected a refusal, got {response:?}");
    };
    let rmux_proto::RmuxError::SpawnFailed { message } = &error.error else {
        panic!("{context}: expected SpawnFailed, got {:?}", error.error);
    };
    assert!(
        message.to_lowercase().contains("nul"),
        "{context}: the refusal must name the NUL that caused it, got {message:?}"
    );
}

async fn pane_identity(
    handler: &RequestHandler,
    session: &SessionName,
) -> (rmux_proto::PaneId, Option<u32>, u64) {
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    let state = handler.state.lock().await;
    let pane_id = state
        .sessions
        .session(session)
        .expect("session exists")
        .window_at(0)
        .expect("window exists")
        .pane(0)
        .expect("pane exists")
        .id();
    let pid = state.pane_pid_in_window(session, 0, 0).ok();
    let generation = state.pane_output_generation_for_target(&target, pane_id);
    (pane_id, pid, generation)
}

async fn send_pane_input(handler: &RequestHandler, session: &SessionName, bytes: &str) {
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(session)
            .expect("session exists")
            .window_at(0)
            .expect("window exists")
            .pane(0)
            .expect("pane exists")
            .id()
    };
    let response = handler
        .handle(Request::PaneInput(PaneInputRequest {
            target: PaneTargetRef::by_id(session.clone(), pane_id),
            keys: vec![bytes.to_owned()],
            literal: true,
        }))
        .await;
    assert!(
        matches!(response, Response::SendKeys(_)),
        "pane input must reach the child, got {response:?}"
    );
}

#[tokio::test]
async fn respawn_pane_rejects_an_interior_nul_and_the_same_child_keeps_reading() {
    // NUL-R2-RESPAWN-WITNESS. A bare pane count and any Error would also pass on
    // a refusal unrelated to NUL, or after the old runtime was replaced. So the
    // pane runs a controlled child that signals READY and reads an exact byte
    // sequence, and that sequence is split across the refused respawn: only the
    // original child can accumulate both halves.
    let handler = RequestHandler::new();
    let alpha = session_name("respawn-nul-continuity");
    let slot = FinalSinkSlot::new("respawn-nul", b"BEFORE\nAFTER\n", false);

    let created = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
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
            command: None,
            process_command: Some(ProcessCommand::Argv(slot.pane_command())),
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    crate::session_fixture::assert_new_session(&created);

    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    // READY proves a live child, not a pane still being born. This witness is
    // deliberately about a running runtime; the Starting cycle keeps its own
    // contract and is not exercised here.
    slot.wait_until_ready();

    send_pane_input(&handler, &alpha, "BEFORE\n").await;
    let before = pane_identity(&handler, &alpha).await;
    assert!(before.1.is_some(), "the witness needs a live child pid");

    let response = handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: Some(vec![NUL_ENVIRONMENT.to_owned()]),
            command: None,
            process_command: None,
        })))
        .await;
    assert_nul_spawn_refusal(&response, "respawn with an interior NUL");

    let after = pane_identity(&handler, &alpha).await;
    assert_eq!(
        after, before,
        "the refused respawn must not replace the runtime"
    );

    // The decisive observation: the same child, not a replacement, completes the
    // capture it had already started.
    send_pane_input(&handler, &alpha, "AFTER\n").await;
    slot.assert_application_bytes("the child that survived a refused respawn must keep reading");
}

#[tokio::test]
async fn new_window_rejects_an_interior_nul_and_restores_the_session_snapshot() {
    // NUL-R2-NEW-WINDOW-WITNESS. This boundary captures a session snapshot,
    // creates, selects and remaps before spawning, then restores on error. The
    // insertion index is occupied so the refused request exercises the remap
    // the snapshot is there to undo.
    let handler = RequestHandler::new();
    let alpha = session_name("new-window-nul-snapshot");
    create_session(&handler, &alpha).await;
    let second = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha.clone(),
            name: Some("occupant".to_owned()),
            detached: false,
            environment: None,
            command: None,
            start_directory: None,
            target_window_index: None,
            insert_at_target: false,
            process_command: None,
        })))
        .await;
    assert!(matches!(second, Response::NewWindow(_)), "{second:?}");

    async fn snapshot(
        handler: &RequestHandler,
        session: &SessionName,
    ) -> (Vec<(u32, u32, Vec<rmux_proto::PaneId>)>, u32, String) {
        let state = handler.state.lock().await;
        let owner = state.sessions.session(session).expect("session exists");
        let windows = owner
            .windows()
            .map(|(index, window)| {
                (
                    index,
                    window.id().as_u32(),
                    window.panes().iter().map(rmux_core::Pane::id).collect(),
                )
            })
            .collect();
        // The options are compared by their rendered value so this witness does
        // not depend on the concrete type staying nameable from here.
        (
            windows,
            owner.active_window_index(),
            format!("{:?}", state.options),
        )
    }

    let before = snapshot(&handler, &alpha).await;

    let refused = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha.clone(),
            name: Some("refused".to_owned()),
            detached: false,
            environment: Some(vec![NUL_ENVIRONMENT.to_owned()]),
            command: None,
            start_directory: None,
            target_window_index: Some(1),
            insert_at_target: true,
            process_command: None,
        })))
        .await;
    assert_nul_spawn_refusal(&refused, "new-window with an interior NUL");

    let after = snapshot(&handler, &alpha).await;
    assert_eq!(
        after, before,
        "the refused new-window must restore windows, pane identities, selection and options"
    );

    // The control: the same request with a valid value creates the window, so
    // the refusal above is the NUL and not the shape of the request.
    let accepted = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha.clone(),
            name: Some("accepted".to_owned()),
            detached: false,
            environment: Some(vec![VALID_ENVIRONMENT.to_owned()]),
            command: None,
            start_directory: None,
            target_window_index: Some(1),
            insert_at_target: true,
            process_command: None,
        })))
        .await;
    assert!(matches!(accepted, Response::NewWindow(_)), "{accepted:?}");
    let created = snapshot(&handler, &alpha).await;
    assert_eq!(
        created.0.len(),
        before.0.len() + 1,
        "a valid value must still create the window"
    );
}

#[tokio::test]
async fn respawn_pane_reuses_structured_command_cwd_and_private_environment() {
    let handler = RequestHandler::new();
    let alpha = session_name("respawn-pane-provenance");
    let initial_cwd = unique_temp_path("respawn-pane-provenance-initial");
    let override_cwd = unique_temp_path("respawn-pane-provenance-override");
    let output = unique_temp_path("respawn-pane-provenance-output");
    fs::create_dir_all(&initial_cwd).expect("initial respawn cwd");
    fs::create_dir_all(&override_cwd).expect("override respawn cwd");
    let initial_argv = respawn_argv_probe_command(&output, "argv");
    let initial_process_command = ProcessCommand::Argv(initial_argv.clone());
    let initial_environment = "RMUX_RESPAWN=initial".to_owned();

    let created = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(alpha.clone()),
            working_directory: Some(initial_cwd.to_string_lossy().into_owned()),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: Some(vec![initial_environment.clone()]),
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: Some(initial_process_command.clone()),
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    crate::session_fixture::assert_new_session(&created);

    let initial_cwd_text = expected_spawn_cwd(&initial_cwd);
    let initial_line = respawn_probe_line(&initial_cwd_text, "initial", "argv");
    wait_for_file_contents(&output, &initial_line).await;
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    let pane_id = {
        let state = handler.state.lock().await;
        let pane_id = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.pane_id_in_window(0, 0))
            .expect("initial pane exists");
        let lifecycle = state.pane_lifecycle(pane_id).expect("initial lifecycle");
        assert_eq!(lifecycle.process_command(), Some(&initial_process_command));
        assert_eq!(
            lifecycle.respawn_environment(),
            std::slice::from_ref(&initial_environment)
        );
        pane_id
    };

    let inherited = handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
        })))
        .await;
    assert!(
        matches!(inherited, Response::RespawnPane(_)),
        "{inherited:?}"
    );
    wait_for_file_contents(&output, &format!("{initial_line}{initial_line}")).await;

    let override_environment = "RMUX_RESPAWN=override".to_owned();
    let explicit_shell = respawn_shell_probe_command(&output, "shell");
    let explicit_process_command = ProcessCommand::Shell(explicit_shell);
    let explicit = handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: Some(override_cwd.clone()),
            environment: Some(vec![override_environment.clone()]),
            command: None,
            process_command: Some(explicit_process_command.clone()),
        })))
        .await;
    assert!(matches!(explicit, Response::RespawnPane(_)), "{explicit:?}");
    let override_cwd_text = expected_spawn_cwd(&override_cwd);
    let override_line = respawn_probe_line(&override_cwd_text, "override", "shell");
    wait_for_file_contents(
        &output,
        &format!("{initial_line}{initial_line}{override_line}"),
    )
    .await;
    {
        let state = handler.state.lock().await;
        let lifecycle = state.pane_lifecycle(pane_id).expect("override lifecycle");
        assert_eq!(lifecycle.process_command(), Some(&explicit_process_command));
        assert_eq!(
            lifecycle.private_environment(),
            std::slice::from_ref(&override_environment)
        );
        assert_eq!(
            lifecycle.respawn_environment(),
            std::slice::from_ref(&initial_environment)
        );
    }

    let inherited_after_override = handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target,
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
        })))
        .await;
    assert!(
        matches!(inherited_after_override, Response::RespawnPane(_)),
        "{inherited_after_override:?}"
    );
    let inherited_override_line = respawn_probe_line(&override_cwd_text, "initial", "shell");
    wait_for_file_contents(
        &output,
        &format!("{initial_line}{initial_line}{override_line}{inherited_override_line}"),
    )
    .await;

    drop(handler);
    let _ = fs::remove_file(output);
    let _ = fs::remove_dir_all(initial_cwd);
    let _ = fs::remove_dir_all(override_cwd);
}

#[cfg(unix)]
#[tokio::test]
async fn respawn_pane_keeps_the_original_resolved_shell_after_option_changes() {
    let handler = RequestHandler::new();
    let alpha = session_name("respawn-pane-shell-provenance");
    let output = unique_temp_path("respawn-pane-shell-provenance-output");
    set_global_default_shell(&handler, "/bin/sh").await;
    create_session(&handler, &alpha).await;
    handler.wait_for_initial_panes_for_test().await;
    set_global_default_shell(&handler, "/bin/bash").await;

    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    let shell_command = respawn_shell_identity_command(&output, "shell");
    let explicit = handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: Some(ProcessCommand::Shell(shell_command)),
        })))
        .await;
    assert!(matches!(explicit, Response::RespawnPane(_)), "{explicit:?}");
    let expected_line = "sh:/bin/sh:shell\n";
    wait_for_file_contents(&output, expected_line).await;

    let inherited = handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target,
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
        })))
        .await;
    assert!(
        matches!(inherited, Response::RespawnPane(_)),
        "{inherited:?}"
    );
    wait_for_file_contents(&output, &format!("{expected_line}{expected_line}")).await;

    let state = handler.state.lock().await;
    let pane_id = state
        .sessions
        .session(&alpha)
        .and_then(|session| session.pane_id_in_window(0, 0))
        .expect("respawned pane exists");
    assert_eq!(
        state
            .pane_lifecycle(pane_id)
            .expect("respawned pane lifecycle")
            .respawn_shell(),
        Path::new("/bin/sh")
    );
    drop(state);
    drop(handler);
    let _ = fs::remove_file(output);
}

#[tokio::test]
async fn respawn_pane_with_kill_flag_does_not_emit_pane_exited_like_tmux() {
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
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(vec![pipe_discard_command()]),
            process_command: None,
        })))
        .await;
    assert!(matches!(response, rmux_proto::Response::RespawnPane(_)));

    assert_no_pane_exited_event(&mut lifecycle_events, "forced respawn").await;

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
async fn pane_id_kill_does_not_emit_pane_exited_like_tmux() {
    let handler = RequestHandler::new();
    let alpha = session_name("pane-id-kill-exit");
    create_session(&handler, &alpha).await;
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)));
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(1))
            .expect("second pane exists")
            .id()
    };
    let mut lifecycle_events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::PaneKill(PaneKillRequest {
            target: PaneTargetRef::by_id(alpha, pane_id),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)));

    assert_no_pane_exited_event(&mut lifecycle_events, "stable pane-id kill").await;
}

#[tokio::test]
async fn pane_id_respawn_with_kill_flag_does_not_emit_pane_exited_like_tmux() {
    let handler = RequestHandler::new();
    let alpha = session_name("pane-id-respawn-exit");
    create_session(&handler, &alpha).await;
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
        .handle(Request::PaneRespawn(Box::new(PaneRespawnRequest {
            target: PaneTargetRef::by_id(alpha.clone(), pane_id),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(vec![pipe_discard_command()]),
            process_command: None,
            keep_alive_on_exit: None,
        })))
        .await;
    assert!(matches!(response, Response::RespawnPane(_)));

    assert_no_pane_exited_event(&mut lifecycle_events, "stable pane-id respawn").await;

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

async fn assert_no_pane_exited_event(
    lifecycle_events: &mut tokio::sync::broadcast::Receiver<super::QueuedLifecycleEvent>,
    context: &str,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(150);
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        match timeout(deadline - now, lifecycle_events.recv()).await {
            Ok(Ok(event)) => assert_ne!(
                event.hook_name,
                HookName::PaneExited,
                "{context} must not synthesize pane-exited: {event:?}"
            ),
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) | Err(_) => break,
        }
    }
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
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(vec![pipe_discard_command()]),
            process_command: None,
        })))
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
            .handle(Request::SelectPane(Box::new(SelectPaneRequest {
                target: PaneTarget::with_window(alpha.clone(), 0, 0),
                title: None,
                style: None,
                input_disabled: None,
                preserve_zoom: false,
            })))
            .await,
        rmux_proto::Response::SelectPane(_)
    ));

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let response = handler
        .handle(Request::DisplayPanes(Box::new(DisplayPanesRequest {
            target: alpha.clone(),
            duration_ms: Some(5_000),
            non_blocking: true,
            no_command: false,
            template: None,
            target_client: None,
        })))
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
async fn display_panes_default_template_runs_select_pane_hooks() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = 4242_u32;
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
            .handle(Request::SelectPane(Box::new(SelectPaneRequest {
                target: PaneTarget::with_window(alpha.clone(), 0, 0),
                title: None,
                style: None,
                input_disabled: None,
                preserve_zoom: false,
            })))
            .await,
        rmux_proto::Response::SelectPane(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SetHookMutation(SetHookMutationRequest {
                scope: ScopeSelector::Global,
                hook: HookName::AfterSelectPane,
                command: Some("set-buffer -b display-panes-probe fired".to_owned()),
                lifecycle: HookLifecycle::Persistent,
                append: false,
                unset: false,
                run_immediately: false,
                index: None,
            }))
            .await,
        rmux_proto::Response::SetHook(_)
    ));

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let response = handler
        .handle(Request::DisplayPanes(Box::new(DisplayPanesRequest {
            target: alpha.clone(),
            duration_ms: Some(5_000),
            non_blocking: true,
            no_command: false,
            template: None,
            target_client: None,
        })))
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
    let (_, buffer) = state
        .buffers
        .show(Some("display-panes-probe"))
        .expect("after-select-pane hook should write the probe buffer");
    assert_eq!(String::from_utf8_lossy(buffer), "fired");
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
            .handle(Request::SelectPane(Box::new(SelectPaneRequest {
                target: PaneTarget::with_window(alpha.clone(), 0, 0),
                title: None,
                style: None,
                input_disabled: None,
                preserve_zoom: false,
            })))
            .await,
        rmux_proto::Response::SelectPane(_)
    ));

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let response = handler
        .handle(Request::DisplayPanes(Box::new(DisplayPanesRequest {
            target: alpha.clone(),
            duration_ms: Some(5_000),
            non_blocking: true,
            no_command: true,
            template: None,
            target_client: None,
        })))
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
async fn display_panes_masks_and_restores_native_pane_focus() {
    let handler = RequestHandler::new();
    let alpha = session_name("display-panes-focus-mask");
    let requester_pid = 4_243_u32;
    create_session(&handler, &alpha).await;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let target = PaneTarget::new(alpha.clone(), 0);
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?1004h")
            .expect("pane enables focus reporting");
        state.start_pane_input_capture_for_test(&target);
    }

    let response = handler
        .handle(Request::DisplayPanes(Box::new(DisplayPanesRequest {
            target: alpha.clone(),
            duration_ms: Some(5_000),
            non_blocking: true,
            no_command: true,
            template: None,
            target_client: None,
        })))
        .await;
    assert!(
        matches!(response, Response::DisplayPanes(_)),
        "{response:?}"
    );
    let _overlay = control_rx.recv().await.expect("display-panes overlay");
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state.pane_input_capture_for_test(&target),
            Some(b"\x1b[O".to_vec())
        );
    }
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[O")
        .await
        .expect("display-panes passes focus-out to the attach controller");
    {
        let active_attach = handler.active_attach.lock().await;
        let active = &active_attach.by_pid[&requester_pid];
        assert!(!active.reported_focused);
        assert!(
            active.display_panes.is_some(),
            "focus-out must not close display-panes"
        );
    }
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b[I")
        .await
        .expect("display-panes passes focus-in to the attach controller");
    {
        let active_attach = handler.active_attach.lock().await;
        let active = &active_attach.by_pid[&requester_pid];
        assert!(active.reported_focused);
        assert!(
            active.display_panes.is_some(),
            "focus-in must not close display-panes"
        );
    }
    handler
        .handle_attached_live_input_for_test(requester_pid, b"0")
        .await
        .expect("display-panes closes on its active pane label");
    let _clear = control_rx
        .recv()
        .await
        .expect("display-panes clear overlay");
    let state = handler.state.lock().await;
    assert_eq!(
        state.pane_input_capture_for_test(&target),
        Some(b"\x1b[O\x1b[I".to_vec())
    );
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
        .handle(Request::DisplayPanes(Box::new(DisplayPanesRequest {
            target: alpha.clone(),
            duration_ms: None,
            non_blocking: true,
            no_command: true,
            template: None,
            target_client: None,
        })))
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
        .handle(Request::DisplayPanes(Box::new(DisplayPanesRequest {
            target: alpha.clone(),
            duration_ms: Some(25),
            non_blocking: true,
            no_command: true,
            template: None,
            target_client: None,
        })))
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
async fn cross_session_swap_from_group_owner_preserves_owner_and_session_pair() {
    let handler = RequestHandler::new();
    let owner = session_name("swap-owner-success");
    let peer = session_name("swap-peer-success");
    let target = session_name("swap-target-success");
    create_session(&handler, &owner).await;
    create_grouped_session(&handler, &peer, &owner).await;
    create_session(&handler, &target).await;
    let (source_pane_id, target_pane_id) = {
        let state = handler.state.lock().await;
        (
            state
                .sessions
                .session(&owner)
                .and_then(|session| session.pane_id_in_window(0, 0))
                .expect("source pane exists"),
            state
                .sessions
                .session(&target)
                .and_then(|session| session.pane_id_in_window(0, 0))
                .expect("target pane exists"),
        )
    };

    let response = handler
        .handle(Request::SwapPane(rmux_proto::SwapPaneRequest {
            source: PaneTarget::with_window(owner.clone(), 0, 0),
            target: PaneTarget::with_window(target.clone(), 0, 0),
            direction: None,
            detached: false,
            preserve_zoom: false,
        }))
        .await;

    assert!(matches!(response, Response::SwapPane(_)), "{response:?}");
    let state = handler.state.lock().await;
    assert_eq!(state.sessions.runtime_owner(&owner), Some(owner.clone()));
    assert_eq!(state.sessions.runtime_owner(&peer), Some(owner.clone()));
    assert_eq!(
        state
            .sessions
            .session(&owner)
            .and_then(|session| session.pane_id_in_window(0, 0)),
        Some(target_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&peer)
            .and_then(|session| session.pane_id_in_window(0, 0)),
        Some(target_pane_id)
    );
    assert_eq!(
        state
            .sessions
            .session(&target)
            .and_then(|session| session.pane_id_in_window(0, 0)),
        Some(source_pane_id)
    );
}

#[tokio::test]
async fn cross_session_swap_rollback_restores_owner_and_session_pair() {
    let handler = RequestHandler::new();
    let owner = session_name("swap-owner-rollback");
    let peer = session_name("swap-peer-rollback");
    let target = session_name("swap-target-rollback");
    create_session(&handler, &owner).await;
    create_grouped_session(&handler, &peer, &owner).await;
    create_session(&handler, &target).await;
    handler.wait_for_initial_panes_for_test().await;
    let (owner_before, peer_before, target_before) = {
        let mut state = handler.state.lock().await;
        let snapshots = (
            state
                .sessions
                .session(&owner)
                .expect("owner exists")
                .test_snapshot(),
            state
                .sessions
                .session(&peer)
                .expect("peer exists")
                .test_snapshot(),
            state
                .sessions
                .session(&target)
                .expect("target exists")
                .test_snapshot(),
        );
        state.fail_next_resize_for_test();
        snapshots
    };

    let response = handler
        .handle(Request::SwapPane(rmux_proto::SwapPaneRequest {
            source: PaneTarget::with_window(owner.clone(), 0, 0),
            target: PaneTarget::with_window(target.clone(), 0, 0),
            direction: None,
            detached: false,
            preserve_zoom: false,
        }))
        .await;

    assert!(
        matches!(&response, Response::Error(error) if error.error == rmux_proto::RmuxError::Server("injected pane terminal resize failure".to_owned())),
        "expected injected resize failure, got {response:?}"
    );
    let state = handler.state.lock().await;
    assert_eq!(state.sessions.runtime_owner(&owner), Some(owner.clone()));
    assert_eq!(state.sessions.runtime_owner(&peer), Some(owner.clone()));
    assert_eq!(
        state
            .sessions
            .session(&owner)
            .map(|session| session.test_snapshot()),
        Some(owner_before)
    );
    assert_eq!(
        state
            .sessions
            .session(&peer)
            .map(|session| session.test_snapshot()),
        Some(peer_before)
    );
    assert_eq!(
        state
            .sessions
            .session(&target)
            .map(|session| session.test_snapshot()),
        Some(target_before)
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

// Source-bound tmux 3.7b swap oracle: L1-034 plus L1-035's six extra
// zoom/source/target/-Z cases. These fixtures own real PTYs when executed.
#[derive(Clone, Copy)]
pub(super) enum FloatingSwapRoute {
    SameWindow,
    CrossWindow,
    CrossSession,
    GroupedAlias,
    GroupedCrossWindow,
    GroupedCrossSession,
}

/// Assertions for the admitted RMUX publication contract. These are not a
/// measured tmux event count/order or an attached-client parity oracle.
pub(super) async fn assert_swap_layout_publication(
    handler: &RequestHandler,
    events: &mut tokio::sync::broadcast::Receiver<super::QueuedLifecycleEvent>,
    expected: &[rmux_core::WindowId],
) {
    let state = handler.state.lock().await;
    let mut actual = Vec::new();
    loop {
        let event = match events.try_recv() {
            Ok(event) => event,
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(error) => panic!("layout event evidence lost: {error}"),
        };
        match event.event {
            rmux_core::LifecycleEvent::WindowLayoutChanged { target } => {
                actual.push(state.sessions.window(&target).unwrap().id());
            }
            rmux_core::LifecycleEvent::WindowResized { .. } => {
                panic!("swap-only layout changes must not invent a window resize");
            }
            _ => {}
        }
    }
    actual.sort_by_key(|id| id.as_u32());
    let mut expected = expected.to_vec();
    expected.sort_by_key(|id| id.as_u32());
    expected.dedup();
    assert_eq!(
        actual, expected,
        "one layout publication per changed canonical window"
    );
}

#[tokio::test]
async fn swap_refusal_publishes_only_windows_actually_unzoomed() {
    for route in [
        FloatingSwapRoute::SameWindow,
        FloatingSwapRoute::CrossWindow,
        FloatingSwapRoute::CrossSession,
    ] {
        for (source_zoom, target_zoom) in [(true, true), (false, true), (false, false)] {
            let mut fixture = floating_swap_fixture(route).await;
            fixture.zoom(source_zoom, target_zoom).await;
            let expected = {
                let state = fixture.handler.state.lock().await;
                fixture
                    .windows
                    .iter()
                    .filter_map(|(target, _)| {
                        let window = state.sessions.window(target).unwrap();
                        window.is_zoomed().then_some(window.id())
                    })
                    .collect::<Vec<_>>()
            };
            let mut events = fixture.handler.subscribe_lifecycle_events();
            let response = fixture
                .handler
                .handle(Request::SwapPane(rmux_proto::SwapPaneRequest {
                    source: fixture.source.clone(),
                    target: fixture.target.clone(),
                    direction: None,
                    detached: true,
                    preserve_zoom: true,
                }))
                .await;
            assert!(
                matches!(&response, Response::Error(error)
                if error.error == rmux_proto::RmuxError::Server("cannot swap floating panes".to_owned())),
                "{response:?}"
            );
            assert_swap_layout_publication(&fixture.handler, &mut events, &expected).await;
            fixture.assert_state_and_runtimes().await;
            fixture.cleanup().await;
        }
    }
}

#[tokio::test]
async fn directional_tiled_swap_publishes_only_the_resolved_window() {
    let mut fixture = floating_swap_fixture(FloatingSwapRoute::CrossSession).await;
    fixture.zoom(true, false).await;
    let (source_before, target_before) = {
        let state = fixture.handler.state.lock().await;
        (
            state
                .sessions
                .window(&fixture.windows[0].0)
                .unwrap()
                .clone(),
            state
                .sessions
                .window(&fixture.windows[1].0)
                .unwrap()
                .clone(),
        )
    };
    let mut events = fixture.handler.subscribe_lifecycle_events();
    // The explicit source is floating and zoomed in another session. -D
    // instead exchanges the two tiled panes in the target's window.
    let response = fixture
        .handler
        .handle(Request::SwapPane(rmux_proto::SwapPaneRequest {
            source: fixture.source.clone(),
            target: fixture.target.clone(),
            direction: Some(rmux_proto::SwapPaneDirection::Down),
            detached: true,
            preserve_zoom: false,
        }))
        .await;
    let Response::SwapPane(response) = response else {
        panic!("expected tiled swap success");
    };
    assert_eq!(response.target, fixture.target);
    assert_eq!(
        response.source,
        PaneTarget::with_window(fixture.target.session_name().clone(), 0, 1)
    );
    assert_swap_layout_publication(&fixture.handler, &mut events, &[target_before.id()]).await;
    {
        let state = fixture.handler.state.lock().await;
        assert_eq!(
            state.sessions.window(&fixture.windows[0].0).unwrap(),
            &source_before
        );
        let target = state.sessions.window(&fixture.windows[1].0).unwrap();
        assert_eq!(
            target.pane(0).unwrap().id(),
            target_before.pane(1).unwrap().id()
        );
        assert_eq!(
            target.pane(1).unwrap().id(),
            target_before.pane(0).unwrap().id()
        );
    }
    fixture.cleanup().await;
}

pub(super) async fn floating_swap_command(
    handler: &RequestHandler,
    command: &str,
) -> Result<rmux_proto::CommandOutput, rmux_proto::RmuxError> {
    handler
        .execute_parsed_commands_for_test(
            std::process::id(),
            rmux_core::command_parser::CommandParser::new()
                .parse(command)
                .unwrap(),
        )
        .await
}

pub(super) struct FloatingSwapFixture {
    pub handler: RequestHandler,
    pub source: PaneTarget,
    pub target: PaneTarget,
    pub windows: Vec<(WindowTarget, rmux_core::Window)>,
    runtimes: Vec<(rmux_core::PaneId, Option<u32>, u64)>,
}

pub(super) async fn floating_swap_fixture(route: FloatingSwapRoute) -> FloatingSwapFixture {
    let handler = RequestHandler::new();
    floating_swap_command(&handler, "set-option -g status off")
        .await
        .unwrap();
    floating_swap_command(&handler, "set-option -g automatic-rename off")
        .await
        .unwrap();
    floating_swap_command(
        &handler,
        &format!(
            "set-option -g default-command {}",
            crate::test_shell::command_quote(&pipe_discard_command())
        ),
    )
    .await
    .unwrap();
    for command in [
        "new-session -d -s swap-source -n base -x80 -y24",
        "split-window -d -h -t swap-source:0.0",
        "new-pane -d -t swap-source:0.0 -x12 -y4 -X3 -Y2",
    ] {
        floating_swap_command(&handler, command).await.unwrap();
    }
    let owner = session_name("swap-source");
    let mut windows = vec![WindowTarget::new(owner.clone())];
    let mut target = PaneTarget::new(owner.clone(), 0);
    if matches!(
        route,
        FloatingSwapRoute::CrossWindow | FloatingSwapRoute::GroupedCrossWindow
    ) {
        for command in [
            "new-window -d -t swap-source:1 -n other",
            "split-window -d -h -t swap-source:1.0",
        ] {
            floating_swap_command(&handler, command).await.unwrap();
        }
        windows.push(WindowTarget::with_window(owner.clone(), 1));
        target = PaneTarget::with_window(owner.clone(), 1, 0);
    } else if matches!(
        route,
        FloatingSwapRoute::CrossSession | FloatingSwapRoute::GroupedCrossSession
    ) {
        for command in [
            "new-session -d -s swap-target -n other -x80 -y24",
            "split-window -d -h -t swap-target:0.0",
        ] {
            floating_swap_command(&handler, command).await.unwrap();
        }
        let other = session_name("swap-target");
        windows.push(WindowTarget::new(other.clone()));
        target = PaneTarget::new(other, 0);
    }
    let mut source = PaneTarget::new(owner.clone(), 2);
    if matches!(
        route,
        FloatingSwapRoute::GroupedAlias
            | FloatingSwapRoute::GroupedCrossWindow
            | FloatingSwapRoute::GroupedCrossSession
    ) {
        let peer = session_name("swap-peer");
        create_grouped_session(&handler, &peer, &owner).await;
        windows.extend(
            windows
                .clone()
                .into_iter()
                .filter(|window| window.session_name() == &owner)
                .map(|window| WindowTarget::with_window(peer.clone(), window.window_index())),
        );
        source = PaneTarget::new(peer, 2);
    }
    handler.wait_for_initial_panes_for_test().await;
    let mut snapshots = Vec::new();
    let mut runtimes = Vec::new();
    for target in windows {
        let window = {
            let state = handler.state.lock().await;
            state.sessions.window(&target).unwrap().clone()
        };
        for pane in window.panes() {
            handler
                .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(
                    target.session_name().clone(),
                    target.window_index(),
                    pane.index(),
                ))
                .await;
        }
        let state = handler.state.lock().await;
        for pane in window.panes() {
            let lifecycle = state.pane_lifecycle(pane.id()).unwrap();
            let PaneLifecycleProcessState::Running { pid } = lifecycle.process else {
                panic!("fixture pane must be running");
            };
            // Creation-bound identities come from the fixture's own live
            // lifecycle handles, never from a system-wide process inventory.
            eprintln!(
                "floating fixture owned pane={} pid={:?} generation={}",
                pane.id().as_u32(),
                pid,
                lifecycle.generation
            );
            runtimes.push((pane.id(), pid, lifecycle.generation));
        }
        snapshots.push((target, window));
    }
    FloatingSwapFixture {
        handler,
        source,
        target,
        windows: snapshots,
        runtimes,
    }
}

impl FloatingSwapFixture {
    pub async fn zoom(&mut self, source: bool, target: bool) {
        let mut seen = std::collections::HashSet::new();
        for (pane, zoom) in [(&self.source, source), (&self.target, target)] {
            if !zoom {
                continue;
            }
            let id = {
                let state = self.handler.state.lock().await;
                state
                    .sessions
                    .session(pane.session_name())
                    .unwrap()
                    .window_at(pane.window_index())
                    .unwrap()
                    .id()
            };
            if seen.insert(id) {
                // Keep the selected tile; zooming the floating endpoint would
                // change selection and would no longer be the archived case.
                assert!(matches!(
                    self.handler
                        .handle(Request::ResizePane(ResizePaneRequest {
                            target: PaneTarget::with_window(
                                pane.session_name().clone(),
                                pane.window_index(),
                                0
                            ),
                            adjustment: ResizePaneAdjustment::Zoom,
                        }))
                        .await,
                    Response::ResizePane(_)
                ));
            }
        }
        // Freeze after the zoom setup: zoom itself may touch selection history
        // or raise the active float. Only remove the oracle-required zoom in
        // the expected detached image; keep every other field exact.
        let state = self.handler.state.lock().await;
        for (target, expected) in &mut self.windows {
            *expected = state
                .sessions
                .session(target.session_name())
                .unwrap()
                .layout_window_snapshot(target.window_index())
                .unwrap();
        }
    }

    pub async fn assert_state_and_runtimes(&self) {
        let mut state = self.handler.state.lock().await;
        for (target, expected) in &self.windows {
            let actual = state.sessions.window(target).unwrap();
            // Activity is wall-clock seconds bumped by any late pane output (a
            // ConPTY repaint after a resize); it is not part of this contract.
            let mut expected = expected.clone();
            for pane in actual.panes() {
                assert!(expected.apply_pane_activity_snapshot(
                    actual.pane_activity_snapshot(pane.id()).unwrap()
                ));
            }
            assert_eq!(
                actual, &expected,
                "exact owner, panes, selection/history, stack and canonical geometry"
            );
            for pane in expected.panes() {
                let target = PaneTarget::with_window(
                    target.session_name().clone(),
                    target.window_index(),
                    pane.index(),
                );
                let geometry = pane.geometry();
                let size = TerminalSize::new(geometry.cols(), geometry.rows());
                assert_eq!(state.pane_lifecycle(pane.id()).unwrap().dimensions(), size);
                let master = state
                    .clone_pane_master_if_alive(
                        target.session_name(),
                        target.window_index(),
                        target.pane_index(),
                    )
                    .unwrap();
                let native = master.size().unwrap();
                assert_eq!((native.cols, native.rows), (size.cols, size.rows));
                assert_eq!(
                    state
                        .transcript_handle(&target)
                        .unwrap()
                        .lock()
                        .screen()
                        .size(),
                    size
                );
            }
        }
        for &(pane, pid, generation) in &self.runtimes {
            let lifecycle = state.pane_lifecycle(pane).unwrap();
            assert!(
                matches!(lifecycle.process, PaneLifecycleProcessState::Running { pid: actual } if actual == pid)
            );
            assert_eq!(lifecycle.generation, generation);
            assert!(lifecycle.exit_state.is_none());
        }
    }

    pub async fn cleanup(&self) {
        assert!(matches!(
            self.handler
                .handle(Request::KillServer(rmux_proto::KillServerRequest))
                .await,
            Response::KillServer(_)
        ));
        // KillServer queues daemon shutdown; this in-process fixture has no
        // daemon loop to drain it. Use the existing test teardown now, while
        // the runtime is alive, so each loop iteration waits for its children.
        self.handler
            .state
            .lock()
            .await
            .shutdown_terminals_for_test();
        eprintln!("floating fixture ordinary terminal teardown completed");
    }
}

pub(super) async fn floating_swap_refusal_case(
    route: FloatingSwapRoute,
    preserve_zoom: bool,
    reverse: bool,
    source_zoom: bool,
    target_zoom: bool,
) {
    let mut fixture = floating_swap_fixture(route).await;
    fixture.zoom(source_zoom, target_zoom).await;
    let (source, target) = if reverse {
        (fixture.target.clone(), fixture.source.clone())
    } else {
        (fixture.source.clone(), fixture.target.clone())
    };
    let response = fixture
        .handler
        .handle(Request::SwapPane(rmux_proto::SwapPaneRequest {
            source,
            target,
            direction: None,
            detached: true,
            preserve_zoom,
        }))
        .await;
    assert!(
        matches!(&response, Response::Error(error)
        if error.error == rmux_proto::RmuxError::Server("cannot swap floating panes".to_owned())),
        "{response:?}"
    );
    fixture.assert_state_and_runtimes().await;
    // A following self-swap with -Z must not resurrect a refused swap or zoom.
    let source = fixture.source.clone();
    assert!(matches!(
        fixture
            .handler
            .handle(Request::SwapPane(rmux_proto::SwapPaneRequest {
                source: source.clone(),
                target: source,
                direction: None,
                detached: false,
                preserve_zoom: true,
            }))
            .await,
        Response::SwapPane(_)
    ));
    fixture.assert_state_and_runtimes().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn floating_swap_refusal_unzooms_and_retains_public_runtime_state() {
    for route in [
        FloatingSwapRoute::SameWindow,
        FloatingSwapRoute::CrossWindow,
        FloatingSwapRoute::CrossSession,
    ] {
        for (preserve, reverse, source_zoom, target_zoom) in [
            (false, false, true, true),
            (true, false, true, true),
            (false, true, true, true),
            (false, false, false, true),
            (false, false, false, false),
        ] {
            floating_swap_refusal_case(route, preserve, reverse, source_zoom, target_zoom).await;
        }
    }
}

#[tokio::test]
async fn directional_floating_swap_validates_the_resolved_endpoint() {
    let mut fixture = floating_swap_fixture(FloatingSwapRoute::SameWindow).await;
    fixture.zoom(true, true).await;
    // -U resolves pane 2 from anchor 0; the explicit source is unused.
    let response = fixture
        .handler
        .handle(Request::SwapPane(rmux_proto::SwapPaneRequest {
            source: PaneTarget::new(session_name("missing-unused-source"), 99),
            target: fixture.target.clone(),
            direction: Some(rmux_proto::SwapPaneDirection::Up),
            detached: false,
            preserve_zoom: true,
        }))
        .await;
    assert!(matches!(response, Response::Error(error)
        if error.error == rmux_proto::RmuxError::Server("cannot swap floating panes".to_owned())));
    fixture.assert_state_and_runtimes().await;
    fixture.cleanup().await;
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
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(vec!["exit 0".to_owned()]),
            process_command: None,
        })))
        .await;
    assert!(matches!(respawned, rmux_proto::Response::RespawnPane(_)));
    wait_for_dead_pane(&handler, &alpha, 0, 0).await;

    let response = handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target,
            kill: false,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
        })))
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
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(vec!["exit 0".to_owned()]),
            process_command: None,
        })))
        .await;
    assert!(matches!(response, rmux_proto::Response::RespawnPane(_)));
    wait_for_dead_pane(&handler, &alpha, 0, 0).await;

    let deadline = tokio::time::Instant::now() + SERVER_SIGNAL_LIVENESS;
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
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(vec!["exit 0".to_owned()]),
            process_command: None,
        })))
        .await;
    assert!(matches!(response, rmux_proto::Response::RespawnPane(_)));
    wait_for_dead_pane(&handler, &alpha, 0, 0).await;

    let deadline = tokio::time::Instant::now() + SERVER_SIGNAL_LIVENESS;
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
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);

    let open = handler
        .handle(Request::PipePane(PipePaneRequest {
            target: target.clone(),
            stdin: false,
            stdout: true,
            once: false,
            command: Some(pipe_discard_command()),
        }))
        .await;
    assert!(matches!(open, rmux_proto::Response::PipePane(_)));
    let first_pipe = pipe_process_group_probe(&handler, &target).await;

    let close = handler
        .handle(Request::PipePane(PipePaneRequest {
            target: target.clone(),
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
    wait_for_pipe_child_to_stop(&first_pipe).await;

    // Opening a new pipe after an empty-command close should succeed, confirming the previous
    // pipe was cleaned up.
    let reopen = handler
        .handle(Request::PipePane(PipePaneRequest {
            target: target.clone(),
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
    let second_pipe = pipe_process_group_probe(&handler, &target).await;

    let _ = handler
        .handle(Request::PipePane(PipePaneRequest {
            target,
            stdin: false,
            stdout: true,
            once: false,
            command: None,
        }))
        .await;
    wait_for_pipe_child_to_stop(&second_pipe).await;
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
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
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
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    crate::session_fixture::assert_new_session(&created);

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
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
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
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    crate::session_fixture::assert_new_session(&created);

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
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
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
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    crate::session_fixture::assert_new_session(&created);

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

mod pane_identity_characterization;
