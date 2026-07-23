use super::*;

use std::time::Duration;

use rmux_proto::{LinkWindowRequest, UnlinkWindowRequest};

use crate::handler::scripting_support::install_queue_exact_target_capture_pause;

fn terminal_size() -> TerminalSize {
    TerminalSize { cols: 80, rows: 24 }
}

async fn create_session(handler: &RequestHandler, name: &str) -> SessionName {
    let name = session_name(name);
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: name.clone(),
            detached: true,
            size: Some(terminal_size()),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    name
}

fn pane_context(session_name: &SessionName, window_index: u32) -> QueueExecutionContext {
    QueueExecutionContext::without_caller_cwd().with_current_target(Some(Target::Pane(
        PaneTarget::with_window(session_name.clone(), window_index, 0),
    )))
}

async fn replace_window_slot(
    handler: &RequestHandler,
    source: &SessionName,
    target: &SessionName,
) -> rmux_core::WindowId {
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(source.clone(), 0),
            target: WindowTarget::with_window(target.clone(), 0),
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    handler
        .state
        .lock()
        .await
        .sessions
        .session(target)
        .and_then(|session| session.window_at(0))
        .map(rmux_core::Window::id)
        .expect("replacement window survives")
}

async fn wait_for_pause(pause: &crate::handler::scripting_support::QueueExactTargetCapturePause) {
    tokio::time::timeout(Duration::from_secs(2), pause.reached.notified())
        .await
        .expect("queued command reaches exact-target capture pause");
}

#[tokio::test]
async fn exact_window_and_pane_targets_reject_post_parse_slot_replacement() {
    for command_name in ["rename-window", "kill-pane"] {
        let handler = RequestHandler::new();
        let source = create_session(&handler, &format!("exact-{command_name}-source")).await;
        let target = create_session(&handler, &format!("exact-{command_name}-target")).await;
        let (selector, original_window_id) = {
            let state = handler.state.lock().await;
            let window = state
                .sessions
                .session(&target)
                .and_then(|session| session.window_at(0))
                .expect("target window exists");
            let selector = if command_name == "rename-window" {
                window.id().to_string()
            } else {
                window.pane(0).expect("target pane exists").id().to_string()
            };
            (selector, window.id())
        };
        let command = if command_name == "rename-window" {
            format!("rename-window -t {selector} stale-name")
        } else {
            format!("kill-pane -t {selector}")
        };
        let parsed = CommandParser::new()
            .parse(&command)
            .expect("command parses");
        let pause = install_queue_exact_target_capture_pause(&handler, command_name);
        let queued_handler = handler.clone();
        let queued_target = target.clone();
        let queued = tokio::spawn(async move {
            queued_handler
                .execute_parsed_commands(
                    std::process::id(),
                    parsed,
                    pane_context(&queued_target, 0),
                )
                .await
        });

        wait_for_pause(&pause).await;
        let replacement_id = replace_window_slot(&handler, &source, &target).await;
        assert_ne!(replacement_id, original_window_id);
        pause.release.notify_one();

        let error = tokio::time::timeout(Duration::from_secs(2), queued)
            .await
            .expect("queued command finishes")
            .expect("queued task joins")
            .expect_err("replacement must invalidate the captured target");
        assert!(
            error.to_string().contains("target identity changed"),
            "{error}"
        );
        let state = handler.state.lock().await;
        let replacement = state
            .sessions
            .session(&target)
            .and_then(|session| session.window_at(0))
            .expect("replacement survives stale command");
        assert_eq!(replacement.id(), replacement_id);
        assert_ne!(replacement.name(), Some("stale-name"));
    }
}

#[tokio::test]
async fn exact_pane_target_rejects_respawned_output_generation() {
    let handler = RequestHandler::new();
    let target_session = create_session(&handler, "exact-pane-respawn").await;
    let target = PaneTarget::with_window(target_session.clone(), 0, 0);
    let (pane_id, initial_generation) = {
        let state = handler.state.lock().await;
        let pane_id = state
            .sessions
            .session(&target_session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(rmux_core::Pane::id)
            .expect("target pane exists");
        (
            pane_id,
            state.pane_output_generation_for_target(&target, pane_id),
        )
    };
    let parsed = CommandParser::new()
        .parse(&format!("kill-pane -t {pane_id}"))
        .expect("command parses");
    let pause = install_queue_exact_target_capture_pause(&handler, "kill-pane");
    let queued_handler = handler.clone();
    let queued_session = target_session.clone();
    let queued = tokio::spawn(async move {
        queued_handler
            .execute_parsed_commands(std::process::id(), parsed, pane_context(&queued_session, 0))
            .await
    });

    wait_for_pause(&pause).await;
    let respawned = handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(vec![crate::test_shell::stdin_discard_command()]),
            process_command: None,
        })))
        .await;
    assert!(
        matches!(respawned, Response::RespawnPane(_)),
        "{respawned:?}"
    );
    let replacement_generation = {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&target_session)
                .and_then(|session| session.window_at(0))
                .and_then(|window| window.pane(0))
                .map(rmux_core::Pane::id),
            Some(pane_id),
            "respawn must preserve PaneId for this regression"
        );
        state.pane_output_generation_for_target(&target, pane_id)
    };
    assert_ne!(replacement_generation, initial_generation);
    pause.release.notify_one();

    let error = tokio::time::timeout(Duration::from_secs(2), queued)
        .await
        .expect("queued command finishes")
        .expect("queued task joins")
        .expect_err("the stale kill must not reach the respawned process");
    assert!(
        error.to_string().contains("target identity changed"),
        "{error}"
    );
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&target_session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(rmux_core::Pane::id),
        Some(pane_id),
        "the respawned pane must survive the stale command"
    );
}

#[tokio::test]
async fn capture_initializes_only_the_addressed_lazy_occurrence() {
    let handler = RequestHandler::new();
    let target = session_name("exact-target-lazy-occurrence");
    let selector = {
        let mut state = handler.state.lock().await;
        state
            .sessions
            .create_session(target.clone(), terminal_size())
            .expect("create direct session");
        assert_eq!(state.window_link_occurrence_id(&target, 0), None);
        state
            .sessions
            .session(&target)
            .and_then(|session| session.window_at(0))
            .map(rmux_core::Window::id)
            .expect("target window exists")
            .to_string()
    };
    let parsed = CommandParser::new()
        .parse(&format!("rename-window -t {selector} guarded"))
        .expect("command parses");
    let pause = install_queue_exact_target_capture_pause(&handler, "rename-window");
    let queued_handler = handler.clone();
    let queued_target = target.clone();
    let queued = tokio::spawn(async move {
        queued_handler
            .execute_parsed_commands(std::process::id(), parsed, pane_context(&queued_target, 0))
            .await
    });

    wait_for_pause(&pause).await;
    {
        let mut state = handler.state.lock().await;
        assert!(
            state.window_link_occurrence_id(&target, 0).is_some(),
            "capture must initialize the addressed occurrence"
        );
        state.ensure_live_window_link_occurrences();
    }
    pause.release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), queued)
        .await
        .expect("queued command finishes")
        .expect("queued task joins")
        .expect("later lazy initialization must not create a false stale target");

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&target)
            .and_then(|session| session.window_at(0))
            .and_then(rmux_core::Window::name),
        Some("guarded")
    );
}

#[tokio::test]
async fn unlink_relink_of_the_same_window_id_is_still_rejected() {
    let handler = RequestHandler::new();
    let owner = create_session(&handler, "exact-target-owner").await;
    let alias = create_session(&handler, "exact-target-alias").await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(alias.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    let window_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&owner)
        .and_then(|session| session.window_at(0))
        .map(rmux_core::Window::id)
        .expect("owner window exists");
    let parsed = CommandParser::new()
        .parse(&format!("rename-window -t {window_id} stale-relink"))
        .expect("command parses");
    let pause = install_queue_exact_target_capture_pause(&handler, "rename-window");
    let queued_handler = handler.clone();
    let queued_alias = alias.clone();
    let queued = tokio::spawn(async move {
        queued_handler
            .execute_parsed_commands(std::process::id(), parsed, pane_context(&queued_alias, 1))
            .await
    });

    wait_for_pause(&pause).await;
    let unlinked = handler
        .handle(Request::UnlinkWindow(UnlinkWindowRequest {
            target: WindowTarget::with_window(alias.clone(), 1),
            kill_if_last: false,
        }))
        .await;
    assert!(
        matches!(unlinked, Response::UnlinkWindow(_)),
        "{unlinked:?}"
    );
    let relinked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(alias.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(relinked, Response::LinkWindow(_)), "{relinked:?}");
    pause.release.notify_one();

    let error = tokio::time::timeout(Duration::from_secs(2), queued)
        .await
        .expect("queued command finishes")
        .expect("queued task joins")
        .expect_err("a new link occurrence must invalidate the captured command");
    assert!(
        error.to_string().contains("target identity changed"),
        "{error}"
    );
    let state = handler.state.lock().await;
    let relinked_window = state
        .sessions
        .session(&alias)
        .and_then(|session| session.window_at(1))
        .expect("relinked window survives");
    assert_eq!(relinked_window.id(), window_id);
    assert_ne!(relinked_window.name(), Some("stale-relink"));
}

#[tokio::test]
async fn control_queue_uses_the_same_exact_target_guard() {
    let handler = RequestHandler::new();
    let source = create_session(&handler, "exact-control-source").await;
    let target = create_session(&handler, "exact-control-target").await;
    let selector = handler
        .state
        .lock()
        .await
        .sessions
        .session(&target)
        .and_then(|session| session.window_at(0))
        .map(rmux_core::Window::id)
        .expect("target window exists")
        .to_string();
    let requester_pid = 82_991;
    let (_control_id, _events) =
        register_control_for_session(&handler, requester_pid, target.clone()).await;
    let parsed = CommandParser::new()
        .parse(&format!("rename-window -t {selector} stale-control"))
        .expect("command parses");
    let pause = install_queue_exact_target_capture_pause(&handler, "rename-window");
    let queued_handler = handler.clone();
    let queued = tokio::spawn(async move {
        queued_handler
            .execute_control_commands(requester_pid, parsed)
            .await
    });

    wait_for_pause(&pause).await;
    let replacement_id = replace_window_slot(&handler, &source, &target).await;
    pause.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(2), queued)
        .await
        .expect("control command finishes")
        .expect("control task joins");
    let error = result.error.expect("control mutation must fail closed");
    assert!(
        error.to_string().contains("target identity changed"),
        "{error}"
    );
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .sessions
            .session(&target)
            .and_then(|session| session.window_at(0))
            .map(rmux_core::Window::id),
        Some(replacement_id)
    );
}

#[tokio::test]
async fn source_file_queue_uses_the_same_exact_target_guard() {
    let handler = RequestHandler::new();
    let source = create_session(&handler, "exact-source-file-source").await;
    let target = create_session(&handler, "exact-source-file-target").await;
    let selector = handler
        .state
        .lock()
        .await
        .sessions
        .session(&target)
        .and_then(|session| session.window_at(0))
        .map(rmux_core::Window::id)
        .expect("target window exists")
        .to_string();
    let root = temp_root("exact-target-guard");
    let config = root.join("guard.conf");
    write_config(
        &config,
        &format!("rename-window -t {selector} stale-source-file\n"),
    );
    let parsed = CommandParser::new()
        .parse(&format!("source-file {}", shell_quote(&config)))
        .expect("source-file command parses");
    let pause = install_queue_exact_target_capture_pause(&handler, "rename-window");
    let queued_handler = handler.clone();
    let queued_target = target.clone();
    let queued = tokio::spawn(async move {
        queued_handler
            .execute_parsed_commands(std::process::id(), parsed, pane_context(&queued_target, 0))
            .await
    });

    wait_for_pause(&pause).await;
    let replacement_id = replace_window_slot(&handler, &source, &target).await;
    pause.release.notify_one();
    let error = tokio::time::timeout(Duration::from_secs(2), queued)
        .await
        .expect("source-file command finishes")
        .expect("source-file task joins")
        .expect_err("sourced mutation must fail closed");
    assert!(
        error.to_string().contains("target identity changed"),
        "{error}"
    );
    let state = handler.state.lock().await;
    let replacement = state
        .sessions
        .session(&target)
        .and_then(|session| session.window_at(0))
        .expect("replacement survives sourced race");
    assert_eq!(replacement.id(), replacement_id);
    assert_ne!(replacement.name(), Some("stale-source-file"));
    drop(state);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn hook_command_path_uses_the_same_exact_target_guard() {
    let handler = RequestHandler::new();
    let source = create_session(&handler, "exact-hook-source").await;
    let target = create_session(&handler, "exact-hook-target").await;
    let selector = handler
        .state
        .lock()
        .await
        .sessions
        .session(&target)
        .and_then(|session| session.window_at(0))
        .map(rmux_core::Window::id)
        .expect("target window exists")
        .to_string();
    let pause = install_queue_exact_target_capture_pause(&handler, "rename-window");
    let queued_handler = handler.clone();
    let command = format!("rename-window -t {selector} stale-hook");
    let current_target = Some(Target::Pane(PaneTarget::with_window(source.clone(), 0, 0)));
    let queued = tokio::spawn(async move {
        queued_handler
            .execute_hook_command_with_context(std::process::id(), &command, current_target)
            .await
    });

    wait_for_pause(&pause).await;
    let replacement_id = replace_window_slot(&handler, &source, &target).await;
    pause.release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), queued)
        .await
        .expect("hook command finishes")
        .expect("hook task joins")
        .expect_err("hook mutation must fail closed");

    let state = handler.state.lock().await;
    let replacement = state
        .sessions
        .session(&target)
        .and_then(|session| session.window_at(0))
        .expect("replacement survives hook race");
    assert_eq!(replacement.id(), replacement_id);
    assert_ne!(replacement.name(), Some("stale-hook"));
}
