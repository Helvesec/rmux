//! The pane a request names and the panes it only reaches read one poisoned
//! transcript under two policies.
//!
//! display-message names the pane of its target and answers
//! "pane %N unavailable", as the pane's own operations do. A listing, loop or
//! session line only reaches its panes: list-panes, whose target is a session
//! or a window and which the CLI sends once per session for list-panes -a,
//! still lists the poisoned pane, with its transcript values empty. Each half
//! fails without the other: the typed error alone fails the listings, the
//! observation alone answers the named pane with an empty expansion.
//!
//! With pane-scrollbars on, the pane geometry reads the transcript too: the
//! scrollbar hides on the alternate screen. The named pane's width fails as
//! its transcript values do; a listing observes the poisoned pane as neither
//! alternate nor copying and lists its width less the scrollbar column.

use super::*;
use rmux_proto::{RuntimeCommandExecutionRequest, RuntimeCommandExecutionResponse};

/// Runs one command line in its own task, as one daemon connection: its
/// stdout on success, otherwise the message its client reads.
pub(super) async fn answer(handler: &RequestHandler, arguments: &[&str]) -> Result<String, String> {
    let worker = handler.clone();
    let request = Request::RuntimeCommandExecution(Box::new(RuntimeCommandExecutionRequest::new(
        arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect(),
        None,
    )));
    let task = tokio::spawn(async move { worker.handle(request).await });
    match tokio::time::timeout(Duration::from_secs(10), task).await {
        Err(_) => Err("no response within 10 s".to_owned()),
        Ok(Err(error)) if error.is_panic() => Err("panicked: connection closed".to_owned()),
        Ok(Err(error)) => Err(format!("task failed: {error}")),
        Ok(Ok(Response::Error(ErrorResponse {
            error: RmuxError::Message(message),
        }))) => Err(message),
        Ok(Ok(Response::RuntimeCommandExecution(result))) => match *result {
            RuntimeCommandExecutionResponse::Complete {
                error: None,
                exit_status: 0,
                stdout,
                ..
            } => Ok(String::from_utf8(stdout).expect("utf-8 stdout")),
            RuntimeCommandExecutionResponse::Complete {
                error: Some(RmuxError::Message(message)),
                ..
            } => Err(message),
            other => Err(format!("{other:?}")),
        },
        Ok(Ok(other)) => Err(format!("{other:?}")),
    }
}

#[tokio::test]
async fn poisoned_pane_answers_unavailable_where_named_and_lists_empty_where_reached() {
    let handler = RequestHandler::new();
    // A quiet child keeps the live pane readers out of this test's timing.
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::DefaultCommand,
                value: crate::test_shell::stdin_discard_command(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    for name in ["poisoned", "healthy"] {
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session_name(name),
                    detached: true,
                    size: None,
                    environment: None,
                }))
                .await,
        );
    }
    let pane_id = answer(
        &handler,
        &["display-message", "-p", "-t", "poisoned:0.0", "#{pane_id}"],
    )
    .await
    .expect("pane id of the pane to poison");
    let pane_id = pane_id.trim_end();
    // Set before the poisoning: a geometry option resizes every session.
    assert_eq!(
        answer(&handler, &["set-option", "-g", "pane-scrollbars", "on"]).await,
        Ok(String::new())
    );
    // 80 columns less the scrollbar column the transcript state shows.
    assert_eq!(
        answer(
            &handler,
            &[
                "display-message",
                "-p",
                "-t",
                "poisoned:0.0",
                "#{pane_width}"
            ],
        )
        .await,
        Ok("79\n".to_owned())
    );
    // The transcript alone, as a recovered pane reader leaves it.
    handler
        .poison_pane_transcript_for_test(&PaneTarget::with_window(session_name("poisoned"), 0, 0))
        .await;

    let unavailable = Err(format!("pane {pane_id} unavailable"));
    for named in [
        &[
            "display-message",
            "-p",
            "-t",
            "poisoned:0.0",
            "#{pane_title}",
        ][..],
        &[
            "display-message",
            "-p",
            "-t",
            "poisoned:0",
            "#{history_size}",
        ],
        &["display-message", "-p", "-t", "poisoned", "#{pane_in_mode}"],
        &[
            "display-message",
            "-p",
            "-t",
            "poisoned:0.0",
            "#{pane_width}",
        ],
    ] {
        assert_eq!(answer(&handler, named).await, unavailable, "{named:?}");
    }
    // The CLI resolves -t to a pane; a typed request may name the session or
    // the window, whose active pane display-message reads.
    for target in [
        Target::Session(session_name("poisoned")),
        Target::Window(rmux_proto::WindowTarget::with_window(
            session_name("poisoned"),
            0,
        )),
    ] {
        let response = handler
            .handle(Request::DisplayMessage(DisplayMessageRequest {
                target: Some(target.clone()),
                print: true,
                message: Some("#{pane_title}".to_owned()),
                empty_target_context: false,
            }))
            .await;
        assert_eq!(
            response,
            Response::Error(ErrorResponse {
                error: RmuxError::Message(format!("pane {pane_id} unavailable")),
            }),
            "{target:?}"
        );
    }

    let listed_empty = format!("{pane_id}=");
    let listed_width = format!("{pane_id}=79");
    for (reached, line) in [
        (
            &[
                "list-panes",
                "-t",
                "poisoned",
                "-F",
                "#{pane_id}=#{pane_width}",
            ][..],
            listed_width.as_str(),
        ),
        (
            &["list-panes", "-a", "-F", "#{pane_id}=#{history_limit}"],
            listed_empty.as_str(),
        ),
        (
            &[
                "list-panes",
                "-t",
                "poisoned",
                "-F",
                "#{pane_id}=#{pane_title}",
            ],
            listed_empty.as_str(),
        ),
        (
            &[
                "list-panes",
                "-s",
                "-t",
                "poisoned",
                "-F",
                "#{pane_id}=#{history_bytes}",
            ],
            listed_empty.as_str(),
        ),
        (
            &["list-sessions", "-F", "#{session_name}=#{history_limit}"],
            "poisoned=",
        ),
        (
            &[
                "display-message",
                "-p",
                "-t",
                "healthy",
                "#{S:#{session_name}=#{history_limit} }",
            ],
            "poisoned=",
        ),
        // A loop builds its own contexts: even from the named pane, it only
        // reaches the panes it walks.
        (
            &[
                "display-message",
                "-p",
                "-t",
                "poisoned",
                "#{S:#{session_name}=#{history_limit} }",
            ],
            "poisoned=",
        ),
    ] {
        let answered = answer(&handler, reached).await;
        let Ok(stdout) = &answered else {
            panic!("{reached:?} must stay served: {answered:?}");
        };
        assert!(
            stdout.split(['\n', ' ']).any(|listed| listed == line),
            "{reached:?} lists the poisoned pane as {line:?}: {stdout:?}"
        );
    }
    // The default listing reads the transcript on its fast path and falls
    // back to the runtime format for the poisoned pane.
    let default_listing = answer(&handler, &["list-panes", "-t", "poisoned"]).await;
    assert!(
        default_listing
            .as_ref()
            .is_ok_and(|stdout| stdout.contains(&format!("[history /,  bytes] {pane_id}"))),
        "{default_listing:?}"
    );

    let healthy = answer(
        &handler,
        &["display-message", "-p", "-t", "healthy", "#{history_limit}"],
    )
    .await
    .expect("the healthy pane stays served");
    assert!(!healthy.trim_end().is_empty(), "{healthy:?}");
}
