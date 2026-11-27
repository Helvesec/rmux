use super::*;
use crate::handler::scripting_support::install_queue_exact_target_capture_pause;
use std::time::Duration;

const CREATE: &str = "new-pane -t fnp:0.0 -x30 -y8 -X3 -Y2";

#[tokio::test]
async fn fnp1_control_authority_is_revalidated_after_admission_wait() {
    for closing in [false, true] {
        let handler = setup().await;
        let pid = 842_106;
        let (_id, _events) = register_control_for_session(&handler, pid, session_name("fnp")).await;
        let pause = install_queue_exact_target_capture_pause(&handler, "new-pane");
        let cloned = handler.clone();
        let pending = tokio::spawn(async move {
            cloned
                .execute_control_commands(pid, CommandParser::new().parse(CREATE).unwrap())
                .await
        });
        tokio::time::timeout(Duration::from_secs(3), pause.reached.notified())
            .await
            .unwrap();
        {
            let mut controls = handler.active_control.lock().await;
            let client = controls.by_pid.get_mut(&pid).unwrap();
            if closing {
                client
                    .closing
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            } else {
                client.can_write = false;
            }
        }
        pause.release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(3), pending)
            .await
            .unwrap()
            .unwrap();
        assert!(result.error.is_some(), "closing={closing}: {result:?}");
        assert_eq!(window(&handler).await.panes().len(), 1);
        assert!(window(&handler).await.floating_stack().is_empty());
        cleanup(&handler).await;
    }
}

#[tokio::test]
async fn fnp1_revalidates_pane_generation_window_session_and_attach_before_commit() {
    for change in [
        "respawn", "kill", "window", "session", "attach", "readonly", "closing",
    ] {
        let handler = setup().await;
        let pid = 842_131;
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        handler.register_attach(pid, session_name("fnp"), tx).await;
        queue(&handler, "split-window -d -t fnp:0.0").await.unwrap();
        let pause = install_queue_exact_target_capture_pause(&handler, "new-pane");
        let cloned = handler.clone();
        let pending = tokio::spawn(async move {
            cloned
                .execute_parsed_commands_for_test(pid, CommandParser::new().parse(CREATE).unwrap())
                .await
        });
        tokio::time::timeout(Duration::from_secs(3), pause.reached.notified())
            .await
            .unwrap();
        match change {
            "respawn" => {
                queue(&handler, "respawn-pane -k -t fnp:0.0").await.unwrap();
            }
            "kill" => {
                queue(&handler, "kill-pane -t fnp:0.0").await.unwrap();
            }
            "window" => {
                queue(&handler, "new-window -dk -t fnp:0").await.unwrap();
            }
            "session" => {
                replace_background_identity_session(&handler, session_name("fnp")).await;
            }
            "attach" => {
                let (tx, _replacement_rx) = tokio::sync::mpsc::unbounded_channel();
                handler.register_attach(pid, session_name("fnp"), tx).await;
            }
            "readonly" => {
                handler
                    .active_attach
                    .lock()
                    .await
                    .by_pid
                    .get_mut(&pid)
                    .unwrap()
                    .can_write = false;
            }
            "closing" => {
                handler
                    .active_attach
                    .lock()
                    .await
                    .by_pid
                    .get(&pid)
                    .unwrap()
                    .closing
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            _ => unreachable!(),
        }
        let before = window(&handler).await.panes().len();
        pause.release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(3), pending)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err(), "{change}: {result:?}");
        assert_eq!(window(&handler).await.panes().len(), before, "{change}");
        assert!(window(&handler).await.floating_stack().is_empty());
        cleanup(&handler).await;
    }
}

#[tokio::test]
async fn fnp1_offer_executes_the_prepared_alias_not_a_second_expansion() {
    let handler = setup().await;
    set_alias(&handler, &format!("probe={CREATE}")).await;
    let pause = install_queue_exact_target_capture_pause(&handler, "new-pane");
    let cloned = handler.clone();
    let pending = tokio::spawn(async move { offer(&cloned, &["probe"], true).await });
    tokio::time::timeout(Duration::from_secs(3), pause.reached.notified())
        .await
        .unwrap();
    set_alias(&handler, "probe=set-environment -g FNP_WRONG yes").await;
    pause.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(3), pending)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            result,
            RuntimeCommandExecutionResponse::Complete {
                error: None,
                exit_status: 0,
                ..
            }
        ),
        "{result:?}"
    );
    assert_eq!(window(&handler).await.floating_stack().len(), 1);
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .environment
            .global_value("FNP_WRONG"),
        None
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn fnp1_inactive_window_and_linked_family_have_one_owner_and_runtime() {
    let handler = setup().await;
    queue(&handler, "new-window -d -t fnp:1").await.unwrap();
    queue(&handler, "new-pane -t fnp:1.0 -x30 -y8 -X3 -Y2")
        .await
        .unwrap();
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .sessions
            .session(&session_name("fnp"))
            .unwrap()
            .active_window_index(),
        0
    );
    create_background_identity_session(&handler, session_name("linked")).await;
    queue(&handler, "link-window -s fnp:0 -t linked:1 -d")
        .await
        .unwrap();
    queue(&handler, "new-pane -t linked:1.0 -x19 -y5 -X-1 -Y3")
        .await
        .unwrap();
    let id = window(&handler).await.pane(1).unwrap().id();
    let pid = {
        let state = handler.state.lock().await;
        let other = state.sessions.session(&session_name("linked")).unwrap();
        assert_eq!(other.window_at(1).unwrap().pane(1).unwrap().id(), id);
        let pid = state
            .pane_pid_in_window(&session_name("fnp"), 0, 1)
            .unwrap();
        assert_eq!(
            state
                .pane_pid_in_window(&session_name("linked"), 1, 1)
                .unwrap(),
            pid
        );
        pid
    };
    assert_ne!(pid, 0);
    let rect = window(&handler)
        .await
        .floating_stack()
        .placement(id)
        .unwrap()
        .rect();
    queue(&handler, "respawn-pane -k -t linked:1.1")
        .await
        .unwrap();
    assert_eq!(
        window(&handler)
            .await
            .floating_stack()
            .placement(id)
            .unwrap()
            .rect(),
        rect
    );
    queue(&handler, "kill-pane -t linked:1.1").await.unwrap();
    assert!(window(&handler).await.floating_stack().is_empty());
    assert!(handler.state.lock().await.pane_lifecycle(id).is_none());
    cleanup(&handler).await;
}

#[tokio::test]
async fn fnp1_validation_and_spawn_failures_restore_placement_selection_and_options() {
    let handler = setup().await;
    for tail in ["-e INVALID", "-e FNP_FAIL=\0"] {
        let before = window(&handler).await;
        assert!(
            queue(&handler, &format!("{CREATE} {tail}")).await.is_err(),
            "{tail}"
        );
        let after = window(&handler).await;
        assert_eq!(after.panes().len(), before.panes().len());
        assert_eq!(after.floating_stack(), before.floating_stack());
        assert_eq!(after.active_pane_index(), before.active_pane_index());
    }
    queue(&handler, CREATE).await.unwrap();
    assert_eq!(window(&handler).await.floating_stack().len(), 1);
    let before_resize = window(&handler).await;
    let options_before_resize = {
        let mut state = handler.state.lock().await;
        state.fail_next_resize_for_test();
        state.options.clone()
    };
    super::fnp2::rename_automatically(&handler, "renamed under a failed resize".to_owned()).await;
    assert!(queue(&handler, "resize-pane -t fnp:0.1 -y 3")
        .await
        .is_err());
    super::fnp2::assert_refusal_kept_window(&handler, &before_resize, "a failed resize").await;
    assert_eq!(handler.state.lock().await.options, options_before_resize);
    queue(&handler, "select-layout -t fnp:0 even-horizontal")
        .await
        .unwrap();
    assert_eq!(window(&handler).await.floating_stack().len(), 1);
    cleanup(&handler).await;
}

#[tokio::test]
async fn fnp1_control_queue_and_source_share_admission_and_keep_source_list_sequencing() {
    let handler = setup().await;
    let pid = 842_105;
    let (_id, _events) = register_control_for_session(&handler, pid, session_name("fnp")).await;
    let result = Box::pin(
        handler.execute_control_commands(pid, CommandParser::new().parse(CREATE).unwrap()),
    )
    .await;
    assert!(result.error.is_none(), "{result:?}");
    assert_eq!(window(&handler).await.floating_stack().len(), 1);
    handler
        .active_control
        .lock()
        .await
        .by_pid
        .get_mut(&pid)
        .unwrap()
        .can_write = false;
    let result = Box::pin(
        handler.execute_control_commands(pid, CommandParser::new().parse(CREATE).unwrap()),
    )
    .await;
    assert!(result.error.is_some());
    assert_eq!(window(&handler).await.floating_stack().len(), 1);
    // Internal source lists are not the deliberately excluded CLI batch offer.
    let source =
        format!("set-environment -g FNP_BEFORE yes ; {CREATE} ; set-environment -g FNP_AFTER yes");
    let response = Box::pin(
        handler.handle(Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec!["-".to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: None,
            caller_cwd: None,
            stdin: Some(source),
        }))),
    )
    .await;
    assert!(matches!(response, Response::SourceFile(_)), "{response:?}");
    assert_eq!(window(&handler).await.floating_stack().len(), 2);
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .environment
            .global_value("FNP_AFTER"),
        Some("yes")
    );
    cleanup(&handler).await;
}
