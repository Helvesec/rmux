//! Issue #182, client-scoped expansion: `set-titles-string` is expanded once
//! per attached client, so the client-scoped format variables must resolve
//! against *that* client.
//!
//! tmux initialises the title's format tree with the active client through
//! `format_defaults()` before expanding `set-titles-string`, so
//! `#{client_width}`, `#{client_height}` and `#{client_name}` carry the real
//! client. Expanding against a session-only context leaves them empty, and two
//! clients of different sizes are handed the same string.

use super::set_titles_support::{
    delivered_titles, new_detached_session, remembered_title, set_global, title_capable_context,
};
use super::*;

/// Registers one client with its own identity and geometry, exactly as
/// `listener.rs` publishes a fresh attach.
async fn attach_sized_client(
    handler: &RequestHandler,
    session: &rmux_proto::SessionName,
    attach_pid: u32,
    client_size: TerminalSize,
) -> mpsc::UnboundedReceiver<AttachControl> {
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let uid = current_owner_uid();
    handler
        .register_attach_with_access(
            attach_pid,
            session.clone(),
            None,
            AttachRegistration {
                control_tx,
                control_backlog: Arc::new(AtomicUsize::new(0)),
                closing: Arc::new(AtomicBool::new(false)),
                persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
                terminal_context: title_capable_context(),
                client_title: None,
                flags: crate::handler::attach_support::ClientFlags::default(),
                render_stream: false,
                uid,
                user: rmux_os::identity::UserIdentity::Uid(uid),
                can_write: true,
                client_size: Some(client_size),
            },
        )
        .await
        .expect("attach registration succeeds");
    control_rx
}

/// The same variables, resolved through the independent `list-clients` format
/// path, keyed by client pid. This is the oracle the title must agree with.
async fn list_clients_bindings(handler: &RequestHandler) -> Vec<(String, String)> {
    let response = handler
        .handle(Request::ListClients(Box::new(
            rmux_proto::ListClientsRequest {
                target_session: None,
                format: Some(
                    "#{client_pid}\tW=#{client_width}-H=#{client_height}-N=#{client_name}"
                        .to_owned(),
                ),
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
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(pid, rest)| (pid.to_owned(), rest.to_owned()))
        .collect()
}

/// The reviewer's native reproduction, at the layer it failed: a 120x30 client
/// and an 80x24 client must each be told their own geometry and name.
#[tokio::test]
async fn two_clients_expand_their_own_size_and_name_into_the_title() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_detached_session(&handler, &alpha).await;

    let wide_pid = 41_821;
    let narrow_pid = 41_822;
    let mut wide_rx = attach_sized_client(
        &handler,
        &alpha,
        wide_pid,
        TerminalSize {
            cols: 120,
            rows: 30,
        },
    )
    .await;
    let mut narrow_rx = attach_sized_client(
        &handler,
        &alpha,
        narrow_pid,
        TerminalSize { cols: 80, rows: 24 },
    )
    .await;

    let expected = list_clients_bindings(&handler).await;
    let expected_wide = expected
        .iter()
        .find(|(pid, _)| pid == &wide_pid.to_string())
        .map(|(_, value)| value.clone())
        .expect("the wide client is listed");
    let expected_narrow = expected
        .iter()
        .find(|(pid, _)| pid == &narrow_pid.to_string())
        .map(|(_, value)| value.clone())
        .expect("the narrow client is listed");
    assert!(
        expected_wide.contains("W=120-H=30-N=") && !expected_wide.ends_with("N="),
        "the list-clients oracle must resolve the wide client, got {expected_wide:?}"
    );
    assert_ne!(
        expected_wide, expected_narrow,
        "the two clients must differ in the oracle"
    );

    set_global(
        &handler,
        OptionName::SetTitlesString,
        "W=#{client_width}-H=#{client_height}-N=#{client_name}",
    )
    .await;
    set_global(&handler, OptionName::SetTitles, "on").await;

    assert_eq!(
        delivered_titles(&mut wide_rx),
        vec![expected_wide.clone()],
        "the wide client's title must carry its own client bindings"
    );
    assert_eq!(
        delivered_titles(&mut narrow_rx),
        vec![expected_narrow.clone()],
        "the narrow client's title must carry its own client bindings"
    );
    assert_eq!(
        remembered_title(&handler, wide_pid).await,
        Some(expected_wide)
    );
    assert_eq!(
        remembered_title(&handler, narrow_pid).await,
        Some(expected_narrow)
    );

    // Per-client dedup: an unrelated redraw repeats neither client's title.
    set_global(&handler, OptionName::StatusInterval, "19").await;
    assert!(
        delivered_titles(&mut wide_rx).is_empty(),
        "the wide client's unchanged title must not be rewritten"
    );
    assert!(
        delivered_titles(&mut narrow_rx).is_empty(),
        "the narrow client's unchanged title must not be rewritten"
    );
}

/// A client's own key table drives `#{client_key_table}` / `#{client_prefix}`
/// in the title, the same pair the status line already resolves per client.
#[tokio::test]
async fn the_title_resolves_the_client_key_table_and_terminal() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_detached_session(&handler, &alpha).await;

    let attach_pid = 41_823;
    let mut control_rx = attach_sized_client(
        &handler,
        &alpha,
        attach_pid,
        TerminalSize { cols: 90, rows: 25 },
    )
    .await;

    set_global(
        &handler,
        OptionName::SetTitlesString,
        "K=#{client_key_table}/P=#{client_prefix}/T=#{client_termname}/C=#{client_control_mode}",
    )
    .await;
    set_global(&handler, OptionName::SetTitles, "on").await;

    assert_eq!(
        delivered_titles(&mut control_rx),
        vec!["K=root/P=0/T=xterm-256color/C=0".to_owned()],
        "an attached client's own terminal and key table must reach the title"
    );
}

#[tokio::test]
async fn same_session_switch_keeps_the_live_transient_key_table_in_its_frame() {
    let handler = RequestHandler::new();
    let alpha = session_name("same-session-key-table");
    new_detached_session(&handler, &alpha).await;

    let attach_pid = 41_824;
    let mut control_rx = attach_sized_client(
        &handler,
        &alpha,
        attach_pid,
        TerminalSize { cols: 90, rows: 25 },
    )
    .await;
    handler
        .set_attached_key_table_for_test(attach_pid, Some("prefix".to_owned()))
        .await
        .expect("transient key table installs");
    set_global(
        &handler,
        OptionName::SetTitlesString,
        "K=#{client_key_table}",
    )
    .await;
    set_global(&handler, OptionName::SetTitles, "on").await;
    assert_eq!(
        delivered_titles(&mut control_rx),
        vec!["K=prefix".to_owned()],
        "the opening frame must use the live transient key table"
    );

    let response = handler
        .handle(Request::SwitchClient(SwitchClientRequest {
            target: alpha.clone(),
        }))
        .await;
    assert!(
        matches!(response, Response::SwitchClient(_)),
        "{response:?}"
    );
    let _ = delivered_titles(&mut control_rx);

    assert_eq!(
        remembered_title(&handler, attach_pid).await,
        Some("K=prefix".to_owned()),
        "same-session switch rendering must preserve the live key table"
    );
    assert_eq!(
        handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get(&attach_pid)
            .expect("attach remains registered")
            .key_table_name
            .as_deref(),
        Some("prefix")
    );
}
