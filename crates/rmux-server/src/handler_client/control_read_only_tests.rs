//! `attach-session -r` on a control client, against tmux 3.7b (measured
//! 2026-11-18 on `tmux -C attach -r`, `-C attach -f read-only` and
//! `-CC attach -r`): the attach succeeds, `#{client_flags}` reports
//! `ignore-size,read-only` (`-f read-only` reports `read-only` alone),
//! `send-keys` without `-X` answers `client is read-only`, `send-keys -X`,
//! `set-option` and the other commands still run, and `refresh-client -f
//! '!read-only'` leaves the flag set.

use super::*;
use crate::handler::control_support::{with_control_queue_identity, ControlClientIdentity};

pub(super) async fn run_control_command(
    handler: &RequestHandler,
    identity: ControlClientIdentity,
    command: &str,
) -> Result<rmux_proto::CommandOutput, rmux_proto::RmuxError> {
    let parsed = CommandParser::new()
        .parse(command)
        .expect("control command parses");
    with_control_queue_identity(
        identity,
        handler.execute_parsed_commands_for_test(identity.requester_pid(), parsed),
    )
    .await
}

pub(super) async fn control_flags(handler: &RequestHandler, requester_pid: u32) -> String {
    let active_control = handler.active_control.lock().await;
    let active = active_control
        .by_pid
        .get(&requester_pid)
        .expect("control client remains registered");
    crate::handler::format_control_client_flags(active)
}

pub(super) fn assert_read_only_refusal(
    result: Result<rmux_proto::CommandOutput, rmux_proto::RmuxError>,
) {
    let error = result.expect_err("a read-only control client cannot send keys");
    assert!(
        error.to_string().contains("client is read-only"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn control_attach_read_only_is_accepted_and_refuses_only_input_like_tmux() {
    let handler = RequestHandler::new();
    let session = session_name("control-read-only");
    create_session(&handler, session.clone(), INITIAL_SIZE).await;
    let requester_pid = std::process::id();
    let (control_id, _events) =
        register_control_client_with_id(&handler, requester_pid, &session).await;
    let identity = ControlClientIdentity::new(requester_pid, control_id);

    run_control_command(&handler, identity, "attach-session -r -t control-read-only")
        .await
        .expect("tmux 3.7b accepts attach-session -r from a control client");
    let flags = control_flags(&handler, requester_pid).await;
    assert!(
        flags.starts_with("attached,focused,control-mode,ignore-size,read-only"),
        "{flags}"
    );

    assert_read_only_refusal(
        run_control_command(&handler, identity, "send-keys -t control-read-only -l RO").await,
    );
    assert_read_only_refusal(
        run_control_command(&handler, identity, "send-keys -t control-read-only Enter").await,
    );

    run_control_command(&handler, identity, "set-option -g @control_read_only 1")
        .await
        .expect("a read-only control client still runs ordinary commands");
    run_control_command(&handler, identity, "copy-mode -t control-read-only")
        .await
        .expect("copy-mode is not input");
    run_control_command(
        &handler,
        identity,
        "send-keys -X -t control-read-only cancel",
    )
    .await
    .expect("send-keys -X stays available to a read-only client");

    run_control_command(&handler, identity, "refresh-client -f !read-only")
        .await
        .expect("refresh-client accepts the read-only flag on a control client");
    assert!(
        control_flags(&handler, requester_pid)
            .await
            .contains("read-only"),
        "tmux masks !read-only off a read-only client"
    );
    assert_read_only_refusal(
        run_control_command(&handler, identity, "send-keys -t control-read-only -l RO").await,
    );
}

#[tokio::test]
async fn control_attach_flag_read_only_sets_read_only_without_ignore_size_like_tmux() {
    let handler = RequestHandler::new();
    let session = session_name("control-flag-read-only");
    create_session(&handler, session.clone(), INITIAL_SIZE).await;
    let requester_pid = std::process::id();
    let (control_id, _events) =
        register_control_client_with_id(&handler, requester_pid, &session).await;
    let identity = ControlClientIdentity::new(requester_pid, control_id);

    run_control_command(
        &handler,
        identity,
        "attach-session -f read-only -t control-flag-read-only",
    )
    .await
    .expect("tmux 3.7b accepts attach-session -f read-only from a control client");
    let flags = control_flags(&handler, requester_pid).await;
    assert!(flags.contains("read-only"), "{flags}");
    assert!(!flags.contains("ignore-size"), "{flags}");
    assert_read_only_refusal(
        run_control_command(
            &handler,
            identity,
            "send-keys -t control-flag-read-only -l RO",
        )
        .await,
    );
}

#[tokio::test]
async fn writable_control_client_keeps_send_keys() {
    let handler = RequestHandler::new();
    let session = session_name("control-writable");
    create_session(&handler, session.clone(), INITIAL_SIZE).await;
    let requester_pid = std::process::id();
    let (control_id, _events) =
        register_control_client_with_id(&handler, requester_pid, &session).await;
    let identity = ControlClientIdentity::new(requester_pid, control_id);

    run_control_command(&handler, identity, "attach-session -t control-writable")
        .await
        .expect("plain control attach");
    run_control_command(&handler, identity, "send-keys -t control-writable -l RW")
        .await
        .expect("a writable control client sends keys");
    assert!(!control_flags(&handler, requester_pid)
        .await
        .contains("read-only"));
}

/// Registers two control clients, makes the last one read-only when
/// `read_only_last` (the first one otherwise), then sends keys from a
/// requester with no client of its own, through the plain request and through
/// a parsed command off any control queue.
async fn send_keys_without_a_client_after(
    read_only_last: bool,
) -> (
    Response,
    Result<rmux_proto::CommandOutput, rmux_proto::RmuxError>,
) {
    let handler = RequestHandler::new();
    let session = session_name("control-read-only-latest");
    create_session(&handler, session.clone(), INITIAL_SIZE).await;
    let requester_pid = std::process::id();
    let first_pid = requester_pid.wrapping_add(11);
    let last_pid = requester_pid.wrapping_add(13);
    let (first_id, _first_events) =
        register_control_client_with_id(&handler, first_pid, &session).await;
    let (last_id, _last_events) =
        register_control_client_with_id(&handler, last_pid, &session).await;
    let read_only = if read_only_last {
        ControlClientIdentity::new(last_pid, last_id)
    } else {
        ControlClientIdentity::new(first_pid, first_id)
    };
    run_control_command(
        &handler,
        read_only,
        "attach-session -r -t control-read-only-latest",
    )
    .await
    .expect("read-only control attach");

    let plain = handler
        .dispatch(
            requester_pid,
            Request::SendKeys(rmux_proto::SendKeysRequest {
                target: rmux_proto::PaneTarget::with_window(session, 0, 0),
                keys: vec!["PLAIN".to_owned()],
            }),
        )
        .await
        .response;
    let parsed = CommandParser::new()
        .parse("send-keys -t control-read-only-latest -l PARSED")
        .expect("send-keys parses");
    let parsed = handler
        .execute_parsed_commands_for_test(requester_pid, parsed)
        .await;
    (plain, parsed)
}

#[tokio::test]
async fn send_keys_without_a_client_follows_the_most_recent_client_read_only_flag() {
    // tmux 3.7b (measured 2026-11-18): `send-keys` without `-c` from a client
    // that is not attached judges `cmd_find_current_client()`, the most
    // recently active client. With a writable `-C attach` first and `-C attach
    // -r` last it answers `client is read-only`; in the other order it runs.
    let (plain, parsed) = send_keys_without_a_client_after(true).await;
    assert!(
        matches!(&plain, Response::Error(rmux_proto::ErrorResponse { error })
            if error.to_string().contains("client is read-only")),
        "the read-only client attached last is the current client: {plain:?}"
    );
    assert_read_only_refusal(parsed);

    let (plain, parsed) = send_keys_without_a_client_after(false).await;
    assert!(
        matches!(plain, Response::SendKeys(_)),
        "the writable client attached last is the current client: {plain:?}"
    );
    parsed.expect("the writable client attached last is the current client");
}

/// Registers a writable and a read-only control client, the read-only one
/// last when `read_only_last`, then sends keys with `-c` naming the read-only
/// one when `name_read_only` (the writable one otherwise), from a requester
/// with no client of its own: through the request the CLI sends, and through
/// a parsed command off any control queue.
async fn send_keys_naming_a_control_client(
    read_only_last: bool,
    name_read_only: bool,
) -> (
    Response,
    Result<rmux_proto::CommandOutput, rmux_proto::RmuxError>,
) {
    let handler = RequestHandler::new();
    let session = session_name("control-read-only-named");
    create_session(&handler, session.clone(), INITIAL_SIZE).await;
    let requester_pid = std::process::id();
    let first_pid = requester_pid.wrapping_add(17);
    let last_pid = requester_pid.wrapping_add(19);
    let (first_id, _first_events) =
        register_control_client_with_id(&handler, first_pid, &session).await;
    let (last_id, _last_events) =
        register_control_client_with_id(&handler, last_pid, &session).await;
    let (read_only, writable) = if read_only_last {
        (
            ControlClientIdentity::new(last_pid, last_id),
            ControlClientIdentity::new(first_pid, first_id),
        )
    } else {
        (
            ControlClientIdentity::new(first_pid, first_id),
            ControlClientIdentity::new(last_pid, last_id),
        )
    };
    run_control_command(
        &handler,
        read_only,
        "attach-session -r -t control-read-only-named",
    )
    .await
    .expect("read-only control attach");
    let named = if name_read_only { read_only } else { writable };
    let name = crate::client_names::control_client_name(named.requester_pid());

    let requested = handler
        .dispatch(
            requester_pid,
            Request::SendKeysExt2(Box::new(rmux_proto::SendKeysExt2Request {
                target: Some(rmux_proto::PaneTarget::with_window(session, 0, 0)),
                keys: vec!["REQUESTED".to_owned()],
                expand_formats: false,
                hex: false,
                literal: true,
                dispatch_key_table: false,
                copy_mode_command: false,
                forward_mouse_event: false,
                reset_terminal: false,
                repeat_count: None,
                target_client: Some(name.clone()),
            })),
        )
        .await
        .response;
    let parsed = CommandParser::new()
        .parse(&format!(
            "send-keys -c {name} -t control-read-only-named -l PARSED"
        ))
        .expect("send-keys parses");
    let parsed = handler
        .execute_parsed_commands_for_test(requester_pid, parsed)
        .await;
    (requested, parsed)
}

#[tokio::test]
async fn send_keys_naming_a_control_client_follows_its_read_only_flag() {
    // tmux 3.7b (measured 2026-11-19): `send-keys -c <control client>`
    // resolves the control client through `cmd_find_client()` and judges its
    // flag, whichever client attached last: the read-only one answers
    // `client is read-only`, the writable one types the keys.
    for read_only_last in [true, false] {
        let (requested, parsed) = send_keys_naming_a_control_client(read_only_last, true).await;
        assert!(
            matches!(&requested, Response::Error(rmux_proto::ErrorResponse { error })
                if error.to_string().contains("client is read-only")),
            "-c names the read-only control client (last: {read_only_last}): {requested:?}"
        );
        assert_read_only_refusal(parsed);

        let (requested, parsed) = send_keys_naming_a_control_client(read_only_last, false).await;
        assert!(
            matches!(
                requested,
                Response::SendKeys(rmux_proto::SendKeysResponse { key_count: 1 })
            ),
            "-c names the writable control client (read-only last: {read_only_last}): \
             {requested:?}"
        );
        parsed.unwrap_or_else(|error| {
            panic!(
                "-c names the writable control client (read-only last: {read_only_last}): {error}"
            )
        });
    }
}

#[tokio::test]
async fn read_only_control_client_send_keys_to_a_named_writable_client_is_not_refused() {
    // tmux checks the target client: with `-c`, that is the named client, not
    // the read-only control client issuing the command.
    let handler = RequestHandler::new();
    let session = session_name("control-read-only-target");
    create_session(&handler, session.clone(), INITIAL_SIZE).await;
    let requester_pid = std::process::id();
    let attached_pid = requester_pid.wrapping_add(7);
    let (_attach_id, _attach_rx) =
        register_attached_client(&handler, attached_pid, &session, INITIAL_SIZE).await;
    let (control_id, _events) =
        register_control_client_with_id(&handler, requester_pid, &session).await;
    let identity = ControlClientIdentity::new(requester_pid, control_id);

    run_control_command(
        &handler,
        identity,
        "attach-session -r -t control-read-only-target",
    )
    .await
    .expect("read-only control attach");
    let result = run_control_command(
        &handler,
        identity,
        &format!("send-keys -c {attached_pid} -t control-read-only-target -l RO"),
    )
    .await;
    if let Err(error) = result {
        assert!(
            !error.to_string().contains("client is read-only"),
            "a writable target client is not read-only: {error}"
        );
    }
}

#[tokio::test]
async fn control_attach_flag_is_refused_or_applied_never_dropped() {
    // tmux 3.7b (measured 2026-11-18 on `tmux -C`, `attach -f <flags>`) accepts
    // `active-pane`, `no-detach-on-destroy` and `active-pane,read-only` and
    // reports each of them in `#{client_flags}`. A control client here keeps no
    // `active-pane` or `no-detach-on-destroy` state yet, so the attach must be
    // refused whole (a declared divergence) rather than report success while
    // dropping a flag; once a flag is supported, it must appear.
    const UNSUPPORTED: &str = "attach-session client flags are not available for control clients";
    let handler = RequestHandler::new();
    let session = session_name("control-attach-flags");
    create_session(&handler, session.clone(), INITIAL_SIZE).await;
    let requester_pid = std::process::id();
    let (control_id, _events) =
        register_control_client_with_id(&handler, requester_pid, &session).await;
    let identity = ControlClientIdentity::new(requester_pid, control_id);

    for requested in [
        "active-pane",
        "no-detach-on-destroy",
        "active-pane,read-only",
    ] {
        let before = control_flags(&handler, requester_pid).await;
        let result = run_control_command(
            &handler,
            identity,
            &format!("attach-session -f {requested} -t control-attach-flags"),
        )
        .await;
        let after = control_flags(&handler, requester_pid).await;
        match result {
            Err(error) => {
                assert!(
                    error.to_string().contains(UNSUPPORTED),
                    "-f {requested}: unexpected error: {error}"
                );
                assert_eq!(
                    after, before,
                    "-f {requested}: a refused attach applies nothing"
                );
            }
            Ok(_) => {
                for flag in requested.split(',') {
                    assert!(
                        after.split(',').any(|reported| reported == flag),
                        "-f {requested} was accepted but #{{client_flags}} lacks {flag}: {after}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn plain_send_keys_off_the_read_only_control_queue_is_not_refused() {
    // The read-only check belongs to the control queue, not to the process id:
    // a plain request from the same process (for instance an SDK that keeps a
    // read-only control observer) is another client. tmux 3.7b (measured
    // 2026-11-18): with `-C attach -r` attached first and a writable `-C
    // attach` last, a plain `send-keys` from another client runs.
    let handler = RequestHandler::new();
    let session = session_name("control-read-only-off-queue");
    create_session(&handler, session.clone(), INITIAL_SIZE).await;
    let requester_pid = std::process::id();
    let (control_id, _events) =
        register_control_client_with_id(&handler, requester_pid, &session).await;
    let identity = ControlClientIdentity::new(requester_pid, control_id);
    run_control_command(
        &handler,
        identity,
        "attach-session -r -t control-read-only-off-queue",
    )
    .await
    .expect("read-only control attach");
    let (_writable_id, _writable_events) =
        register_control_client_with_id(&handler, requester_pid.wrapping_add(11), &session).await;
    assert_read_only_refusal(
        run_control_command(
            &handler,
            identity,
            "send-keys -t control-read-only-off-queue -l RO",
        )
        .await,
    );

    let response = handler
        .dispatch(
            requester_pid,
            Request::SendKeys(rmux_proto::SendKeysRequest {
                target: rmux_proto::PaneTarget::with_window(session.clone(), 0, 0),
                keys: vec!["OFFQUEUE".to_owned()],
            }),
        )
        .await
        .response;
    assert!(
        matches!(response, Response::SendKeys(_)),
        "a plain request off the read-only control queue is not that client: {response:?}"
    );
}

async fn control_client_readonly(
    handler: &RequestHandler,
    identity: ControlClientIdentity,
) -> (String, String) {
    let render = |output: rmux_proto::CommandOutput| {
        String::from_utf8(output.stdout().to_vec()).expect("format output is UTF-8")
    };
    let displayed = run_control_command(handler, identity, "display-message -p #{client_readonly}")
        .await
        .expect("display-message runs on a control client");
    let listed = run_control_command(handler, identity, "list-clients -F #{client_readonly}")
        .await
        .expect("list-clients runs on a control client");
    (render(displayed), render(listed))
}

#[tokio::test]
async fn control_client_readonly_format_reports_zero_or_one_like_tmux() {
    // tmux 3.7b `format_cb_client_readonly()` answers "1" for a
    // `CLIENT_READONLY` client and "0" for any other client, measured on
    // `-C attach`, `-C attach -r` and `-C attach -f read-only`.
    for (attach, expected) in [
        ("attach-session -t control-readonly-format", "0\n"),
        ("attach-session -r -t control-readonly-format", "1\n"),
        (
            "attach-session -f read-only -t control-readonly-format",
            "1\n",
        ),
    ] {
        let handler = RequestHandler::new();
        let session = session_name("control-readonly-format");
        create_session(&handler, session.clone(), INITIAL_SIZE).await;
        let requester_pid = std::process::id();
        let (control_id, _events) =
            register_control_client_with_id(&handler, requester_pid, &session).await;
        let identity = ControlClientIdentity::new(requester_pid, control_id);

        run_control_command(&handler, identity, attach)
            .await
            .expect("tmux 3.7b accepts this control attach");
        let (displayed, listed) = control_client_readonly(&handler, identity).await;
        assert_eq!(displayed, expected, "display-message after {attach}");
        assert_eq!(listed, expected, "list-clients -F after {attach}");
    }
}
