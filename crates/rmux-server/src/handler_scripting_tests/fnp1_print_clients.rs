use super::*;
use std::{
    future::{poll_fn, Future},
    task::Poll,
};

const CLIENT_FORMAT: &str = "#{client_name}|#{client_pid}|#{client_width}";

async fn control_stdout(handler: &RequestHandler, pid: u32, command: &str) -> Vec<u8> {
    let parsed = handler.parse_control_commands(command).await.unwrap();
    let result = handler.execute_control_commands(pid, parsed).await;
    assert!(result.error.is_none(), "{command}: {result:?}");
    result.stdout
}

async fn clients(
    handler: &RequestHandler,
    session: &str,
    caller: u32,
) -> (
    u64,
    tokio::sync::mpsc::Receiver<ControlServerEvent>,
    tokio::sync::mpsc::Receiver<ControlServerEvent>,
) {
    let (id, first) = register_control_for_session(handler, caller, session_name(session)).await;
    let (_, second) =
        register_control_for_session(handler, caller + 1, session_name(session)).await;
    control_stdout(handler, caller, "refresh-client -C 91x31").await;
    control_stdout(handler, caller + 1, "refresh-client -C 117x41").await;
    (id, first, second)
}

fn expected_client(pid: u32) -> String {
    // Direct tmux 3.7b new-pane and alias measurements: A retains width 91.
    format!("client-{pid}|{pid}|91")
}

#[tokio::test]
async fn fnp1_print_runtime_offer_keeps_invoking_client() {
    let handler = setup().await;
    let caller = std::process::id();
    let (_id, _a, _b) = clients(&handler, "fnp", caller).await;
    let result = offer(
        &handler,
        &[
            "new-pane",
            "-d",
            "-t",
            "fnp:0.0",
            "-x19",
            "-y5",
            "-X3",
            "-Y2",
            "-P",
            "-F",
            CLIENT_FORMAT,
        ],
        true,
    )
    .await;
    cleanup(&handler).await;
    let RuntimeCommandExecutionResponse::Complete { stdout, error, .. } = result else {
        panic!("not executed: {result:?}");
    };
    assert!(error.is_none(), "{error:?}");
    assert_eq!(stdout, format!("{}\n", expected_client(caller)).as_bytes());
}

#[tokio::test]
async fn fnp1_print_control_aliases_keep_invoking_client_in_linked_session() {
    let handler = setup().await;
    create_background_identity_session(&handler, session_name("linked")).await;
    queue(&handler, "link-window -d -s fnp:0 -t linked:1")
        .await
        .unwrap();
    queue(
        &handler,
        "set-option -s command-alias[77] 'printchild=new-pane'",
    )
    .await
    .unwrap();
    let caller = 429_101;
    let (_id, _a, _b) = clients(&handler, "linked", caller).await;
    for command in ["new-pane", "newp", "printchild"] {
        let stdout = control_stdout(
            &handler,
            caller,
            &format!(
            "{command} -d -t linked:1.0 -x19 -y5 -X3 -Y2 -P -F '{CLIENT_FORMAT}|#{{session_name}}'"
        ),
        )
        .await;
        assert_eq!(
            stdout,
            format!("{}|linked\n", expected_client(caller)).as_bytes()
        );
    }
    cleanup(&handler).await;
}

#[tokio::test]
async fn fnp1_print_keeps_invoking_client_after_created_child_moves() {
    let handler = setup().await;
    let caller = 429_201;
    let (_id, _a, _b) = clients(&handler, "fnp", caller).await;
    let pause = Arc::new(PrintPause::default());
    let waiting = Arc::clone(&pause);
    let cloned = handler.clone();
    let pending = tokio::spawn(BEFORE_PRINT.scope(waiting, async move {
        control_stdout(&cloned, caller, &format!(
            "new-pane -d -t fnp:0.0 -x19 -y5 -X3 -Y2 -P -F '{CLIENT_FORMAT}|#{{pane_id}}|#{{pane_index}}'"
        )).await
    }));
    tokio::time::timeout(Duration::from_secs(3), pause.reached.notified())
        .await
        .unwrap();
    let created = window(&handler).await.pane(1).unwrap().id();
    super::super::super::frs1::swap_fixture_identities(
        &mut handler.state.lock().await.sessions,
        &WindowTarget::with_window(session_name("fnp"), 0),
        0,
        1,
    );
    pause.release.notify_one();
    let stdout = tokio::time::timeout(Duration::from_secs(3), pending)
        .await
        .unwrap()
        .unwrap();
    cleanup(&handler).await;
    assert_eq!(
        stdout,
        format!("{}|%{}|0\n", expected_client(caller), created.as_u32()).as_bytes()
    );
}

#[tokio::test]
async fn fnp1_print_retry_keeps_invoking_client_after_target_slot_moves() {
    let handler = setup().await;
    queue(&handler, "new-pane -d -t fnp:0.0 -x19 -y5 -X3 -Y2")
        .await
        .unwrap();
    let caller = 429_301;
    let (id, _a, _b) = clients(&handler, "fnp", caller).await;
    let created = window(&handler).await.pane(1).unwrap().id();
    let held = handler.active_control.lock().await;
    assert!(handler.state.try_lock().is_ok());
    let mut printing = Box::pin(with_control_queue_identity(
        ControlClientIdentity::new(caller, id),
        handler.handle_display_message_for_stable_pane(
            caller,
            created,
            DisplayMessageRequest {
                target: Some(Target::Pane(PaneTarget::with_window(
                    session_name("fnp"),
                    0,
                    1,
                ))),
                print: true,
                message: Some(format!("{CLIENT_FORMAT}|#{{pane_id}}|#{{pane_index}}")),
                empty_target_context: false,
            },
        ),
    ));
    // The state lock is free. The first poll resolves slot 1, then blocks on
    // is_control_client's held lock, before validating the captured PaneId.
    poll_fn(|cx| {
        assert!(printing.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    super::super::super::frs1::swap_fixture_identities(
        &mut handler.state.lock().await.sessions,
        &WindowTarget::with_window(session_name("fnp"), 0),
        0,
        1,
    );
    drop(held);
    let response = tokio::time::timeout(Duration::from_secs(3), printing)
        .await
        .unwrap();
    cleanup(&handler).await;
    let output = response
        .command_output()
        .expect("retry prints the moved identity");
    assert_eq!(
        output.stdout(),
        format!("{}|%{}|0\n", expected_client(caller), created.as_u32()).as_bytes()
    );
}

#[tokio::test]
async fn fnp1_nonprinted_stable_message_broadcasts_to_target_controls() {
    let handler = setup().await;
    create_background_identity_session(&handler, session_name("other")).await;
    let caller = 429_401;
    let (_id, mut a, mut b) = clients(&handler, "fnp", caller).await;
    let (other_id, mut other) =
        register_control_for_session(&handler, caller + 2, session_name("other")).await;
    for receiver in [&mut a, &mut b, &mut other] {
        while receiver.try_recv().is_ok() {}
    }
    let pane_id = window(&handler).await.pane(0).unwrap().id();
    let response = with_control_queue_identity(
        ControlClientIdentity::new(caller + 2, other_id),
        handler.handle_display_message_for_stable_pane(
            caller + 2,
            pane_id,
            DisplayMessageRequest {
                target: Some(Target::Pane(PaneTarget::with_window(
                    session_name("fnp"),
                    0,
                    0,
                ))),
                print: false,
                message: Some("S3290_RS".to_owned()),
                empty_target_context: false,
            },
        ),
    )
    .await;
    assert!(
        matches!(response, Response::DisplayMessage(_)),
        "{response:?}"
    );
    for receiver in [&mut a, &mut b] {
        assert!(std::iter::from_fn(|| receiver.try_recv().ok()).any(|event|
            matches!(event, ControlServerEvent::Notification(line) if line == "%message S3290_RS")
        ), "each target control must receive the nonprinted message");
    }
    assert!(!std::iter::from_fn(|| other.try_recv().ok()).any(|event|
        matches!(event, ControlServerEvent::Notification(line) if line == "%message S3290_RS")
    ));
    cleanup(&handler).await;
}
