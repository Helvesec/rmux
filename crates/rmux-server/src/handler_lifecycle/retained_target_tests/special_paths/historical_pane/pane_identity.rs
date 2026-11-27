use super::*;

async fn ordinary_command(
    handler: &RequestHandler,
    session: &SessionName,
    command: &str,
) -> Result<(), rmux_proto::RmuxError> {
    handler
        .execute_hook_command_with_target_binding(
            std::process::id(),
            command,
            Some(Target::Session(session.clone())),
            None,
        )
        .await
}

#[tokio::test]
async fn pane_base_one_preserves_explicit_identity_in_historical_and_ordinary_children() {
    let mut violations = Vec::new();
    for entry in ["if-shell", "run-shell", "source-file"] {
        for historical in [true, false] {
            for (two_panes, qualified) in [(true, false), (true, true), (false, false)] {
                for mutation in ["kill-pane", "select-pane -m"] {
                    let handler = RequestHandler::new();
                    let session =
                        tombstone::session_with_native_spare_window(&handler, "pane-base-one")
                            .await;
                    if two_panes {
                        ordinary_command(
                            &handler,
                            &session,
                            &format!(
                                "split-window -d -t {session}:1.0 {}",
                                crate::test_shell::command_quote(
                                    &crate::test_shell::stdin_discard_command()
                                )
                            ),
                        )
                        .await
                        .expect("create second native pane");
                    }
                    let index = u32::from(two_panes);
                    let (first_id, requested_id) = {
                        let state = handler.state.lock().await;
                        let window = state
                            .sessions
                            .session(&session)
                            .unwrap()
                            .window_at(1)
                            .unwrap();
                        (
                            window.pane(0).unwrap().id(),
                            window.pane(index).unwrap().id(),
                        )
                    };
                    ordinary_command(
                        &handler,
                        &session,
                        &format!("set-window-option -t {session}:1 pane-base-index 1"),
                    )
                    .await
                    .expect("set visible pane base");
                    let target = if qualified {
                        format!("{session}:1.{}", index + 1)
                    } else {
                        requested_id.to_string()
                    };
                    let commands = format!("{mutation} -t {target}");
                    let (command, source) = tombstone::wrapped_command(entry, &commands).await;
                    let result = if historical {
                        tombstone::execute_retired_child_command(&handler, &session, &command)
                            .await
                            .map(|_| ())
                    } else {
                        ordinary_command(&handler, &session, &command).await
                    };
                    if let Some(source) = source {
                        std::fs::remove_file(source).expect("remove sourced fixture");
                    }
                    let state = handler.state.lock().await;
                    let window = state
                        .sessions
                        .session(&session)
                        .and_then(|s| s.window_at(1));
                    let contains = |id| {
                        window
                            .is_some_and(|window| window.panes().iter().any(|pane| pane.id() == id))
                    };
                    let correct = if mutation == "kill-pane" {
                        !contains(requested_id) && (!two_panes || contains(first_id))
                    } else {
                        contains(requested_id)
                            && state.marked_pane_target()
                                == Some(PaneTarget::with_window(session.clone(), 1, index))
                    };
                    if result.is_err() || !correct {
                        violations.push(format!(
                            "{entry} historical={historical} two_panes={two_panes} \
                             qualified={qualified} {commands}: {result:?}; \
                             first={} requested={} marked={:?}",
                            contains(first_id),
                            contains(requested_id),
                            state.marked_pane_target()
                        ));
                    }
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "pane identity changed: {violations:#?}"
    );
}
