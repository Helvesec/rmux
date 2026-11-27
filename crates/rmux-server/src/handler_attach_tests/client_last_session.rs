//! `#{client_last_session}` names the session an attached client left last, as
//! tmux 3.7b's `format_cb_client_last_session` reads `c->last_session` while
//! `session_alive()` holds.
//!
//! Oracle: tmux 3.7b, measured on this host with a client attached to `alpha`
//! and `set-titles-string 'T=#{client_session}/#{client_last_session}='`.
//! Before any move it lists `alpha|`; `switch-client -t beta` lists
//! `beta|alpha`; `switch-client -l` then lists `alpha|beta`; an attached
//! `new-session` lists `gamma|alpha`. Killing the remembered session lists
//! `beta|`, renaming it lists the new name, and tmux redraws the client in both
//! cases. A client re-homed because its own session died has no last session,
//! nor does any other client re-homed with it. A control client lists the
//! session it left like an attached one: after `switch-client -t beta` it
//! lists `beta|alpha`.

use super::set_titles_support::{
    delivered_titles, new_detached_session, set_global, title_capable_context,
};
use super::*;

const LAST_SESSION_FORMAT: &str = "#{client_session}|#{client_last_session}";

async fn attached_client(
    handler: &RequestHandler,
    attach_pid: u32,
    alpha: &SessionName,
    beta: &SessionName,
) -> mpsc::UnboundedReceiver<AttachControl> {
    new_detached_session(handler, alpha).await;
    new_detached_session(handler, beta).await;
    // No periodic tick may repair a frame or stand in for a missing redraw.
    set_global(handler, OptionName::StatusInterval, "0").await;
    set_global(handler, OptionName::SetTitles, "on").await;
    set_global(handler, OptionName::SetTitlesString, LAST_SESSION_FORMAT).await;
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach_with_terminal_context(
            attach_pid,
            alpha.clone(),
            control_tx,
            title_capable_context(),
        )
        .await;
    control_rx
}

async fn listed(handler: &RequestHandler) -> String {
    let response = handler
        .handle(Request::ListClients(Box::new(
            rmux_proto::ListClientsRequest {
                target_session: None,
                format: Some(LAST_SESSION_FORMAT.to_owned()),
                filter: None,
                sort_order: None,
                reversed: false,
            },
        )))
        .await;
    let Response::ListClients(list) = response else {
        panic!("list-clients must answer, got {response:?}");
    };
    String::from_utf8_lossy(list.output.stdout())
        .trim_end()
        .to_owned()
}

async fn switch_client(handler: &RequestHandler, attach_pid: u32, target: Option<&SessionName>) {
    let response = handler
        .dispatch(
            attach_pid,
            Request::SwitchClientExt2(Box::new(SwitchClientExt2Request {
                target: target.cloned(),
                key_table: None,
                last_session: target.is_none(),
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                flags: None,
                sort_order: None,
                skip_environment_update: false,
            })),
        )
        .await
        .response;
    assert!(
        matches!(response, Response::SwitchClient(_)),
        "{response:?}"
    );
}

async fn kill_session(handler: &RequestHandler, target: &SessionName) {
    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: target.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(response, Response::KillSession(_)), "{response:?}");
}

async fn expect_redraw(control_rx: &mut mpsc::UnboundedReceiver<AttachControl>, context: &str) {
    let _ = recv_matching_attach_control(control_rx, context, |control| {
        matches!(control, AttachControl::Refresh | AttachControl::Switch(_))
    })
    .await;
}

#[tokio::test]
async fn switch_client_records_the_left_session_in_list_clients_and_the_frame() {
    let handler = RequestHandler::new();
    let attach_pid = std::process::id();
    let alpha = session_name("last-switch-alpha");
    let beta = session_name("last-switch-beta");
    let mut control_rx = attached_client(&handler, attach_pid, &alpha, &beta).await;
    assert_eq!(listed(&handler).await, format!("{alpha}|"));
    let _opening = delivered_titles(&mut control_rx);

    switch_client(&handler, attach_pid, Some(&beta)).await;
    assert_eq!(listed(&handler).await, format!("{beta}|{alpha}"));
    assert_eq!(
        delivered_titles(&mut control_rx).last(),
        Some(&format!("{beta}|{alpha}")),
        "the switch frame expands the same last session list-clients reports"
    );

    switch_client(&handler, attach_pid, None).await;
    assert_eq!(listed(&handler).await, format!("{alpha}|{beta}"));
    assert_eq!(
        delivered_titles(&mut control_rx).last(),
        Some(&format!("{alpha}|{beta}"))
    );
}

#[tokio::test]
async fn switching_to_the_current_session_keeps_no_last_session() {
    let handler = RequestHandler::new();
    let attach_pid = std::process::id();
    let alpha = session_name("last-self-alpha");
    let beta = session_name("last-self-beta");
    let _control_rx = attached_client(&handler, attach_pid, &alpha, &beta).await;

    switch_client(&handler, attach_pid, Some(&alpha)).await;
    assert_eq!(listed(&handler).await, format!("{alpha}|"));
}

#[tokio::test]
async fn attached_new_session_records_the_session_it_left() {
    let handler = RequestHandler::new();
    let attach_pid = std::process::id();
    let alpha = session_name("last-new-alpha");
    let beta = session_name("last-new-beta");
    let gamma = session_name("last-new-gamma");
    let _control_rx = attached_client(&handler, attach_pid, &alpha, &beta).await;

    new_session_from_binding(&handler, attach_pid, &gamma).await;
    assert_eq!(listed(&handler).await, format!("{gamma}|{alpha}"));
}

async fn new_session_from_binding(handler: &RequestHandler, attach_pid: u32, gamma: &SessionName) {
    let response = handler
        .handle(Request::BindKey(Box::new(rmux_proto::BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "Y".to_owned(),
            note: None,
            repeat: false,
            command: Some(vec![
                "new-session".to_owned(),
                "-s".to_owned(),
                gamma.to_string(),
            ]),
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)), "{response:?}");
    handler
        .handle_attached_live_input_for_test(attach_pid, b"\x02Y")
        .await
        .expect("prefix Y dispatches new-session");
    let deadline = tokio::time::Instant::now() + ATTACH_LIFECYCLE_TIMEOUT;
    while !listed(handler).await.starts_with(&format!("{gamma}|")) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "client must enter {gamma}; lists {}",
            listed(handler).await
        );
        sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn killing_the_remembered_session_clears_it_and_redraws_the_client() {
    let handler = RequestHandler::new();
    let attach_pid = std::process::id();
    let alpha = session_name("last-kill-alpha");
    let beta = session_name("last-kill-beta");
    let mut control_rx = attached_client(&handler, attach_pid, &alpha, &beta).await;
    switch_client(&handler, attach_pid, Some(&beta)).await;
    while control_rx.try_recv().is_ok() {}

    kill_session(&handler, &alpha).await;
    assert_eq!(listed(&handler).await, format!("{beta}|"));
    expect_redraw(&mut control_rx, "redraw after the last session died").await;
}

#[tokio::test]
async fn renaming_the_remembered_session_follows_it_and_redraws_the_client() {
    let handler = RequestHandler::new();
    let attach_pid = std::process::id();
    let alpha = session_name("last-rename-alpha");
    let beta = session_name("last-rename-beta");
    let zeta = session_name("last-rename-zeta");
    let mut control_rx = attached_client(&handler, attach_pid, &alpha, &beta).await;
    switch_client(&handler, attach_pid, Some(&beta)).await;
    while control_rx.try_recv().is_ok() {}

    let response = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: alpha.clone(),
            new_name: zeta.clone(),
        }))
        .await;
    assert!(
        matches!(response, Response::RenameSession(_)),
        "{response:?}"
    );
    assert_eq!(listed(&handler).await, format!("{beta}|{zeta}"));
    expect_redraw(&mut control_rx, "redraw after the last session was renamed").await;
}

#[tokio::test]
async fn a_client_rehomed_from_its_dying_session_has_no_last_session() {
    let handler = RequestHandler::new();
    let attach_pid = std::process::id();
    let alpha = session_name("last-rehome-alpha");
    let beta = session_name("last-rehome-beta");
    let mut control_rx = attached_client(&handler, attach_pid, &alpha, &beta).await;
    set_global(&handler, OptionName::DetachOnDestroy, "off").await;
    switch_client(&handler, attach_pid, Some(&beta)).await;
    let _switched = delivered_titles(&mut control_rx);

    kill_session(&handler, &beta).await;
    assert_eq!(listed(&handler).await, format!("{alpha}|"));
    // tmux clears the last session before re-homing, so no frame, not even a
    // transient one later repaired by a redraw, may name the dead session.
    let titles = delivered_titles(&mut control_rx);
    let rehomed_title = format!("{alpha}|");
    assert!(
        !titles.is_empty() && titles.iter().all(|title| title == &rehomed_title),
        "the re-homing frames must not name the session that just died: {titles:?}"
    );
}

#[tokio::test]
async fn clients_rehomed_together_never_name_their_dying_session() {
    let handler = RequestHandler::new();
    let first_pid = std::process::id();
    let second_pid = first_pid + 1;
    let alpha = session_name("last-rehome-pair-alpha");
    let beta = session_name("last-rehome-pair-beta");
    let mut first_rx = attached_client(&handler, first_pid, &alpha, &beta).await;
    let (second_tx, mut second_rx) = mpsc::unbounded_channel();
    let _second_id = handler
        .register_attach_with_terminal_context(
            second_pid,
            alpha.clone(),
            second_tx,
            title_capable_context(),
        )
        .await;
    set_global(&handler, OptionName::DetachOnDestroy, "off").await;
    switch_client(&handler, first_pid, Some(&beta)).await;
    switch_client(&handler, second_pid, Some(&beta)).await;
    let _first_switched = delivered_titles(&mut first_rx);
    let _second_switched = delivered_titles(&mut second_rx);

    kill_session(&handler, &beta).await;
    let rehomed_title = format!("{alpha}|");
    assert_eq!(
        listed(&handler).await,
        format!("{rehomed_title}\n{rehomed_title}")
    );
    // The second client's re-homing redraws the first one before the dead
    // session is closed: that redraw must not name it either, as tmux clears
    // every client's last session before it re-homes any of them.
    for (client, control_rx) in [("first", &mut first_rx), ("second", &mut second_rx)] {
        let titles = delivered_titles(control_rx);
        assert!(
            !titles.is_empty() && titles.iter().all(|title| title == &rehomed_title),
            "the {client} client was shown the session that just died: {titles:?}"
        );
    }
}

/// The returned receiver keeps the control client deliverable: once it drops,
/// the next session update marks the client closing.
async fn control_client(
    handler: &RequestHandler,
    control_pid: u32,
    session: &SessionName,
) -> mpsc::Receiver<crate::control::ControlServerEvent> {
    let (event_tx, event_rx) = mpsc::channel(crate::control::CONTROL_SERVER_EVENT_CAPACITY);
    let _control_id = handler
        .register_control_with_closing(
            control_pid,
            crate::control::ControlModeUpgrade {
                initial_command_count: 0,
                mode: rmux_proto::ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    handler
        .set_control_session(control_pid, Some(session.clone()))
        .await
        .expect("control client attaches to its session");
    event_rx
}

#[tokio::test]
async fn a_control_client_lists_the_session_it_left() {
    let handler = RequestHandler::new();
    let control_pid = std::process::id();
    let alpha = session_name("last-control-alpha");
    let beta = session_name("last-control-beta");
    new_detached_session(&handler, &alpha).await;
    new_detached_session(&handler, &beta).await;
    let _control_events = control_client(&handler, control_pid, &alpha).await;
    assert_eq!(listed(&handler).await, format!("{alpha}|"));

    switch_client(&handler, control_pid, Some(&beta)).await;
    assert_eq!(listed(&handler).await, format!("{beta}|{alpha}"));

    switch_client(&handler, control_pid, None).await;
    assert_eq!(listed(&handler).await, format!("{alpha}|{beta}"));
}
