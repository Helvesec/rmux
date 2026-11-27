//! A client that declares no size still owns outer terminal geometry.
//!
//! Every consumer of `ActiveAttach::client_size` — the window-size policies,
//! refresh rendering, `switch-client`, destroy-time rehoming — reads it as
//! outer terminal geometry and subtracts the status rows itself. Anchoring a
//! sizeless client to the session's *content* geometry made those consumers
//! subtract the status rows a second time.
//!
//! tmux 3.7b measured 2026-07-31 on macOS 26.5.2 (arm64) with an 80x24 PTY
//! client attached to a session created `-x 80 -y 24`, toggling
//! `status` `2 -> off -> 2 -> off -> 2`:
//!
//! ```text
//! attached 80x24 status=2      window=80x22    client_height=24
//! after status=off             window=80x24    client_height=24
//! after status=2               window=80x22    client_height=24
//! after status=off             window=80x24    client_height=24
//! after status=2               window=80x22    client_height=24
//! tty resized to 80x22         window=80x20    client_height=22
//! ```
//!
//! The client height stays the outer tty height, and the status rows come off
//! it exactly once per reconciliation.

use super::*;

const TERMINAL_SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const TWO_LINE_CONTENT_SIZE: TerminalSize = TerminalSize { cols: 80, rows: 22 };
const DOUBLE_SUBTRACTED_SIZE: TerminalSize = TerminalSize { cols: 80, rows: 20 };

#[tokio::test]
async fn sizeless_attach_anchors_to_outer_terminal_geometry() {
    let handler = RequestHandler::new();
    let session = session_name("sizeless-anchor");
    let sizeless_pid = 92_101;
    let _declared_rx = seed_two_line_status_geometry(&handler, &session, 92_100).await;

    let _sizeless_rx = register_sizeless_attach(&handler, sizeless_pid, &session).await;

    assert_eq!(
        attached_client_size(&handler, sizeless_pid).await,
        TERMINAL_SIZE,
        "a sizeless client must anchor to the session's outer terminal rows, \
         not to its already status-subtracted content rows"
    );
}

#[tokio::test]
async fn list_clients_reports_the_outer_terminal_height_of_a_sizeless_client() {
    let handler = RequestHandler::new();
    let session = session_name("sizeless-list-clients");
    let sizeless_pid = 92_201;
    let _declared_rx = seed_two_line_status_geometry(&handler, &session, 92_200).await;
    let _sizeless_rx = register_sizeless_attach(&handler, sizeless_pid, &session).await;

    let response = handler
        .handle(Request::ListClients(Box::new(
            rmux_proto::ListClientsRequest {
                format: Some("#{client_pid}|#{client_width}x#{client_height}".to_owned()),
                target_session: Some(session.clone()),
                filter: None,
                sort_order: None,
                reversed: false,
            },
        )))
        .await;
    let Response::ListClients(response) = response else {
        panic!("expected list-clients response");
    };
    let listed = String::from_utf8(response.output.stdout().to_vec()).expect("utf-8");
    // tmux 3.7b reports `client_height` as the outer tty height (24 above),
    // never the status-subtracted content height.
    assert!(
        listed.contains(&format!("{sizeless_pid}|80x24\n")),
        "list-clients must report outer terminal geometry, got {listed:?}"
    );
}

#[tokio::test]
async fn sizeless_attach_status_changes_subtract_status_rows_exactly_once() {
    let handler = RequestHandler::new();
    let session = session_name("sizeless-status-cycle");
    let _declared_rx = seed_two_line_status_geometry(&handler, &session, 92_110).await;

    let _sizeless_rx = register_sizeless_attach(&handler, 92_111, &session).await;

    // Matches the tmux 3.7b measurement in this module's header: the content
    // height tracks the outer terminal height minus the current status rows,
    // and repeated toggles never accumulate.
    for (value, expected) in [
        ("off", TERMINAL_SIZE),
        ("2", TWO_LINE_CONTENT_SIZE),
        ("off", TERMINAL_SIZE),
        ("2", TWO_LINE_CONTENT_SIZE),
    ] {
        set_session_status(&handler, &session, value).await;
        assert_eq!(
            session_content_size(&handler, &session).await,
            expected,
            "status={value} must subtract the status rows from the outer \
             terminal anchor exactly once"
        );
        assert_eq!(
            session_terminal_size(&handler, &session).await,
            TERMINAL_SIZE,
            "status={value} must not move the outer terminal anchor"
        );
    }
    assert_ne!(
        session_content_size(&handler, &session).await,
        DOUBLE_SUBTRACTED_SIZE
    );
}

#[tokio::test]
async fn sizeless_attach_refresh_renders_status_at_the_outer_terminal_rows() {
    let handler = RequestHandler::new();
    let session = session_name("sizeless-refresh");
    let sizeless_pid = 92_121;
    let _declared_rx = seed_two_line_status_geometry(&handler, &session, 92_120).await;

    let mut sizeless_rx = register_sizeless_attach(&handler, sizeless_pid, &session).await;
    while sizeless_rx.try_recv().is_ok() {}
    handler.refresh_attached_session(&session).await;

    let frame = recv_render_frame(&mut sizeless_rx, "sizeless refresh").await;
    assert_eq!(
        cursor_row_before(&frame, "[sizeless-"),
        23,
        "a two-line status on an 80x24 anchor starts at row 23; rendering the \
         content rows as the terminal height moves it up to row 21, got {frame:?}"
    );
    assert!(
        frame.contains("\x1b[22;1H\x1b[0m\x1b[K"),
        "the content region must still reach row 22, got {frame:?}"
    );
}

#[tokio::test]
async fn sizeless_attach_switch_carries_the_outer_terminal_anchor() {
    let handler = RequestHandler::new();
    let alpha = session_name("sizeless-switch-alpha");
    let beta = session_name("sizeless-switch-beta");
    let sizeless_pid = 92_131;
    create_session_with_status_two(&handler, &beta).await;
    let _declared_rx = seed_two_line_status_geometry(&handler, &alpha, 92_130).await;

    let _sizeless_rx = register_sizeless_attach(&handler, sizeless_pid, &alpha).await;
    let response = handler
        .dispatch(
            sizeless_pid,
            Request::SwitchClient(SwitchClientRequest {
                target: beta.clone(),
            }),
        )
        .await
        .response;
    assert!(
        matches!(response, Response::SwitchClient(_)),
        "{response:?}"
    );

    assert_eq!(
        session_terminal_size(&handler, &beta).await,
        TERMINAL_SIZE,
        "the switch destination must inherit the client's outer terminal anchor"
    );
    assert_eq!(
        session_content_size(&handler, &beta).await,
        TWO_LINE_CONTENT_SIZE
    );
}

#[tokio::test]
async fn sizeless_attach_destroy_rehoming_carries_the_outer_terminal_anchor() {
    let handler = RequestHandler::new();
    let alpha = session_name("sizeless-destroy-alpha");
    let beta = session_name("sizeless-destroy-beta");
    create_session_with_status_two(&handler, &beta).await;
    let _declared_rx = seed_two_line_status_geometry(&handler, &alpha, 92_140).await;
    set_session_option(&handler, &alpha, OptionName::DetachOnDestroy, "off").await;

    let mut sizeless_rx = register_sizeless_attach(&handler, 92_141, &alpha).await;
    while sizeless_rx.try_recv().is_ok() {}

    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: alpha.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(response, Response::KillSession(_)), "{response:?}");

    assert_eq!(
        session_terminal_size(&handler, &beta).await,
        TERMINAL_SIZE,
        "destroy-time rehoming must carry the outer terminal anchor"
    );
    assert_eq!(
        session_content_size(&handler, &beta).await,
        TWO_LINE_CONTENT_SIZE
    );
}

#[tokio::test]
async fn same_numeric_live_resize_promotes_a_sizeless_client_to_declared_geometry() {
    let handler = RequestHandler::new();
    let session = session_name("sizeless-promotion");
    let declared_pid = 92_150;
    let sizeless_pid = 92_151;
    let _declared_rx = seed_two_line_status_geometry(&handler, &session, declared_pid).await;
    let _sizeless_rx = register_sizeless_attach(&handler, sizeless_pid, &session).await;

    // The declared client moves last, so `window-size latest` owns the session.
    let declared_size = TerminalSize {
        cols: 100,
        rows: 40,
    };
    handler
        .handle_attached_resize(declared_pid, declared_size)
        .await
        .expect("declared client resize succeeds");
    assert_eq!(
        session_terminal_size(&handler, &session).await,
        declared_size
    );

    // The sizeless client now reports the real geometry of its terminal, which
    // happens to equal the anchor it was registered with. That is still a new
    // declaration and must take over `latest`.
    handler
        .handle_attached_resize(sizeless_pid, TERMINAL_SIZE)
        .await
        .expect("promoting resize succeeds");

    assert_eq!(
        session_terminal_size(&handler, &session).await,
        TERMINAL_SIZE,
        "a real resize must promote the inferred anchor into the latest declaration"
    );
    assert_eq!(
        session_content_size(&handler, &session).await,
        TWO_LINE_CONTENT_SIZE
    );
    assert!(
        !attached_client_size_is_inferred(&handler, sizeless_pid).await,
        "a real resize leaves no inferred provenance behind"
    );
}

#[tokio::test]
async fn sizeless_attach_preserves_both_dimensions_under_every_policy() {
    for (policy, expected_terminal, expected_content) in [
        ("latest", TERMINAL_SIZE, TerminalSize { cols: 80, rows: 22 }),
        (
            "largest",
            TerminalSize {
                cols: 100,
                rows: 30,
            },
            TerminalSize {
                cols: 100,
                rows: 28,
            },
        ),
        (
            "smallest",
            TERMINAL_SIZE,
            TerminalSize { cols: 80, rows: 22 },
        ),
    ] {
        let handler = RequestHandler::new();
        let session = session_name("sizeless-policy");
        let declared_pid = 92_160;
        let _declared_rx = seed_two_line_status_geometry(&handler, &session, declared_pid).await;
        handler
            .handle_attached_resize(
                declared_pid,
                TerminalSize {
                    cols: 100,
                    rows: 30,
                },
            )
            .await
            .expect("declared client resize succeeds");
        set_window_size_policy(&handler, &session, policy).await;

        let _sizeless_rx = register_sizeless_attach(&handler, 92_161, &session).await;
        // The sizeless client anchors to the 100x30 the declared client owns,
        // so force a distinguishable anchor by resizing it to the fixture size.
        handler
            .handle_attached_resize(92_161, TERMINAL_SIZE)
            .await
            .expect("sizeless client reports its real terminal");

        assert_eq!(
            session_terminal_size(&handler, &session).await,
            expected_terminal,
            "window-size {policy} must preserve the selected terminal geometry"
        );
        assert_eq!(
            session_content_size(&handler, &session).await,
            expected_content,
            "window-size {policy} must preserve the selected content geometry"
        );
    }
}

/// The same policy contract for a client that is *still* inferred: it keeps the
/// anchor it registered against while a declared peer moves away from it, so
/// `largest` and `smallest` weigh a real outer terminal against a real outer
/// terminal instead of against status-subtracted content rows.
#[tokio::test]
async fn a_still_inferred_client_competes_on_outer_terminal_rows_under_every_policy() {
    const DECLARED_TERMINAL_SIZE: TerminalSize = TerminalSize {
        cols: 100,
        rows: 30,
    };
    const DECLARED_CONTENT_SIZE: TerminalSize = TerminalSize {
        cols: 100,
        rows: 28,
    };

    for (policy, expected_terminal, expected_content) in [
        ("latest", DECLARED_TERMINAL_SIZE, DECLARED_CONTENT_SIZE),
        ("largest", DECLARED_TERMINAL_SIZE, DECLARED_CONTENT_SIZE),
        ("smallest", TERMINAL_SIZE, TWO_LINE_CONTENT_SIZE),
    ] {
        let handler = RequestHandler::new();
        let session = session_name("sizeless-inferred-policy");
        let declared_pid = 92_260;
        let sizeless_pid = 92_261;
        let _declared_rx = seed_two_line_status_geometry(&handler, &session, declared_pid).await;

        // Registered while the session still measures 80x24, and never resized.
        let _sizeless_rx = register_sizeless_attach(&handler, sizeless_pid, &session).await;
        assert!(attached_client_size_is_inferred(&handler, sizeless_pid).await);
        handler
            .handle_attached_resize(declared_pid, DECLARED_TERMINAL_SIZE)
            .await
            .expect("declared client resize succeeds");

        set_window_size_policy(&handler, &session, policy).await;

        assert_eq!(
            session_terminal_size(&handler, &session).await,
            expected_terminal,
            "window-size {policy} must rank the inferred anchor by its outer \
             terminal rows"
        );
        assert_eq!(
            session_content_size(&handler, &session).await,
            expected_content,
            "window-size {policy} must subtract the status rows once from the \
             selected anchor"
        );
        assert!(
            attached_client_size_is_inferred(&handler, sizeless_pid).await,
            "policy selection must not silently promote an inferred client"
        );
    }
}

#[tokio::test]
async fn manual_window_size_never_resizes_for_a_sizeless_client() {
    let handler = RequestHandler::new();
    let session = session_name("sizeless-manual");
    let _declared_rx = seed_two_line_status_geometry(&handler, &session, 92_170).await;
    set_window_size_policy(&handler, &session, "manual").await;

    let _sizeless_rx = register_sizeless_attach(&handler, 92_171, &session).await;
    for value in ["off", "2"] {
        set_session_status(&handler, &session, value).await;
    }

    assert_eq!(
        session_terminal_size(&handler, &session).await,
        TERMINAL_SIZE
    );
    assert_eq!(
        session_content_size(&handler, &session).await,
        TWO_LINE_CONTENT_SIZE,
        "window-size manual must not resize for any client"
    );
}

#[tokio::test]
async fn read_only_sizeless_attach_acquires_no_sizing_authority() {
    let handler = RequestHandler::new();
    let session = session_name("sizeless-read-only");
    let _declared_rx = seed_two_line_status_geometry(&handler, &session, 92_180).await;
    let declared_size = TerminalSize {
        cols: 100,
        rows: 30,
    };
    handler
        .handle_attached_resize(92_180, declared_size)
        .await
        .expect("declared client resize succeeds");

    let _read_only_rx = register_sizeless_attach_with_flags(
        &handler,
        92_181,
        &session,
        super::super::attach_support::ClientFlags::default().with_read_only(),
    )
    .await;
    handler
        .handle_attached_resize(92_181, TerminalSize { cols: 40, rows: 10 })
        .await
        .expect("read-only resize is accepted but owns nothing");

    assert_eq!(
        session_terminal_size(&handler, &session).await,
        declared_size,
        "a read-only client never acquires sizing authority"
    );
    assert_eq!(
        session_content_size(&handler, &session).await,
        TerminalSize {
            cols: 100,
            rows: 28
        }
    );
}

#[tokio::test]
async fn ignore_size_sizeless_attach_acquires_no_sizing_authority() {
    let handler = RequestHandler::new();
    let session = session_name("sizeless-ignore-size");
    let _declared_rx = seed_two_line_status_geometry(&handler, &session, 92_190).await;

    let _ignored_rx = register_sizeless_attach_with_flags(
        &handler,
        92_191,
        &session,
        super::super::attach_support::ClientFlags::IGNORESIZE,
    )
    .await;
    for value in ["off", "2"] {
        set_session_status(&handler, &session, value).await;
    }

    assert_eq!(
        session_terminal_size(&handler, &session).await,
        TERMINAL_SIZE
    );
    assert_eq!(
        session_content_size(&handler, &session).await,
        TWO_LINE_CONTENT_SIZE,
        "an ignore-size client must not drive session geometry"
    );
}

/// Leaves `session` with a two-line status, an 80x24 outer terminal and an
/// 80x22 content window, driven by a declared client that stays attached.
async fn seed_two_line_status_geometry(
    handler: &RequestHandler,
    session: &SessionName,
    declared_pid: u32,
) -> mpsc::UnboundedReceiver<AttachControl> {
    create_session_with_status_two(handler, session).await;
    let (_attach_id, mut control_rx) = register_declared_attach(
        handler,
        declared_pid,
        session,
        TERMINAL_SIZE,
        super::super::attach_support::ClientFlags::default(),
    )
    .await;
    while control_rx.try_recv().is_ok() {}
    assert_eq!(session_terminal_size(handler, session).await, TERMINAL_SIZE);
    assert_eq!(
        session_content_size(handler, session).await,
        TWO_LINE_CONTENT_SIZE
    );
    control_rx
}

/// The row a `CSI row;1H` placed the cursor on immediately before `needle`.
fn cursor_row_before(frame: &str, needle: &str) -> u16 {
    let (prefix, _) = frame
        .split_once(needle)
        .unwrap_or_else(|| panic!("frame must contain {needle:?}, got {frame:?}"));
    prefix
        .match_indices("\x1b[")
        .filter_map(|(index, _)| {
            let (parameters, _) = prefix[index + 2..].split_once('H')?;
            parameters.strip_suffix(";1")?.parse::<u16>().ok()
        })
        .last()
        .unwrap_or_else(|| panic!("no cursor positioning before {needle:?}, got {frame:?}"))
}

async fn create_session_with_status_two(handler: &RequestHandler, session: &SessionName) {
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(TERMINAL_SIZE),
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);
    set_session_status(handler, session, "2").await;
}

async fn register_sizeless_attach(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &SessionName,
) -> mpsc::UnboundedReceiver<AttachControl> {
    register_sizeless_attach_with_flags(
        handler,
        requester_pid,
        session,
        super::super::attach_support::ClientFlags::default(),
    )
    .await
}

async fn register_sizeless_attach_with_flags(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &SessionName,
    flags: super::super::attach_support::ClientFlags,
) -> mpsc::UnboundedReceiver<AttachControl> {
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach_with_access(
            requester_pid,
            session.clone(),
            None,
            attach_registration(control_tx, flags, None),
        )
        .await
        .expect("sizeless attach registration succeeds");
    control_rx
}

async fn register_declared_attach(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &SessionName,
    size: TerminalSize,
    flags: super::super::attach_support::ClientFlags,
) -> (u64, mpsc::UnboundedReceiver<AttachControl>) {
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let attach_id = handler
        .register_attach_with_access(
            requester_pid,
            session.clone(),
            None,
            attach_registration(control_tx, flags, Some(size)),
        )
        .await
        .expect("declared attach registration succeeds");
    handler
        .handle_attached_resize(requester_pid, size)
        .await
        .expect("initial declared client size is accepted");
    (attach_id, control_rx)
}

fn attach_registration(
    control_tx: mpsc::UnboundedSender<AttachControl>,
    flags: super::super::attach_support::ClientFlags,
    client_size: Option<TerminalSize>,
) -> AttachRegistration {
    let uid = current_owner_uid();
    AttachRegistration {
        control_tx,
        control_backlog: Arc::new(AtomicUsize::new(0)),
        closing: Arc::new(AtomicBool::new(false)),
        persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
        terminal_context: OuterTerminalContext::default(),
        client_title: None,
        flags,
        render_stream: false,
        uid,
        user: rmux_os::identity::UserIdentity::Uid(uid),
        can_write: true,
        client_size,
    }
}

async fn attached_client_size(handler: &RequestHandler, attach_pid: u32) -> TerminalSize {
    handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get(&attach_pid)
        .expect("attached client is tracked")
        .client_size
}

async fn attached_client_size_is_inferred(handler: &RequestHandler, attach_pid: u32) -> bool {
    handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get(&attach_pid)
        .expect("attached client is tracked")
        .client_size_is_inferred()
}

async fn session_terminal_size(handler: &RequestHandler, session: &SessionName) -> TerminalSize {
    handler
        .state
        .lock()
        .await
        .sessions
        .session(session)
        .expect("session exists")
        .terminal_size()
}

async fn session_content_size(handler: &RequestHandler, session: &SessionName) -> TerminalSize {
    handler
        .state
        .lock()
        .await
        .sessions
        .session(session)
        .expect("session exists")
        .window()
        .size()
}

async fn set_session_status(handler: &RequestHandler, session: &SessionName, value: &str) {
    set_session_option(handler, session, OptionName::Status, value).await;
}

async fn set_session_option(
    handler: &RequestHandler,
    session: &SessionName,
    option: OptionName,
    value: &str,
) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(session.clone()),
            option,
            value: value.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

async fn set_window_size_policy(handler: &RequestHandler, session: &SessionName, value: &str) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(WindowTarget::with_window(session.clone(), 0)),
            option: OptionName::WindowSize,
            value: value.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}
