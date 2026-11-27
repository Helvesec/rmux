//! if-shell -F and run-shell name the pane of their target, as
//! display-message does: a poisoned transcript answers "pane %N unavailable"
//! where the command names the pane, and expands empty where it is only
//! reached. A hook runs beyond its request's boundary, so a hook that names
//! the poisoned pane observes it and never fails the request that fired it.

use super::poisoned_transcript_reach::answer;
use super::*;

async fn poisoned_and_healthy_sessions(handler: &RequestHandler) -> String {
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
        handler,
        &["display-message", "-p", "-t", "poisoned:0.0", "#{pane_id}"],
    )
    .await
    .expect("pane id of the pane to poison");
    handler
        .poison_pane_transcript_for_test(&PaneTarget::with_window(session_name("poisoned"), 0, 0))
        .await;
    pane_id.trim_end().to_owned()
}

#[tokio::test]
async fn poisoned_pane_answers_unavailable_to_if_shell_and_run_shell_that_name_it() {
    let handler = RequestHandler::new();
    let pane_id = poisoned_and_healthy_sessions(&handler).await;

    let unavailable = Err(format!("pane {pane_id} unavailable"));
    for named in [
        &[
            "if-shell",
            "-F",
            "-t",
            "poisoned:0.0",
            "#{history_size}",
            "display-message -p yes",
            "display-message -p no",
        ][..],
        &[
            "if-shell",
            "-F",
            "-t",
            "poisoned",
            "#{pane_in_mode}",
            "display-message -p yes",
            "display-message -p no",
        ],
        &[
            "run-shell",
            "-t",
            "poisoned:0.0",
            "echo history=#{history_size}",
        ],
        &[
            "run-shell",
            "-C",
            "-t",
            "poisoned:0.0",
            "set-option -g @run_history #{history_size}",
        ],
    ] {
        assert_eq!(answer(&handler, named).await, unavailable, "{named:?}");
    }

    // Variables that do not read the transcript, the healthy pane and the
    // panes a loop or a listing only reaches stay served.
    for (served, stdout) in [
        (
            &[
                "if-shell",
                "-F",
                "-t",
                "poisoned:0.0",
                "#{pane_id}",
                "display-message -p yes",
                "display-message -p no",
            ][..],
            "yes\n".to_owned(),
        ),
        (
            &[
                "if-shell",
                "-F",
                "-t",
                "healthy",
                "#{history_limit}",
                "display-message -p yes",
                "display-message -p no",
            ],
            "yes\n".to_owned(),
        ),
        (
            &[
                "if-shell",
                "-F",
                "-t",
                "healthy",
                "#{S:#{history_limit}}",
                "display-message -p yes",
                "display-message -p no",
            ],
            "yes\n".to_owned(),
        ),
        (
            &[
                "list-windows",
                "-t",
                "poisoned",
                "-F",
                "#{window_index}:#{history_size}",
            ],
            "0:\n".to_owned(),
        ),
    ] {
        assert_eq!(answer(&handler, served).await, Ok(stdout), "{served:?}");
    }
    for (command, mark) in [
        ("set-option -g @run_history #{history_limit}", "2000\n"),
        ("set-option -g @run_pane #{pane_id}", "%1\n"),
    ] {
        assert_eq!(
            answer(&handler, &["run-shell", "-C", "-t", "healthy:0.0", command]).await,
            Ok(String::new()),
            "{command:?}"
        );
        let option = command.split(' ').nth(2).expect("option name");
        assert_eq!(
            answer(&handler, &["show-options", "-gv", option]).await,
            Ok(mark.to_owned()),
            "{command:?}"
        );
    }
}

#[tokio::test]
async fn hook_naming_a_poisoned_pane_observes_it_and_never_fails_its_request() {
    let handler = RequestHandler::new();
    poisoned_and_healthy_sessions(&handler).await;
    answer(
        &handler,
        &[
            "set-hook",
            "-g",
            "after-rename-window",
            "if-shell -F -t poisoned:0.0 '#{history_limit}' \
             'set -g @poisoned_hook named' 'set -g @poisoned_hook reached'",
        ],
    )
    .await
    .expect("set-hook");

    // One queued command line, then one plain request: both run the hook
    // after their command, beyond the request boundary.
    assert_eq!(
        answer(&handler, &["rename-window", "-t", "healthy:0", "queued"]).await,
        Ok(String::new())
    );
    assert_eq!(
        answer(&handler, &["show-options", "-gv", "@poisoned_hook"]).await,
        Ok("reached\n".to_owned())
    );
    answer(&handler, &["set-option", "-gu", "@poisoned_hook"])
        .await
        .expect("unset the hook's mark");

    let response = handler
        .handle(Request::RenameWindow(rmux_proto::RenameWindowRequest {
            target: rmux_proto::WindowTarget::with_window(session_name("healthy"), 0),
            name: "plain".to_owned(),
        }))
        .await;
    assert!(
        matches!(response, Response::RenameWindow(_)),
        "{response:?}"
    );
    assert_eq!(
        answer(&handler, &["show-options", "-gv", "@poisoned_hook"]).await,
        Ok("reached\n".to_owned())
    );
}

/// The CLI resolves `-t poisoned` to the session's active pane, so only a raw
/// request reaches the session and window branches of the named target: each
/// answers "pane %N unavailable" for the active pane it reads, and keeps the
/// variables that do not read the transcript served.
#[tokio::test]
async fn poisoned_pane_answers_unavailable_to_if_shell_naming_its_session_or_window() {
    let handler = RequestHandler::new();
    let pane_id = poisoned_and_healthy_sessions(&handler).await;
    let if_shell = |target: &rmux_proto::Target, condition: &str| {
        Request::IfShell(Box::new(rmux_proto::IfShellRequest {
            condition: condition.to_owned(),
            format_mode: true,
            then_command: "display-message -p yes".to_owned(),
            else_command: Some("display-message -p no".to_owned()),
            target: Some(target.clone()),
            caller_cwd: None,
            background: false,
        }))
    };
    let unavailable = Response::Error(ErrorResponse {
        error: RmuxError::Message(format!("pane {pane_id} unavailable")),
    });

    for target in [
        rmux_proto::Target::Session(session_name("poisoned")),
        rmux_proto::Target::Window(rmux_proto::WindowTarget::with_window(
            session_name("poisoned"),
            0,
        )),
    ] {
        assert_eq!(
            handler.handle(if_shell(&target, "#{history_size}")).await,
            unavailable,
            "{target:?}"
        );
        let served = handler.handle(if_shell(&target, "#{pane_id}")).await;
        assert_eq!(
            served
                .command_output()
                .map(|output| output.stdout().to_vec()),
            Some(b"yes\n".to_vec()),
            "{target:?}: {served:?}"
        );
    }
}

/// A background run-shell renders its command beyond any request boundary:
/// naming the poisoned pane there observes it, as a hook does, and never
/// panics the task that runs it.
#[tokio::test]
async fn background_run_shell_naming_a_poisoned_pane_observes_it() {
    let handler = RequestHandler::new();
    poisoned_and_healthy_sessions(&handler).await;
    assert_eq!(
        answer(
            &handler,
            &[
                "run-shell",
                "-b",
                "-C",
                "-t",
                "poisoned:0.0",
                "set-option -g @background_mark x#{history_size}",
            ],
        )
        .await,
        Ok(String::new())
    );
    for _ in 0..200 {
        match answer(&handler, &["show-options", "-gv", "@background_mark"]).await {
            Ok(value) if !value.is_empty() => {
                assert_eq!(value, "x\n");
                return;
            }
            _ => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
        }
    }
    panic!("the background run-shell never set @background_mark");
}
