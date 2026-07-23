use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use super::*;
use crate::handler::scripting_support::install_queue_exact_target_capture_pause;

async fn retained_window_binding(
    handler: &RequestHandler,
    session_name: &SessionName,
) -> (Option<Target>, std::sync::Arc<LifecycleTargetLease>) {
    let mut state = handler.state.lock().await;
    let event = super::super::super::prepare_lifecycle_event(
        &mut state,
        &rmux_core::LifecycleEvent::AlertActivity {
            target: WindowTarget::with_window(session_name.clone(), 0),
        },
    );
    (
        event.current_target,
        event
            .retained_current_target
            .expect("activity event captures a retained window"),
    )
}

async fn session_with_spare_window(handler: &RequestHandler, name: &str) -> SessionName {
    let session_name = create_handler_session(handler, name).await;
    handler
        .state
        .lock()
        .await
        .sessions
        .session_mut(&session_name)
        .expect("session exists")
        .create_window(terminal_size())
        .expect("create spare window");
    session_name
}

async fn wait_for_pause(pause: &crate::handler::scripting_support::QueueExactTargetCapturePause) {
    tokio::time::timeout(Duration::from_secs(2), pause.reached.notified())
        .await
        .expect("special command reaches post-capture pause");
}

async fn retire_window_zero(handler: &RequestHandler, session_name: &SessionName, replace: bool) {
    let mut state = handler.state.lock().await;
    let session = state
        .sessions
        .session_mut(session_name)
        .expect("session survives through its spare window");
    session
        .remove_window_allowing_empty(0)
        .expect("remove retained window");
    if replace {
        session
            .insert_window_with_initial_pane(0, terminal_size())
            .expect("replace retained numeric slot");
    }
    state.retire_removed_lifecycle_targets();
}

async fn run_paused_hook(
    handler: &RequestHandler,
    session_name: &SessionName,
    command_name: &'static str,
    command: String,
    replace: bool,
) -> Result<(), rmux_proto::RmuxError> {
    let (current_target, lease) = retained_window_binding(handler, session_name).await;
    let pause = install_queue_exact_target_capture_pause(handler, command_name);
    let queued_handler = handler.clone();
    let queued = tokio::spawn(async move {
        queued_handler
            .execute_hook_command_with_target_binding(
                std::process::id(),
                &command,
                current_target,
                Some(lease),
            )
            .await
    });
    wait_for_pause(&pause).await;
    retire_window_zero(handler, session_name, replace).await;
    pause.release.notify_one();
    queued.await.expect("special command task joins")
}

async fn buffer_is_missing(handler: &RequestHandler, name: &str) -> bool {
    handler.state.lock().await.buffers.show(Some(name)).is_err()
}

#[tokio::test]
async fn if_shell_foreground_rejects_retired_and_replaced_lifecycle_targets() {
    for replace in [false, true] {
        let handler = RequestHandler::new();
        let session_name =
            session_with_spare_window(&handler, &format!("special-if-fg-{replace}")).await;
        let buffer = format!("special-if-fg-{replace}");
        let error = run_paused_hook(
            &handler,
            &session_name,
            "if-shell",
            format!("if-shell -F 1 'set-buffer -b {buffer} stale'"),
            replace,
        )
        .await
        .expect_err("stale foreground branch fails closed");
        let phase = if replace { "replaced" } else { "retired" };
        assert!(error.to_string().contains(phase), "{error}");
        assert!(buffer_is_missing(&handler, &buffer).await);
    }
}

#[tokio::test]
async fn if_shell_background_rejects_retired_and_replaced_lifecycle_targets() {
    for replace in [false, true] {
        let handler = RequestHandler::new();
        let session_name =
            session_with_spare_window(&handler, &format!("special-if-bg-{replace}")).await;
        let buffer = format!("special-if-bg-{replace}");
        let result = run_paused_hook(
            &handler,
            &session_name,
            "if-shell",
            format!("if-shell -b true 'set-buffer -b {buffer} stale'"),
            replace,
        )
        .await;
        if let Err(error) = result {
            let phase = if replace { "replaced" } else { "retired" };
            assert!(error.to_string().contains(phase), "{error}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(buffer_is_missing(&handler, &buffer).await);
    }
}

#[tokio::test]
async fn source_file_rejects_retired_and_replaced_lifecycle_targets() {
    for replace in [false, true] {
        let handler = RequestHandler::new();
        let session_name =
            session_with_spare_window(&handler, &format!("special-source-{replace}")).await;
        let buffer = format!("special-source-{replace}");
        let path = unique_temp_path(&format!("source-{replace}.conf"));
        std::fs::write(&path, format!("set-buffer -b {buffer} stale\n"))
            .expect("write source fixture");
        let command = format!(
            "source-file {}",
            crate::test_shell::command_quote(&path.display().to_string())
        );
        let error = run_paused_hook(&handler, &session_name, "source-file", command, replace)
            .await
            .expect_err("stale source target fails closed before loading");
        let _ = std::fs::remove_file(path);
        let phase = if replace { "replaced" } else { "retired" };
        assert!(error.to_string().contains(phase), "{error}");
        assert!(buffer_is_missing(&handler, &buffer).await);
    }
}

#[tokio::test]
async fn source_file_missing_explicit_target_cuts_the_outer_lifecycle_lease() {
    let handler = RequestHandler::new();
    let alpha = session_with_spare_window(&handler, "special-source-missing-alpha").await;
    let beta = create_handler_session(&handler, "special-source-missing-beta").await;
    let (current_target, lease) = retained_window_binding(&handler, &alpha).await;
    let path = unique_temp_path("source-missing-target.conf");
    std::fs::write(&path, "rename-window fallback-beta\n").expect("write source fixture");
    let pause = install_queue_exact_target_capture_pause(&handler, "rename-window");
    let queued_handler = handler.clone();
    let queued = tokio::spawn(async move {
        queued_handler
            .execute_hook_command_with_target_binding(
                std::process::id(),
                &format!(
                    "source-file -t %9999 {}",
                    crate::test_shell::command_quote(&path.display().to_string())
                ),
                current_target,
                Some(lease),
            )
            .await
            .map(|result| (result, path))
    });

    wait_for_pause(&pause).await;
    retire_window_zero(&handler, &alpha, false).await;
    pause.release.notify_one();
    let (_, path) = queued
        .await
        .expect("missing-target source task joins")
        .expect("fallback target is independent from the retired outer lease");
    let _ = std::fs::remove_file(path);

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(0))
            .and_then(rmux_core::Window::name),
        Some("fallback-beta")
    );
}

#[tokio::test]
async fn run_shell_command_modes_reject_retired_and_replaced_lifecycle_targets() {
    for background in [false, true] {
        for replace in [false, true] {
            let handler = RequestHandler::new();
            let session_name =
                session_with_spare_window(&handler, &format!("special-run-{background}-{replace}"))
                    .await;
            let buffer = format!("special-run-{background}-{replace}");
            let flags = if background { "-bC" } else { "-C" };
            let result = run_paused_hook(
                &handler,
                &session_name,
                "run-shell",
                format!("run-shell {flags} 'set-buffer -b {buffer} stale'"),
                replace,
            )
            .await;
            if background {
                if let Err(error) = result {
                    let phase = if replace { "replaced" } else { "retired" };
                    assert!(error.to_string().contains(phase), "{error}");
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            } else {
                let error = result.expect_err("foreground command mode fails closed");
                let phase = if replace { "replaced" } else { "retired" };
                assert!(error.to_string().contains(phase), "{error}");
            }
            assert!(buffer_is_missing(&handler, &buffer).await);
        }
    }
}

#[tokio::test]
async fn explicit_if_shell_target_cuts_the_lifecycle_lease_and_pins_beta() {
    let handler = RequestHandler::new();
    let alpha = session_with_spare_window(&handler, "special-explicit-alpha").await;
    let beta = create_handler_session(&handler, "special-explicit-beta").await;
    let (current_target, lease) = retained_window_binding(&handler, &alpha).await;
    retire_window_zero(&handler, &alpha, false).await;

    handler
        .execute_hook_command_with_target_binding(
            std::process::id(),
            &format!("if-shell -t {beta} -F 1 'rename-window explicit-beta'"),
            current_target,
            Some(lease),
        )
        .await
        .expect("explicit beta target is independent from retired alpha");

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(0))
            .and_then(rmux_core::Window::name),
        Some("explicit-beta")
    );
}

#[tokio::test]
async fn explicit_if_shell_target_rejects_same_name_slot_replacement() {
    let handler = RequestHandler::new();
    let alpha = session_with_spare_window(&handler, "special-explicit-aba-alpha").await;
    let beta = create_handler_session(&handler, "special-explicit-aba-beta").await;
    let (current_target, lease) = retained_window_binding(&handler, &alpha).await;
    let pause = install_queue_exact_target_capture_pause(&handler, "if-shell");
    let queued_handler = handler.clone();
    let beta_for_command = beta.clone();
    let queued = tokio::spawn(async move {
        queued_handler
            .execute_hook_command_with_target_binding(
                std::process::id(),
                &format!("if-shell -t {beta_for_command} -F 1 'rename-window stale-beta'"),
                current_target,
                Some(lease),
            )
            .await
    });
    wait_for_pause(&pause).await;
    {
        let mut state = handler.state.lock().await;
        state.sessions.remove_session(&beta).expect("remove beta");
        state
            .sessions
            .create_session(beta.clone(), terminal_size())
            .expect("recreate beta with the same name");
    }
    pause.release.notify_one();
    let error = queued
        .await
        .expect("explicit target task joins")
        .expect_err("same-name replacement fails closed");
    assert!(error.to_string().contains("replaced"), "{error}");
    let state = handler.state.lock().await;
    assert_ne!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(0))
            .and_then(rmux_core::Window::name),
        Some("stale-beta")
    );
}

#[tokio::test]
async fn special_mutations_reject_a_respawned_pane_with_the_same_pane_id() {
    let source_path = unique_temp_path("source-respawn.conf");
    std::fs::write(&source_path, "set-buffer -b special-respawn-source stale\n")
        .expect("write respawn source fixture");
    let cases = vec![
        (
            "if-shell",
            "if-shell -F 1 'set-buffer -b special-respawn-if stale'".to_owned(),
            "special-respawn-if",
        ),
        (
            "source-file",
            format!(
                "source-file {}",
                crate::test_shell::command_quote(&source_path.display().to_string())
            ),
            "special-respawn-source",
        ),
        (
            "run-shell",
            "run-shell -C 'set-buffer -b special-respawn-run stale'".to_owned(),
            "special-respawn-run",
        ),
    ];
    for (command_name, command, buffer) in cases {
        let handler = RequestHandler::new();
        let session_name =
            create_handler_session(&handler, &format!("{command_name}-respawn")).await;
        let (current_target, lease) = retained_window_binding(&handler, &session_name).await;
        let pause = install_queue_exact_target_capture_pause(&handler, command_name);
        let queued_handler = handler.clone();
        let queued = tokio::spawn(async move {
            queued_handler
                .execute_hook_command_with_target_binding(
                    std::process::id(),
                    &command,
                    current_target,
                    Some(lease),
                )
                .await
        });
        wait_for_pause(&pause).await;
        let response = handler
            .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
                target: PaneTarget::with_window(session_name.clone(), 0, 0),
                kill: true,
                start_directory: None,
                environment: None,
                command: Some(vec![crate::test_shell::stdin_discard_command()]),
                process_command: None,
            })))
            .await;
        assert!(matches!(response, Response::RespawnPane(_)), "{response:?}");
        pause.release.notify_one();
        let error = queued
            .await
            .expect("respawn guard task joins")
            .expect_err("same-pane-id process replacement fails closed");
        assert!(
            error.to_string().contains("replaced before execution"),
            "{error}"
        );
        assert!(buffer_is_missing(&handler, buffer).await);
    }
    let _ = std::fs::remove_file(source_path);
}

#[tokio::test]
async fn admitted_background_shell_finishes_after_its_lifecycle_target_retires() {
    let handler = RequestHandler::new();
    let session_name = session_with_spare_window(&handler, "special-admitted-shell").await;
    let started = unique_temp_path("shell-started");
    let finished = unique_temp_path("shell-finished");
    let shell_command = delayed_file_command(&started, &finished);
    let (current_target, lease) = retained_window_binding(&handler, &session_name).await;

    handler
        .execute_hook_command_with_target_binding(
            std::process::id(),
            &format!(
                "run-shell -b {}",
                crate::test_shell::command_quote(&shell_command)
            ),
            current_target,
            Some(lease),
        )
        .await
        .expect("background shell is admitted while its exact profile is live");
    wait_for_file(&started).await;
    retire_window_zero(&handler, &session_name, false).await;
    wait_for_file(&finished).await;
    let _ = std::fs::remove_file(started);
    let _ = std::fs::remove_file(finished);
}

fn unique_temp_path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("rmux-{label}-{}-{nanos}", std::process::id()))
}

async fn wait_for_file(path: &std::path::Path) {
    tokio::time::timeout(background_shell_marker_timeout(), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("background shell did not write marker {}", path.display()));
}

fn background_shell_marker_timeout() -> Duration {
    #[cfg(windows)]
    {
        // Hosted Windows can heavily delay a cold PowerShell process. This
        // test checks eventual completion, not process-start latency.
        Duration::from_secs(30)
    }
    #[cfg(not(windows))]
    {
        Duration::from_secs(10)
    }
}

fn delayed_file_command(started: &std::path::Path, finished: &std::path::Path) -> String {
    #[cfg(unix)]
    {
        format!(
            "printf started > {}; sleep 0.2; printf finished > {}",
            crate::test_shell::sh_quote_path(started),
            crate::test_shell::sh_quote_path(finished)
        )
    }
    #[cfg(windows)]
    {
        crate::test_shell::powershell_encoded_command(&format!(
            "[IO.File]::WriteAllText({}, 'started'); Start-Sleep -Milliseconds 200; [IO.File]::WriteAllText({}, 'finished')",
            crate::test_shell::powershell_quote_path(started),
            crate::test_shell::powershell_quote_path(finished)
        ))
    }
}
