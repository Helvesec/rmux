use super::*;

#[derive(Clone, Copy, Debug)]
enum Replacement {
    Reindex,
    Respawn,
}

async fn stale_reset(entry: Entry, replacement: Replacement, sync: &str, external_control: bool) {
    let handler = RequestHandler::new();
    let (session, targets) = two_panes(&handler, "reset-identity").await;
    let child = reset_child::ReplacementChild::new();
    if matches!(replacement, Replacement::Reindex) {
        // Keep the survivor's child alive through the reindex. Its file gate
        // makes delayed PTY startup reproducible without scheduling sleeps.
        let response = handler
            .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
                target: targets[1].clone(),
                kill: true,
                start_directory: None,
                environment: None,
                command: Some(vec![child.command()]),
                process_command: None,
            })))
            .await;
        assert!(matches!(response, Response::RespawnPane(_)), "{response:?}");
    }
    let _entries = register_entries(&handler, &session).await;
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(WindowTarget::with_window(session.clone(), 0)),
            option: OptionName::SynchronizePanes,
            value: sync.into(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
    let (original_id, original_generation) = {
        let state = handler.state.lock().await;
        let id = state
            .sessions
            .session(&session)
            .unwrap()
            .window()
            .pane(0)
            .unwrap()
            .id();
        (id, state.pane_output_generation_for_target(&targets[0], id))
    };
    // The queue capture and the handler's first identity check have succeeded.
    // Hold execution after that check, outside the state lock, before reset.
    let pause = install_queue_exact_target_capture_pause(&handler, "send-keys-after-identity");
    let queued_handler = handler.clone();
    let queued = tokio::spawn(async move {
        run_entry(&queued_handler, entry, "send-keys -t reset-identity:0.0 -R").await
    });
    wait_for_pause(&pause).await;
    let response = match replacement {
        Replacement::Reindex => {
            handler
                .handle(Request::KillPane(KillPaneRequest {
                    target: targets[0].clone(),
                    kill_all_except: false,
                }))
                .await
        }
        Replacement::Respawn => {
            let command = child.command();
            handler
                .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
                    target: targets[0].clone(),
                    kill: true,
                    start_directory: None,
                    environment: None,
                    command: Some(vec![command]),
                    process_command: None,
                })))
                .await
        }
    };
    assert!(
        matches!(response, Response::KillPane(_) | Response::RespawnPane(_)),
        "{response:?}"
    );
    handler
        .disable_pane_focus_reports_for_test(&targets[0])
        .await;
    let (transcript, output) = {
        let state = handler.state.lock().await;
        let id = state
            .sessions
            .session(&session)
            .unwrap()
            .window()
            .pane(0)
            .unwrap()
            .id();
        match replacement {
            Replacement::Reindex => assert_ne!(id, original_id),
            Replacement::Respawn => {
                assert_eq!(id, original_id);
                assert_ne!(
                    state.pane_output_generation_for_target(&targets[0], id),
                    original_generation
                );
            }
        }
        (
            state.transcript_handle(&targets[0]).unwrap(),
            state.pane_output_for_target(&session, 0, 0).unwrap(),
        )
    };
    // Reindex retains a live PTY too. Its child must finish startup after the
    // resize, before KEEP is installed, just like a respawned child. Waiting
    // for spawn or for the first native preamble does not establish this.
    child.prepare_snapshot(&output).await;
    handler
        .disable_pane_focus_reports_for_test(&targets[0])
        .await;
    handler
        .state
        .lock()
        .await
        .start_pane_input_capture_for_test(&targets[0]);
    let before = {
        let mut transcript = transcript.lock();
        if external_control {
            transcript.append_bytes(b"\x1b[?7h");
        }
        transcript.append_bytes(b"\x1b[2J\x1b[3;4HKEEP\x1b[?1h\x1b[?2004h");
        (
            transcript.clone_screen(),
            transcript.mode(),
            transcript.history_size(),
        )
    };
    if external_control {
        child.release_and_observe(&output).await;
        assert!(!queued.is_finished(), "reset is still suspended");
        let current = {
            let transcript = transcript.lock();
            (
                transcript.clone_screen(),
                transcript.mode(),
                transcript.history_size(),
            )
        };
        assert_eq!(
            current, before,
            "{entry:?}/{replacement:?}/{sync}: external child output changed the fixture while reset remained suspended"
        );
    }
    let sequence = output.capture_with_next_sequence(|| ()).0;
    pause.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(2), queued)
        .await
        .expect("reset queue completes")
        .expect("reset task joins");
    let after = {
        let transcript = transcript.lock();
        (
            transcript.clone_screen(),
            transcript.mode(),
            transcript.history_size(),
        )
    };
    assert_eq!(
        after, before,
        "{entry:?}/{replacement:?}/{sync}: stale reset changed replacement terminal: {result:?}"
    );
    assert_eq!(
        output.capture_with_next_sequence(|| ()).0,
        sequence,
        "stale reset must not publish a terminal invalidation"
    );
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .pane_input_capture_for_test(&targets[0]),
        Some(Vec::new()),
        "stale reset must not write to the replacement child"
    );
    if matches!(entry, Entry::Control) {
        assert!(result
            .expect_err("stale reset must fail")
            .contains("target identity changed"));
    }
}

#[tokio::test]
async fn queued_send_keys_reset_rejects_replacement_after_initial_identity_check() {
    for entry in [Entry::Control, Entry::Binding] {
        for replacement in [Replacement::Reindex, Replacement::Respawn] {
            for sync in ["off", "on"] {
                stale_reset(entry, replacement, sync, false).await;
            }
        }
    }
}

#[path = "queue_exact_target_send_keys_reset_child.rs"]
mod reset_child;

#[tokio::test]
async fn queued_send_keys_reset_replacement_isolates_external_wrap_while_reset_paused() {
    stale_reset(Entry::Binding, Replacement::Respawn, "on", true).await;
}

#[tokio::test]
async fn queued_send_keys_reset_reindex_isolates_child_startup_while_reset_paused() {
    // No stale reset can run before the child boundary is observed and the
    // snapshot checked. A failure here identifies the fixture, not send-keys.
    stale_reset(Entry::Binding, Replacement::Reindex, "on", true).await;
}
