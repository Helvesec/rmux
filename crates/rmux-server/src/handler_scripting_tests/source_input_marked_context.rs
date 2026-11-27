use super::fpc1_junction::{canonical_request, quiet_fixture, stale_requester};
use super::frs1::{cleanup, queue, window};
use super::*;
use rmux_proto::{SourceFileInputV1Request, SourceFileResponse};

fn input_request(root: &Path, arguments: &[&str], queue_status: bool) -> Request {
    let request = SourceFileInputV1Request::new(
        arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect(),
        Some(root.to_path_buf()),
        Some(vec![0, 255]),
    );
    Request::SourceFileInputV1(Box::new(if queue_status {
        request.requiring_queue_status()
    } else {
        request
    }))
}

fn source_response(response: Response, queue_status: bool) -> SourceFileResponse {
    match response {
        Response::SourceFileQueueV1(result) if queue_status => {
            assert!(
                !result.stop_queue,
                "selected source must retain the caller's queue"
            );
            result.source
        }
        Response::SourceFile(result) if !queue_status => result,
        response => panic!("unexpected source response: {response:?}"),
    }
}

#[tokio::test]
async fn source_input_live_mark_defers_stale_current_for_transfer_neighbors() {
    for entry in ["plain", "queue-status", "alias", "nested", "canonical"] {
        for command in [
            "swap-pane -d -t junction-decoy:0.0",
            "join-pane -d -t junction-decoy:0.0",
            "move-pane -d -t junction-decoy:0.0",
            "swap-window -d -t junction-decoy:0",
        ] {
            let (handler, primary, _linked, root, valid, _events) = quiet_fixture().await;
            let before = window(&handler, &primary).await;
            let source_id = before.pane(0).unwrap().id();
            queue(&handler, "select-pane -m -t frs-sibling-primary:0.0")
                .await
                .unwrap();
            queue(
                &handler,
                "set-option -s command-alias[20] markedsource=source-file",
            )
            .await
            .unwrap();
            let (stale, _stale_events) = stale_requester(&handler, &primary, &root).await;
            fs::write(root.join("child.conf"), format!("{command}\n")).unwrap();
            fs::write(root.join("outer.conf"), "source-file child.conf\n").unwrap();
            let arguments = [
                if entry == "alias" {
                    "markedsource"
                } else {
                    "source-file"
                },
                if entry == "nested" {
                    "outer.conf"
                } else {
                    "child.conf"
                },
            ];
            let negotiated = !matches!(entry, "plain" | "canonical");
            let request = if entry == "canonical" {
                let parsed = CommandParser::new()
                    .parse_arguments_with_assignments(&[
                        "source-file".to_owned(),
                        root.join("child.conf").to_string_lossy().into_owned(),
                    ])
                    .unwrap();
                canonical_request(&parsed.to_tmux_reparse_string())
            } else {
                input_request(&root, &arguments, negotiated)
            };
            let response = handler.dispatch(stale.0.id(), request).await.response;
            let result = source_response(response, negotiated);
            assert_eq!(
                result.exit_status().unwrap_or(0),
                0,
                "{entry}: {command}: {result:?}"
            );
            assert!(result.stderr().is_empty(), "{entry}: {command}: {result:?}");
            let destination = window(&handler, &session_name("junction-decoy")).await;
            if command.starts_with("swap-window") {
                assert_eq!(destination.id(), before.id(), "{entry}: {command}");
            } else {
                assert!(
                    destination
                        .panes()
                        .iter()
                        .any(|pane| pane.id() == source_id),
                    "{entry}: {command}"
                );
            }
            cleanup(&handler).await;
            drop(stale);
            drop(valid);
            fs::remove_dir_all(root).unwrap();
        }
    }
}

#[tokio::test]
async fn source_input_mark_does_not_hide_current_consumers_or_explicit_relative_targets() {
    for scenario in [
        "missing",
        "cleared",
        "removed",
        "relative-command",
        "current-consumer",
        "relative-source",
        "compact-relative-source",
        "cleared-in-source",
    ] {
        let (handler, primary, _linked, root, valid, _events) = quiet_fixture().await;
        if scenario != "missing" {
            queue(&handler, "select-pane -m -t frs-sibling-primary:0.0")
                .await
                .unwrap();
        }
        match scenario {
            "cleared" => {
                queue(&handler, "select-pane -M -t frs-sibling-primary:0.0")
                    .await
                    .unwrap();
            }
            "removed" => {
                queue(&handler, "select-pane -m -t frs-sibling-primary:0.1")
                    .await
                    .unwrap();
                queue(&handler, "kill-pane -t frs-sibling-primary:0.1")
                    .await
                    .unwrap();
            }
            _ => {}
        }
        let (stale, _stale_events) = stale_requester(&handler, &primary, &root).await;
        let primary_before = window(&handler, &primary).await;
        let decoy = session_name("junction-decoy");
        let decoy_before = window(&handler, &decoy).await;
        let body = match scenario {
            "relative-command" => "swap-pane -d -t .0\n",
            "current-consumer" => "link-window -d -t junction-decoy:2\n",
            "cleared-in-source" => {
                "select-pane -M -t frs-sibling-primary:0.0\nswap-pane -d -t junction-decoy:0.0\n"
            }
            _ => "swap-pane -d -t junction-decoy:0.0\n",
        };
        fs::write(root.join("input.conf"), body).unwrap();
        let arguments = match scenario {
            "relative-source" => vec!["source-file", "-t", ".", "input.conf"],
            "compact-relative-source" => vec!["source-file", "-qt.", "input.conf"],
            _ => vec!["source-file", "input.conf"],
        };
        let response = handler
            .dispatch(stale.0.id(), input_request(&root, &arguments, true))
            .await
            .response;
        let result = source_response(response, true);
        assert_eq!(result.exit_status(), Some(1), "{scenario}: {result:?}");
        assert!(
            String::from_utf8_lossy(result.stderr())
                .contains("is not present in inherited session"),
            "{scenario}: {result:?}"
        );
        assert_eq!(
            window(&handler, &primary).await,
            primary_before,
            "{scenario}"
        );
        assert_eq!(window(&handler, &decoy).await, decoy_before, "{scenario}");
        assert!(handler
            .state
            .lock()
            .await
            .sessions
            .session(&decoy)
            .unwrap()
            .window_at(2)
            .is_none());
        if scenario == "cleared-in-source" {
            assert!(handler.state.lock().await.marked_pane_target().is_none());
        }
        cleanup(&handler).await;
        drop(stale);
        drop(valid);
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn source_input_preserves_valid_inherited_and_explicit_source_context() {
    for explicit in [false, true] {
        let (handler, primary, _linked, root, valid, _events) = quiet_fixture().await;
        // The mark differs from current: a current consumer must still use the
        // inherited or explicitly selected pane, never borrow the mark.
        queue(&handler, "select-pane -m -t junction-decoy:0.0")
            .await
            .unwrap();
        let source = window(&handler, &primary).await.pane(0).unwrap().id();
        let (stale, _stale_events) = stale_requester(&handler, &primary, &root).await;
        fs::write(root.join("input.conf"), "display-message -p '#{pane_id}'\n").unwrap();
        let arguments = if explicit {
            vec!["source-file", "-t", "frs-sibling-primary:0.0", "input.conf"]
        } else {
            vec!["source-file", "input.conf"]
        };
        let response = handler
            .dispatch(
                if explicit { stale.0.id() } else { valid.0.id() },
                input_request(&root, &arguments, true),
            )
            .await
            .response;
        let result = source_response(response, true);
        assert_eq!(result.exit_status().unwrap_or(0), 0, "{result:?}");
        assert!(result.stderr().is_empty(), "{result:?}");
        assert_eq!(
            result.command_output().unwrap().stdout(),
            format!("{source}\n").as_bytes()
        );
        cleanup(&handler).await;
        drop(stale);
        drop(valid);
        fs::remove_dir_all(root).unwrap();
    }
}
