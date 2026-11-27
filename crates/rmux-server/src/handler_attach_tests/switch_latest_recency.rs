//! A client that switches into a window becomes that window's newest sizing
//! authority, and the frame it receives is the geometry the window keeps.
//!
//! tmux 3.7b's `cmd_switch_client_exec` assigns `s->curw->window->latest = c`
//! and only then calls `recalculate_sizes()`, so under `window-size latest` the
//! moving client outranks every client that was already there — including one
//! that attached after it. rmux selects the same answer for the frame it sends,
//! but the recency it selected with has to be the recency the moving client's
//! registration then *carries*: the very next reconcile of the same linked
//! family re-derives the winner from `active_attach`, and a registration still
//! holding its original arrival order loses to the resident client again.
//!
//! Measured against the pinned tmux 3.7b (`tmux -V` → `3.7b`, this Mac's build
//! of source `e802909d…`, binary sha256 `5bfe78e4…`) on macOS 26.5.2 arm64,
//! 2026-08-01. `alpha:0` is linked into `beta:1`, `grouped` is a grouped alias
//! of `beta` showing the same window, every session has `status off`, a 120x50
//! PTY client attaches to `alpha`, a *later* 100x40 PTY client attaches to
//! `beta`, and the 120x50 client then switches to `beta`:
//!
//! ```text
//! window-size  before   at switch  settled  alpha:0  beta:1  grouped:1
//! latest       100x40   120x50     120x50   120x50   120x50  120x50
//! largest      120x50   120x50     120x50   120x50   120x50  120x50
//! smallest     100x40   100x40     100x40   100x40   100x40  100x40
//! manual       80x24    80x24      80x24    80x24    80x24   80x24
//! ```
//!
//! Both clients stay attached to `beta` afterwards, at 120x50 and 100x40. The
//! `manual` row is the geometry the sessions were created with — 80x24 in the
//! oracle, `CLIENT_SIZE` here — because `manual` never resizes at all.
//!
//! The same probe measured the client switching to the session it is *already*
//! on: `before=100x40`, `settled=120x50` across all three aliases, so tmux's
//! assignment is unconditional and is not a side effect of changing session.
//!
//! Probe and log: `m9-latest/oracle_latest_switch.py`,
//! `oracle-latest-switch.log`.

use super::*;

use super::super::attach_support::ClientFlags;
use super::switch_frame_geometry::{
    active_window_index, frame_geometry, linked_alias_sessions, pane_pty_size,
    register_declared_attach, set_session_status, set_window_size_policy, window_content_size,
    CLIENT_SIZE, SOURCE_WINDOW_INDEX, STATUS_OFF, SWITCHING_PID, TARGET_WINDOW_INDEX,
};

/// The geometry the moving client owns. It registers *first*, so under `latest`
/// it is the older vote right up to the moment it switches.
const MOVING_CLIENT_SIZE: TerminalSize = TerminalSize {
    cols: 120,
    rows: 50,
};
/// The geometry of the client already resident in the target session, which
/// registers *after* the moving one and is therefore the newest vote until the
/// switch. It is also the geometry the sessions are created with, so the
/// `manual` row's "never resized" answer and this client's vote coincide.
const RESIDENT_CLIENT_SIZE: TerminalSize = CLIENT_SIZE;
const RESIDENT_PID: u32 = 93_102;

/// `(window-size, geometry before the switch, geometry the switch must commit)`,
/// transcribed from the oracle table in this module's header.
const LATEST_SWITCH_MATRIX: [(&str, TerminalSize, TerminalSize); 4] = [
    ("latest", RESIDENT_CLIENT_SIZE, MOVING_CLIENT_SIZE),
    ("largest", MOVING_CLIENT_SIZE, MOVING_CLIENT_SIZE),
    ("smallest", RESIDENT_CLIENT_SIZE, RESIDENT_CLIENT_SIZE),
    ("manual", RESIDENT_CLIENT_SIZE, RESIDENT_CLIENT_SIZE),
];

/// The two production commands that move an already-attached client. Both reach
/// `commit_attached_session_switch`, so both must commit one coherent geometry.
#[derive(Clone, Copy, Debug)]
enum AttachedSwitchEntry {
    /// `switch-client`, dispatched by the client's own pid.
    SwitchClient,
    /// A sized `attach-session` issued inside the client's own registration,
    /// which is how attached key dispatch and the command prompt run it.
    SizedAttachSession,
}

/// `alpha:0` linked into `beta:1`, plus a grouped alias of `beta` showing the
/// same window. One physical window, three names for it, one `window-size`.
struct LinkedFamily {
    alpha: SessionName,
    beta: SessionName,
    grouped: SessionName,
}

impl LinkedFamily {
    /// Every alias of the one shared window, with the index it is reached by.
    fn aliases(&self) -> [(&SessionName, u32); 3] {
        [
            (&self.beta, TARGET_WINDOW_INDEX),
            (&self.alpha, SOURCE_WINDOW_INDEX),
            (&self.grouped, TARGET_WINDOW_INDEX),
        ]
    }
}

/// The measured transition: the switching client owns the window afterwards,
/// through every alias and down to the real pane PTY, and the frame it was sent
/// is that same geometry rather than one the family immediately abandons.
#[tokio::test]
async fn an_attached_switch_makes_the_moving_client_the_newest_sizing_authority() {
    let mut regressions = Vec::new();
    for entry in [
        AttachedSwitchEntry::SwitchClient,
        AttachedSwitchEntry::SizedAttachSession,
    ] {
        for (policy, before, expected) in LATEST_SWITCH_MATRIX {
            let handler = RequestHandler::new();
            let family = linked_family_with_grouped_alias(&handler, policy).await;
            let mut moving_rx = register_declared_attach(
                &handler,
                SWITCHING_PID,
                &family.alpha,
                MOVING_CLIENT_SIZE,
            )
            .await;
            // Registered second, so this client is the newest vote until the
            // switch happens.
            let _resident_rx = register_declared_attach(
                &handler,
                RESIDENT_PID,
                &family.beta,
                RESIDENT_CLIENT_SIZE,
            )
            .await;
            drain_attach_controls(&mut moving_rx);

            let staged = window_content_size(&handler, &family.beta, TARGET_WINDOW_INDEX).await;
            assert_eq!(
                staged, before,
                "{entry:?} window-size={policy}: the resident client must own the \
                 shared window before the switch"
            );

            let response = move_attached_client(&handler, entry, &family.alpha, &family.beta).await;
            if !matches!(response, Response::SwitchClient(_)) {
                regressions.push(format!(
                    "{entry:?} window-size={policy}: the switch must succeed, got \
                     {response:?}"
                ));
                continue;
            }

            let framed =
                frame_geometry(recv_switch_target(&mut moving_rx, "latest switch frame").await);
            if framed != expected {
                regressions.push(format!(
                    "{entry:?} window-size={policy}: switch frame is {framed:?}, \
                     expected {expected:?}"
                ));
            }
            regressions.extend(
                family_geometry_regressions(
                    &handler,
                    &family,
                    expected,
                    &format!("{entry:?} window-size={policy} settled"),
                )
                .await,
            );
            if framed != expected {
                continue;
            }
            let settled = window_content_size(&handler, &family.beta, TARGET_WINDOW_INDEX).await;
            if settled != framed {
                regressions.push(format!(
                    "{entry:?} window-size={policy}: the frame carried {framed:?} while \
                     the window kept {settled:?}"
                ));
            }

            // A later reconcile of the same window must reach the same answer:
            // re-applying the policy is a production trigger for exactly that.
            set_window_size_policy(&handler, &family.beta, TARGET_WINDOW_INDEX, policy).await;
            regressions.extend(
                family_geometry_regressions(
                    &handler,
                    &family,
                    expected,
                    &format!("{entry:?} window-size={policy} reconciled"),
                )
                .await,
            );
        }
    }

    assert!(
        regressions.is_empty(),
        "a client that switches in becomes the window's newest sizing authority \
         and keeps it, but {regressions:?}"
    );
}

/// tmux assigns the window's latest client unconditionally, so a client that
/// switches to the session it is already on becomes the newest authority too.
///
/// This row has no source-session reconcile to expose a stale registration, so
/// it turns on the later reconcile: the recency must have been *committed*, not
/// merely used to pick one frame.
#[tokio::test]
async fn a_current_session_attached_switch_still_makes_the_moving_client_newest() {
    let mut regressions = Vec::new();
    for entry in [
        AttachedSwitchEntry::SwitchClient,
        AttachedSwitchEntry::SizedAttachSession,
    ] {
        let handler = RequestHandler::new();
        let family = linked_family_with_grouped_alias(&handler, "latest").await;
        let mut moving_rx =
            register_declared_attach(&handler, SWITCHING_PID, &family.alpha, MOVING_CLIENT_SIZE)
                .await;
        let _resident_rx =
            register_declared_attach(&handler, RESIDENT_PID, &family.beta, RESIDENT_CLIENT_SIZE)
                .await;
        drain_attach_controls(&mut moving_rx);
        assert_eq!(
            window_content_size(&handler, &family.beta, TARGET_WINDOW_INDEX).await,
            RESIDENT_CLIENT_SIZE,
            "{entry:?}: the resident client must own the shared window first"
        );

        let alpha = family.alpha.clone();
        let response = move_attached_client(&handler, entry, &family.alpha, &alpha).await;
        if !matches!(response, Response::SwitchClient(_)) {
            regressions.push(format!(
                "{entry:?}: switching to the current session must succeed, got {response:?}"
            ));
            continue;
        }

        let framed = frame_geometry(
            recv_switch_target(&mut moving_rx, "current-session switch frame").await,
        );
        if framed != MOVING_CLIENT_SIZE {
            regressions.push(format!(
                "{entry:?}: switch frame is {framed:?}, expected {MOVING_CLIENT_SIZE:?}"
            ));
        }
        regressions.extend(
            family_geometry_regressions(
                &handler,
                &family,
                MOVING_CLIENT_SIZE,
                &format!("{entry:?} settled"),
            )
            .await,
        );
        set_window_size_policy(&handler, &family.beta, TARGET_WINDOW_INDEX, "latest").await;
        regressions.extend(
            family_geometry_regressions(
                &handler,
                &family,
                MOVING_CLIENT_SIZE,
                &format!("{entry:?} reconciled"),
            )
            .await,
        );
    }

    assert!(
        regressions.is_empty(),
        "switching to the session a client is already on still makes it the \
         window's latest, but {regressions:?}"
    );
}

/// Who is allowed to become the newest vote, and who owns no vote to renew.
#[derive(Clone, Copy, Debug)]
enum LatestVoterRow {
    /// `ignore-size` leaves the moving client owning no geometry at all, exactly
    /// as tmux's `ignore_client_size()` skips it. Joining a window must not hand
    /// a sizeless client the window.
    MovingClientIgnoresSize,
    /// An explicitly read-only client is not a sizeless one: it keeps its
    /// geometry, so it becomes the newest vote like any other.
    MovingClientIsReadOnly,
    /// A suspended resident is not a voter, so the moving client is the only
    /// candidate left and owns the window outright.
    ResidentIsSuspended,
}

/// The voter field `latest` decides over must survive a switch unchanged.
#[tokio::test]
async fn a_switch_under_latest_preserves_the_voter_field() {
    let mut regressions = Vec::new();
    for row in [
        LatestVoterRow::MovingClientIgnoresSize,
        LatestVoterRow::MovingClientIsReadOnly,
        LatestVoterRow::ResidentIsSuspended,
    ] {
        let expected = match row {
            // No vote to cast, so the resident client keeps the window.
            LatestVoterRow::MovingClientIgnoresSize => RESIDENT_CLIENT_SIZE,
            LatestVoterRow::MovingClientIsReadOnly | LatestVoterRow::ResidentIsSuspended => {
                MOVING_CLIENT_SIZE
            }
        };
        let handler = RequestHandler::new();
        let family = linked_family_with_grouped_alias(&handler, "latest").await;
        let moving_flags = match row {
            LatestVoterRow::MovingClientIgnoresSize => ClientFlags::IGNORESIZE,
            LatestVoterRow::MovingClientIsReadOnly => ClientFlags::READONLY,
            LatestVoterRow::ResidentIsSuspended => ClientFlags::default(),
        };
        let mut moving_rx = register_flagged_attach(
            &handler,
            SWITCHING_PID,
            &family.alpha,
            MOVING_CLIENT_SIZE,
            moving_flags,
        )
        .await;
        let _resident_rx =
            register_declared_attach(&handler, RESIDENT_PID, &family.beta, RESIDENT_CLIENT_SIZE)
                .await;
        drain_attach_controls(&mut moving_rx);
        if matches!(row, LatestVoterRow::ResidentIsSuspended) {
            suspend_attached_client(&handler, RESIDENT_PID).await;
        }

        let response = move_attached_client(
            &handler,
            AttachedSwitchEntry::SwitchClient,
            &family.alpha,
            &family.beta,
        )
        .await;
        if !matches!(response, Response::SwitchClient(_)) {
            regressions.push(format!(
                "{row:?}: the switch must succeed, got {response:?}"
            ));
            continue;
        }
        regressions.extend(
            family_geometry_regressions(&handler, &family, expected, &format!("{row:?} settled"))
                .await,
        );
        set_window_size_policy(&handler, &family.beta, TARGET_WINDOW_INDEX, "latest").await;
        regressions.extend(
            family_geometry_regressions(
                &handler,
                &family,
                expected,
                &format!("{row:?} reconciled"),
            )
            .await,
        );
    }

    assert!(
        regressions.is_empty(),
        "joining a window must not change who owns a size vote, but {regressions:?}"
    );
}

/// Every alias's stored geometry and the real PTY behind it, reported together
/// so one run names every alias that disagrees rather than only the first.
async fn family_geometry_regressions(
    handler: &RequestHandler,
    family: &LinkedFamily,
    expected: TerminalSize,
    phase: &str,
) -> Vec<String> {
    let mut regressions = Vec::new();
    for (alias, window_index) in family.aliases() {
        let stored = window_content_size(handler, alias, window_index).await;
        if stored != expected {
            regressions.push(format!(
                "{phase}: alias {alias}:{window_index} is {stored:?}, expected {expected:?}"
            ));
        }
        let pty = pane_pty_size(handler, alias, window_index).await;
        if pty != expected {
            regressions.push(format!(
                "{phase}: the PTY behind {alias}:{window_index} is {pty:?}, \
                 expected {expected:?}"
            ));
        }
    }
    regressions
}

async fn move_attached_client(
    handler: &RequestHandler,
    entry: AttachedSwitchEntry,
    source: &SessionName,
    target: &SessionName,
) -> Response {
    match entry {
        AttachedSwitchEntry::SwitchClient => {
            handler
                .dispatch(
                    SWITCHING_PID,
                    Request::SwitchClient(SwitchClientRequest {
                        target: target.clone(),
                    }),
                )
                .await
                .response
        }
        AttachedSwitchEntry::SizedAttachSession => {
            let identity = handler.active_attach_identity_for_test(SWITCHING_PID).await;
            super::super::with_expected_attach_and_session_identity(
                identity,
                source.clone(),
                identity.session_id(),
                handler.dispatch(
                    SWITCHING_PID,
                    Request::AttachSessionExt2(Box::new(AttachSessionExt2Request {
                        target: Some(target.clone()),
                        target_spec: Some(target.to_string()),
                        detach_other_clients: false,
                        kill_other_clients: false,
                        read_only: false,
                        skip_environment_update: false,
                        flags: None,
                        working_directory: None,
                        client_terminal: rmux_proto::ClientTerminalContext::default(),
                        client_size: Some(MOVING_CLIENT_SIZE),
                    })),
                ),
            )
            .await
            .response
        }
    }
}

/// `alpha:0` linked into `beta:1`, a grouped alias of `beta` showing the same
/// window, `status off` everywhere, and one `window-size` across all three
/// aliases — rmux keys window options per `(session, index)`, so the oracle's
/// single window option is reproduced by setting each alias.
async fn linked_family_with_grouped_alias(handler: &RequestHandler, policy: &str) -> LinkedFamily {
    let (alpha, beta) = linked_alias_sessions(handler, STATUS_OFF, STATUS_OFF).await;
    let grouped = grouped_alias_session(handler, &beta).await;
    let family = LinkedFamily {
        alpha,
        beta,
        grouped,
    };
    for (alias, window_index) in family.aliases() {
        set_window_size_policy(handler, alias, window_index, policy).await;
    }
    family
}

async fn grouped_alias_session(
    handler: &RequestHandler,
    group_target: &SessionName,
) -> SessionName {
    let grouped = session_name("switch-frame-grouped");
    let created = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(grouped.clone()),
            working_directory: None,
            detached: true,
            size: Some(CLIENT_SIZE),
            environment: None,
            group_target: Some(group_target.clone()),
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(
        matches!(created, Response::NewSession(_)),
        "expected a grouped session, got {created:?}"
    );
    set_session_status(handler, &grouped, STATUS_OFF).await;
    assert_eq!(
        active_window_index(handler, &grouped).await,
        0,
        "the grouped alias must start on its own first window"
    );
    grouped
}

/// `register_declared_attach` with the client flags a real `attach-session -r`
/// or `-f ignore-size` would have parsed.
async fn register_flagged_attach(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &SessionName,
    size: TerminalSize,
    flags: ClientFlags,
) -> mpsc::UnboundedReceiver<AttachControl> {
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let uid = current_owner_uid();
    handler
        .register_attach_with_access(
            requester_pid,
            session.clone(),
            None,
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
                client_size: Some(size),
            },
        )
        .await
        .expect("flagged attach registration succeeds");
    handler
        .handle_attached_resize(requester_pid, size)
        .await
        .expect("declared client size is accepted");
    control_rx
}

async fn suspend_attached_client(handler: &RequestHandler, attach_pid: u32) {
    let response = handler
        .dispatch(
            attach_pid,
            Request::SuspendClient(rmux_proto::SuspendClientRequest {
                target_client: None,
            }),
        )
        .await
        .response;
    assert!(
        matches!(response, Response::SuspendClient(_)),
        "suspend-client must succeed, got {response:?}"
    );
}
