use super::*;
use rmux_core::formats::{
    DEFAULT_LIST_PANES_ALL_FORMAT, DEFAULT_LIST_PANES_SESSION_FORMAT,
    DEFAULT_LIST_PANES_WINDOW_FORMAT,
};

#[tokio::test]
async fn runtime_execution_list_panes_preserves_cli_scope_formats_and_indices() {
    for base in [0, 4] {
        let handler = RequestHandler::new();
        run_hook_probe_queue(
            &handler,
            &format!("set-option -g base-index {base} ; set-option -g pane-base-index {base}"),
        )
        .await;
        // The default formats print each pane's history byte count, which a
        // live shell grows between the two listings compared below.
        handler.pin_quiet_default_command_for_test().await;
        for name in ["r2-a", "r2-b"] {
            create_background_identity_session(&handler, session_name(name)).await;
            let pane = {
                let state = handler.state.lock().await;
                let session = state
                    .sessions
                    .session(&session_name(name))
                    .expect("session");
                PaneTarget::with_window(
                    session_name(name),
                    session.active_window_index(),
                    session.active_pane_index(),
                )
            };
            handler.wait_for_pane_startup_output_for_test(&pane).await;
        }
        let target = format!("r2-a:{base}");
        // The existing all-session parser declines an explicit -t. Preserve
        // that before-effect helper boundary; native CLI proof checks output.
        assert_eq!(
            execution_result(
                handler
                    .handle(execution_request(&["list-panes", "-as", "-t", &target]))
                    .await
            ),
            RuntimeCommandExecutionResponse::Fallback
        );
        for (arguments, names, index, format) in [
            (
                vec!["list-panes", "-a"],
                vec!["r2-a", "r2-b"],
                None,
                DEFAULT_LIST_PANES_ALL_FORMAT,
            ),
            (
                vec!["list-panes", "-t", &target],
                vec!["r2-a"],
                Some(base),
                DEFAULT_LIST_PANES_WINDOW_FORMAT,
            ),
            (
                vec!["list-panes", "-s", "-t", &target],
                vec!["r2-a"],
                None,
                DEFAULT_LIST_PANES_SESSION_FORMAT,
            ),
            (
                vec![
                    "list-panes",
                    "-a",
                    "-F",
                    "#{session_name}|#{window_index}|#{pane_index}",
                ],
                vec!["r2-a", "r2-b"],
                None,
                "#{session_name}|#{window_index}|#{pane_index}",
            ),
            (
                vec!["list-panes", "-a", "-F", "old", "-F", ""],
                vec!["r2-a", "r2-b"],
                None,
                "",
            ),
        ] {
            let mut expected = Vec::new();
            for name in names {
                // This is the actual full CLI's typed list request and shared
                // scope default, not output reconstructed from the offer.
                let response = handler
                    .handle(Request::ListPanes(Box::new(rmux_proto::ListPanesRequest {
                        target: session_name(name),
                        target_window_index: index,
                        format: Some(format.to_owned()),
                        filter: None,
                        sort_order: None,
                        reversed: false,
                    })))
                    .await;
                let Response::ListPanes(response) = response else {
                    panic!("full list request: {response:?}")
                };
                expected.extend_from_slice(response.command_output().stdout());
            }
            let actual = execution_result(handler.handle(execution_request(&arguments)).await);
            assert!(
                matches!(actual, RuntimeCommandExecutionResponse::Complete {
                ref stdout, error: None, exit_status: 0, ..
            } if stdout == &expected),
                "{arguments:?}, base={base}: {actual:?}, expected={expected:?}"
            );
            if format == DEFAULT_LIST_PANES_ALL_FORMAT {
                assert!(expected.starts_with(format!("r2-a:{base}.{base}:").as_bytes()));
            }
        }
    }
}
