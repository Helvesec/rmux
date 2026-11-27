use super::*;

#[tokio::test]
async fn queued_formatter_refreshes_client_snapshot_after_switch() {
    let handler = RequestHandler::new();
    let requester_pid = 424_901;
    let alpha = session_name("queue-client-fresh-alpha");
    let beta = session_name("queue-client-fresh-beta");
    create_background_identity_session(&handler, alpha.clone()).await;
    create_background_identity_session(&handler, beta.clone()).await;
    let (_control_id, _events) = register_control_for_session(&handler, requester_pid, alpha).await;

    let commands = handler
        .parse_control_commands(&format!(
            "switch-client -t {beta} ; display-message -p '#{{L:#{{client_session}}}}'"
        ))
        .await
        .expect("switch and formatter commands parse");
    let result = handler
        .execute_control_commands(requester_pid, commands)
        .await;

    assert!(result.error.is_none(), "queued commands failed: {result:?}");
    assert_eq!(
        result.stdout,
        format!("{beta}\n").into_bytes(),
        "the formatter after switch must see a fresh client snapshot"
    );
}

// Snapshot resolution nests active_attach inside state. Churn every path that
// takes both locks concurrently and require progress within a deadline (a
// lock-order cycle would hang).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_snapshots_make_progress_under_attach_churn_and_option_writes() {
    let handler = RequestHandler::new();
    let requester_pid = 424_921;
    let alpha = session_name("queue-client-churn-alpha");
    create_background_identity_session(&handler, alpha.clone()).await;
    let (_control_id, mut events) =
        register_control_for_session(&handler, requester_pid, alpha.clone()).await;
    let drain = tokio::spawn(async move { while events.recv().await.is_some() {} });

    let writer = {
        let handler = handler.clone();
        tokio::spawn(async move {
            for i in 0..200 {
                let commands = handler
                    .parse_control_commands(&format!(
                        "set -g @r3222_{i} v ; display-message -p '#{{L:#{{client_name}}}}'"
                    ))
                    .await
                    .expect("commands parse");
                let result = handler
                    .execute_control_commands(requester_pid, commands)
                    .await;
                assert!(result.error.is_none(), "{result:?}");
            }
        })
    };
    let churn = {
        let handler = handler.clone();
        let alpha = alpha.clone();
        tokio::spawn(async move {
            for i in 0..200u32 {
                let pid = 424_930 + (i % 4);
                let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                let id = handler.register_attach(pid, alpha.clone(), tx).await;
                tokio::task::yield_now().await;
                handler.finish_attach(pid, id).await;
            }
        })
    };
    let readers = {
        let handler = handler.clone();
        tokio::spawn(async move {
            for _ in 0..200 {
                handler.update_client_viewports().await;
                let _ = handler.list_clients_snapshot().await;
                handler.refresh_theme_sources().await;
                let _ = handler.client_loop_snapshots().await;
            }
        })
    };
    let all = async {
        writer.await.expect("writer");
        churn.await.expect("churn");
        readers.await.expect("readers");
    };
    let _drain = drain;
    tokio::time::timeout(std::time::Duration::from_secs(120), all)
        .await
        .expect("no lock-order deadlock between snapshots, attach churn and option writes");
}

// run-shell -C expands its command with the queue item's own client-loop
// snapshot (display-message re-reads clients instead), so this pins both the
// per-item snapshot refresh of the command queue and the live option read.
#[tokio::test]
async fn queued_item_snapshot_sees_attached_client_features_after_option_change() {
    let handler = RequestHandler::new();
    let requester_pid = 424_941;
    let attach_pid = 424_942;
    let alpha = session_name("queue-item-features-alpha");
    create_background_identity_session(&handler, alpha.clone()).await;
    let (_control_id, _events) =
        register_control_for_session(&handler, requester_pid, alpha.clone()).await;
    let environment =
        std::collections::HashMap::from([("TERM".to_owned(), "xterm-r3223".to_owned())]);
    let (attach_tx, _attach_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach_with_terminal_context(
            attach_pid,
            alpha,
            attach_tx,
            crate::outer_terminal::OuterTerminalContext::from_environment(Some(&environment)),
        )
        .await;

    let commands = handler
        .parse_control_commands(
            "run-shell -C 'set-buffer -b r3223-before \"#{L:#{?client_control_mode,,[#{client_termfeatures}]}}\"' ; \
             set -s terminal-features 'xterm-r3223:sixel' ; \
             run-shell -C 'set-buffer -b r3223-after \"#{L:#{?client_control_mode,,[#{client_termfeatures}]}}\"'",
        )
        .await
        .expect("run-shell and option commands parse");
    let result = handler
        .execute_control_commands(requester_pid, commands)
        .await;
    assert!(result.error.is_none(), "queued commands failed: {result:?}");

    let mut buffers = Vec::new();
    for name in ["r3223-before", "r3223-after"] {
        let response = handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some(name.to_owned()),
            }))
            .await;
        let stdout = response
            .command_output()
            .unwrap_or_else(|| panic!("show-buffer {name}: {response:?}"))
            .stdout()
            .to_vec();
        buffers.push(String::from_utf8(stdout).expect("utf-8"));
    }
    assert!(
        buffers[0].contains("bpaste"),
        "attached client seen: {buffers:?}"
    );
    assert!(!buffers[0].contains("sixel"), "before: {buffers:?}");
    assert!(buffers[1].contains("sixel"), "after: {buffers:?}");
}
