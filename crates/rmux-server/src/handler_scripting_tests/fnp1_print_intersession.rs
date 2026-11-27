use super::*;

const FORMAT: &str = "#{client_name}|#{client_pid}|#{client_width}|#{session_name}";

async fn control_stdout(handler: &RequestHandler, pid: u32, command: &str) -> Vec<u8> {
    let parsed = handler.parse_control_commands(command).await.unwrap();
    let result = handler.execute_control_commands(pid, parsed).await;
    assert!(result.error.is_none(), "{command}: {result:?}");
    result.stdout
}

#[tokio::test]
async fn fnp1_print_intersession_control_keeps_invoking_client() {
    let handler = setup().await;
    create_background_identity_session(&handler, session_name("linked")).await;
    let caller = 429_501;
    let (_id, _a) = register_control_for_session(&handler, caller, session_name("fnp")).await;
    let (_peer_id, _b) =
        register_control_for_session(&handler, caller + 1, session_name("linked")).await;
    control_stdout(&handler, caller, "refresh-client -C 91x31").await;
    control_stdout(&handler, caller + 1, "refresh-client -C 117x41").await;
    let stdout = control_stdout(
        &handler,
        caller,
        &format!("new-pane -d -t linked:0.0 -x19 -y5 -X3 -Y2 -P -F '{FORMAT}'"),
    )
    .await;
    cleanup(&handler).await;
    assert_eq!(
        stdout,
        format!("client-{caller}|{caller}|91|linked\n").as_bytes(),
        "the caller's client context survives an independent target session"
    );
}

async fn fixture(
    caller: u32,
) -> (
    RequestHandler,
    tokio::sync::mpsc::Receiver<ControlServerEvent>,
    tokio::sync::mpsc::Receiver<ControlServerEvent>,
) {
    let handler = setup().await;
    create_background_identity_session(&handler, session_name("linked")).await;
    let (_, a) = register_control_for_session(&handler, caller, session_name("fnp")).await;
    let (_, b) = register_control_for_session(&handler, caller + 1, session_name("linked")).await;
    control_stdout(&handler, caller, "refresh-client -C 91x31").await;
    control_stdout(&handler, caller + 1, "refresh-client -C 117x41").await;
    queue(
        &handler,
        "set-option -s command-alias[77] 'printchild=new-pane'",
    )
    .await
    .unwrap();
    (handler, a, b)
}

#[tokio::test]
async fn fnp1_print_intersession_control_aliases_keep_both_callers() {
    let caller = 429_601;
    let (handler, _a, _b) = fixture(caller).await;
    // tmux 3.7b: both intersession directions and the same-session neighbor
    // keep the initiating client; session_name still describes the new pane.
    for (pid, width, session) in [
        (caller, 91, "linked"),
        (caller + 1, 117, "fnp"),
        (caller, 91, "fnp"),
    ] {
        for command in ["new-pane", "newp", "printchild"] {
            let stdout = control_stdout(
                &handler,
                pid,
                &format!("{command} -d -t {session}:0.0 -x19 -y5 -X3 -Y2 -P -F '{FORMAT}'"),
            )
            .await;
            assert_eq!(
                stdout,
                format!("client-{pid}|{pid}|{width}|{session}\n").as_bytes()
            );
        }
    }
    cleanup(&handler).await;
}

#[tokio::test]
async fn fnp1_print_intersession_runtime_offer_keeps_invoking_client() {
    let caller = std::process::id();
    let (handler, _a, _b) = fixture(caller).await;
    for command in ["new-pane", "newp", "printchild"] {
        let result = offer(
            &handler,
            &[
                command,
                "-d",
                "-t",
                "linked:0.0",
                "-x19",
                "-y5",
                "-X3",
                "-Y2",
                "-P",
                "-F",
                FORMAT,
            ],
            true,
        )
        .await;
        let RuntimeCommandExecutionResponse::Complete { stdout, error, .. } = result else {
            panic!("not executed: {result:?}");
        };
        assert!(error.is_none(), "{error:?}");
        assert_eq!(
            stdout,
            format!("client-{caller}|{caller}|91|linked\n").as_bytes()
        );
    }
    cleanup(&handler).await;
}

#[tokio::test]
async fn fnp1_print_intersession_without_caller_keeps_target_fallback() {
    let caller = 429_701;
    let (handler, _a, _b) = fixture(caller).await;
    create_background_identity_session(&handler, session_name("other")).await;
    let (_, _c) = register_control_for_session(&handler, caller + 2, session_name("other")).await;
    control_stdout(&handler, caller + 2, "refresh-client -C 133x43").await;
    let result = offer(
        &handler,
        &[
            "new-pane",
            "-d",
            "-t",
            "linked:0.0",
            "-x19",
            "-y5",
            "-X3",
            "-Y2",
            "-P",
            "-F",
            FORMAT,
        ],
        true,
    )
    .await;
    cleanup(&handler).await;
    let RuntimeCommandExecutionResponse::Complete { stdout, error, .. } = result else {
        panic!("not executed: {result:?}");
    };
    assert!(error.is_none(), "{error:?}");
    let peer = caller + 1;
    // Preserve the existing target-session fallback when no caller resolves,
    // including when another session has the most recently active client.
    assert_eq!(
        stdout,
        format!("client-{peer}|{peer}|117|linked\n").as_bytes()
    );
}

#[tokio::test]
async fn fnp1_print_intersession_without_clients_has_empty_client_formats() {
    let handler = setup().await;
    create_background_identity_session(&handler, session_name("linked")).await;
    let result = offer(
        &handler,
        &[
            "new-pane",
            "-d",
            "-t",
            "linked:0.0",
            "-x19",
            "-y5",
            "-X3",
            "-Y2",
            "-P",
            "-F",
            FORMAT,
        ],
        true,
    )
    .await;
    cleanup(&handler).await;
    let RuntimeCommandExecutionResponse::Complete { stdout, error, .. } = result else {
        panic!("not executed: {result:?}");
    };
    assert!(error.is_none(), "{error:?}");
    assert_eq!(stdout, b"|||linked\n");
}

#[tokio::test]
async fn fnp1_print_intersession_leaves_display_message_target_client() {
    let caller = 429_801;
    let (handler, _a, _b) = fixture(caller).await;
    let stdout = control_stdout(
        &handler,
        caller,
        &format!("display-message -p -t linked:0.0 '{FORMAT}'"),
    )
    .await;
    cleanup(&handler).await;
    let peer = caller + 1;
    assert_eq!(
        stdout,
        format!("client-{peer}|{peer}|117|linked\n").as_bytes()
    );
}
