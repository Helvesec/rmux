//! `new-session` run by an attached client moves that client into the new
//! session, as tmux 3.7b `cmd_new_session` does through
//! `server_client_set_session`. The four entry paths are the ones measured
//! against tmux 3.7b: a direct key binding, a `command-prompt` template, the
//! `:` prompt and a `{ ... }` block.

use super::*;

async fn attached_session_of(handler: &RequestHandler, requester_pid: u32) -> SessionName {
    handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get(&requester_pid)
        .expect("attached client remains registered")
        .session_name
        .clone()
}

async fn wait_for_attached_session(
    handler: &RequestHandler,
    requester_pid: u32,
    expected: &SessionName,
) {
    let deadline = tokio::time::Instant::now() + ATTACH_LIFECYCLE_TIMEOUT;
    loop {
        let current = attached_session_of(handler, requester_pid).await;
        let created = handler
            .state
            .lock()
            .await
            .sessions
            .contains_session(expected);
        if created && &current == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "client must enter {expected} after new-session; created={created}, client is on {current}"
        );
        sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_tail_buffer(handler: &RequestHandler, name: &str) {
    let deadline = tokio::time::Instant::now() + ATTACH_LIFECYCLE_TIMEOUT;
    loop {
        let response = handler
            .handle(Request::ShowBuffer(rmux_proto::ShowBufferRequest {
                name: Some(name.to_owned()),
            }))
            .await;
        if response
            .command_output()
            .is_some_and(|output| output.stdout() == b"done")
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for queue tail {name:?}; last response: {response:?}"
        );
        sleep(Duration::from_millis(10)).await;
    }
}

async fn bind_prefix_key(handler: &RequestHandler, key: &str, command: &[&str]) {
    let response = handler
        .handle(Request::BindKey(Box::new(rmux_proto::BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: key.to_owned(),
            note: None,
            repeat: false,
            command: Some(command.iter().map(|word| (*word).to_owned()).collect()),
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)), "{response:?}");
}

async fn window_count(handler: &RequestHandler, session: &SessionName) -> usize {
    handler
        .state
        .lock()
        .await
        .sessions
        .session(session)
        .expect("session survives")
        .windows()
        .count()
}

#[tokio::test]
async fn attached_binding_new_session_moves_the_client() {
    let handler = RequestHandler::new();
    let requester_pid = u32::MAX - 141;
    let alpha = session_name("ns-bind-alpha");
    let beta = session_name("ns-bind-beta");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    let beta_name = beta.to_string();
    bind_prefix_key(&handler, "Y", &["new-session", "-s", &beta_name]).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02Y")
        .await
        .expect("prefix Y dispatches new-session");

    wait_for_attached_session(&handler, requester_pid, &beta).await;
}

#[tokio::test]
async fn attached_command_prompt_template_new_session_moves_the_client() {
    let handler = RequestHandler::new();
    let requester_pid = u32::MAX - 142;
    let alpha = session_name("ns-template-alpha");
    let beta = session_name("ns-template-beta");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    bind_prefix_key(
        &handler,
        "Y",
        &["command-prompt", "-p", "n: ", "new-session -s %%"],
    )
    .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02Y")
        .await
        .expect("prefix Y opens the prompt");
    let answer = format!("{beta}\r");
    handler
        .handle_attached_live_input_for_test(requester_pid, answer.as_bytes())
        .await
        .expect("prompt accepts the session name");

    wait_for_attached_session(&handler, requester_pid, &beta).await;
}

#[tokio::test]
async fn attached_colon_prompt_new_session_moves_the_client() {
    let handler = RequestHandler::new();
    let requester_pid = u32::MAX - 143;
    let alpha = session_name("ns-colon-alpha");
    let beta = session_name("ns-colon-beta");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    let input = format!("\x02:new-session -s {beta}\r");
    handler
        .handle_attached_live_input_for_test(requester_pid, input.as_bytes())
        .await
        .expect("attached command prompt accepts new-session");

    wait_for_attached_session(&handler, requester_pid, &beta).await;
}

#[tokio::test]
async fn attached_block_new_session_moves_the_client_and_its_queue_tail() {
    let handler = RequestHandler::new();
    let requester_pid = u32::MAX - 144;
    let alpha = session_name("ns-block-alpha");
    let beta = session_name("ns-block-beta");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    // Bound through the `:` prompt so the `{ ... }` block goes through the
    // command parser, as in a configuration file.
    let bind = format!(
        "\x02:bind Y {{ new-session -s {beta} ; new-window -d ; set-buffer -b ns-block-tail done }}\r"
    );
    handler
        .handle_attached_live_input_for_test(requester_pid, bind.as_bytes())
        .await
        .expect("attached command prompt binds the block");

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02Y")
        .await
        .expect("prefix Y dispatches the block");

    wait_for_tail_buffer(&handler, "ns-block-tail").await;
    assert_eq!(attached_session_of(&handler, requester_pid).await, beta);
    // tmux 3.7b: the block tail targets the session the client entered.
    assert_eq!(window_count(&handler, &alpha).await, 1);
    assert_eq!(window_count(&handler, &beta).await, 2);
}

#[tokio::test]
async fn attached_new_session_attach_if_exists_moves_the_client() {
    let handler = RequestHandler::new();
    let requester_pid = u32::MAX - 145;
    let alpha = session_name("ns-existing-alpha");
    let beta = session_name("ns-existing-beta");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: beta.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );

    let input = format!("\x02:new-session -A -s {beta} ; set-buffer -b ns-existing-tail done\r");
    handler
        .handle_attached_live_input_for_test(requester_pid, input.as_bytes())
        .await
        .expect("attached command prompt accepts new-session -A");

    wait_for_tail_buffer(&handler, "ns-existing-tail").await;
    assert_eq!(attached_session_of(&handler, requester_pid).await, beta);
}

#[tokio::test]
async fn attached_detached_new_session_keeps_the_client() {
    let handler = RequestHandler::new();
    let requester_pid = u32::MAX - 146;
    let alpha = session_name("ns-detached-alpha");
    let beta = session_name("ns-detached-beta");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    let input = format!("\x02:new-session -d -s {beta} ; set-buffer -b ns-detached-tail done\r");
    handler
        .handle_attached_live_input_for_test(requester_pid, input.as_bytes())
        .await
        .expect("attached command prompt accepts new-session -d");

    wait_for_tail_buffer(&handler, "ns-detached-tail").await;
    assert!(handler.state.lock().await.sessions.contains_session(&beta));
    assert_eq!(attached_session_of(&handler, requester_pid).await, alpha);
}
