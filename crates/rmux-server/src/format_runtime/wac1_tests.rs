//! `#{window_active_clients}` counts the CLIENTS whose session is currently on
//! this window, wherever they are attached.
//!
//! The expectations come from the primary text, never from the accessor under
//! test. tmux answers nothing without a winlink, and otherwise walks its ONE
//! global client list, skips every client with no session, and counts each
//! remaining client whose `session->curw->window` is this format's window
//! (`format.c:772-795`). There is no group restriction, no winlink multiplier
//! and no branch on the session the format is being read from.
//!
//! Measured on this lot's new tmux 3.7b image, LC_ALL=C, in ONE oracle cell with
//! ONE private client and the two readers A and B, BEFORE this file was written
//! (`wac1/cells/o1/INVENTORIES.json`). `zulu` held reader A in
//! window 0 and reader B in window 1, `alpha` joined it, `zulu` was made current
//! on B and `alpha` on A, and `list-panes -s -t <session> -F
//! '#{pane_id}|#{pane_title}|#{window_active_clients}'` answered:
//!
//!   no client at all, read from zulu        `A=0  B=0`
//!   one client on alpha, read from zulu     `A=1  B=0`
//!   the same, read from alpha               `A=1  B=0`
//!   alpha made current on B, read from zulu `A=0  B=1`
//!   alpha back on A, read from zulu         `A=1  B=0`
//!   the client closed, read from zulu       `A=0  B=0`
//!
//! Scope of each witness, stated plainly. MEASURED: those six inventories, which
//! the first and third witnesses hold. SOURCE-DERIVED and explicitly NOT
//! measured by that one-client table: two clients, a duplicate link, a shared
//! alias across sessions that are in no group, an unrelated window at the same
//! local index, and rows for clients with no session or a session that is gone -
//! the second witness names each of them as such. The fourth is a compatibility
//! guard on this runtime's own boundary, not a parity claim.

use rmux_core::formats::FormatContext;
use rmux_core::{PaneId, WindowId};
use rmux_proto::{SessionName, TerminalSize, WindowTarget};

use crate::client_activity::ClientActivityTimestamp;
use crate::client_format::{ClientFormatBindings, ClientLoopSnapshot};
use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::HandlerState;

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
/// The one column of the measured inventory this lot is about.
const COUNT: &str = "#{window_active_clients}";

fn name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn window(session: &str, index: u32) -> WindowTarget {
    WindowTarget::with_window(name(session), index)
}

/// One client the server has already created, on the session it is attached to.
fn client(label: &str, order: u64, session: &str) -> ClientLoopSnapshot {
    ClientLoopSnapshot {
        bindings: ClientFormatBindings {
            name: label.to_owned(),
            session: session.to_owned(),
            width: "80".to_owned(),
            height: "24".to_owned(),
            ..ClientFormatBindings::default()
        },
        order,
        activity_at: ClientActivityTimestamp::from_parts(0, 0),
    }
}

/// The journey's world: `zulu` holds reader A in window 0 and reader B in
/// window 1, `alpha` joins it, and each session is put on its own window.
fn world() -> HandlerState {
    let mut state = HandlerState::default();
    state
        .sessions
        .create_session(name("zulu"), SIZE)
        .expect("the creating session");
    let second = state
        .sessions
        .insert_window_with_initial_pane(&name("zulu"), 1, SIZE)
        .expect("its second window");
    assert_eq!(second.window_index, 1);
    let _ = state
        .sessions
        .create_grouped_session_with_base_index(name("alpha"), SIZE, 0, name("zulu"))
        .expect("the peer joins the creator");
    state
        .sessions
        .select_window(&window("zulu", 1))
        .expect("zulu is put on B");
    state
        .sessions
        .select_window(&window("alpha", 0))
        .expect("alpha is put on A");
    state
}

fn window_id(state: &HandlerState, session: &str, index: u32) -> WindowId {
    state
        .sessions
        .window(&window(session, index))
        .expect("the window exists")
        .id()
}

fn pane_id(state: &HandlerState, session: &str, index: u32) -> PaneId {
    state
        .sessions
        .window(&window(session, index))
        .expect("the window exists")
        .pane(0)
        .expect("its first pane")
        .id()
}

/// How many of these clients are attached to this very session - which is what
/// the local `#{session_attached}` has always supplied.
fn attached_locally(clients: &[ClientLoopSnapshot], session: &str) -> usize {
    clients
        .iter()
        .filter(|entry| entry.bindings.session == session)
        .count()
}

/// Reads `#{window_active_clients}` for one occurrence, exactly as the
/// list-panes and display-message callers build their context.
fn read(state: &HandlerState, session: &str, index: u32, clients: &[ClientLoopSnapshot]) -> String {
    let view = state
        .sessions
        .session(&name(session))
        .expect("the session exists");
    let target = view.window_at(index).expect("the window exists");
    let active = view.active_window_index() == index;
    let base = FormatContext::new()
        .with_window(index, target, active, false)
        .with_session_attached(attached_locally(clients, session));
    let runtime = RuntimeFormatContext::new(base)
        .with_state(state)
        .with_session(view)
        .with_window(index, target)
        .with_client_loop_snapshots(clients);
    render_runtime_template(COUNT, &runtime, false)
}

#[test]
fn a_detached_member_finds_the_window_its_peer_client_is_viewing() {
    let state = world();
    // The two windows are two distinct canonical windows, and both sessions
    // name the SAME ones: what follows is about clients, not about links.
    let a = window_id(&state, "zulu", 0);
    let b = window_id(&state, "zulu", 1);
    assert_ne!(a, b, "A and B are two different canonical windows");
    assert_eq!(window_id(&state, "alpha", 0), a);
    assert_eq!(window_id(&state, "alpha", 1), b);
    assert_eq!(pane_id(&state, "alpha", 0), pane_id(&state, "zulu", 0));
    // zulu is detached and current on B; alpha holds the only client and is
    // current on A.
    let clients = [client("/dev/ttys019", 1, "alpha")];
    assert_eq!(attached_locally(&clients, "zulu"), 0);
    // Measured: from the DETACHED session, A answers 1 and B answers 0.
    assert_eq!(read(&state, "zulu", 0, &clients), "1");
    assert_eq!(read(&state, "zulu", 1, &clients), "0");
    // Measured: the same window through the peer that holds the client.
    assert_eq!(read(&state, "alpha", 0, &clients), "1");
    assert_eq!(read(&state, "alpha", 1, &clients), "0");
}

#[test]
fn canonical_window_identity_counts_each_client_once_despite_duplicate_links() {
    // SOURCE-DERIVED, and explicitly NOT measured by the one-client oracle
    // table: every clause below follows from `format.c:772-795` walking clients
    // and comparing one canonical window.
    let mut state = world();
    let shared = window_id(&state, "zulu", 0);
    // zulu links the SAME canonical window a second time, at another index.
    state
        .sessions
        .link_window(&window("zulu", 2), shared)
        .expect("a duplicate link of the shared window");
    assert_eq!(window_id(&state, "zulu", 2), shared);
    // A session in NO group with them holds an explicit alias of it, and is
    // current on that alias.
    state
        .sessions
        .create_session(name("beta"), SIZE)
        .expect("a session outside the group");
    state
        .sessions
        .link_window(&window("beta", 5), shared)
        .expect("an explicit alias across sessions that are in no group");
    state
        .sessions
        .select_window(&window("beta", 5))
        .expect("beta is current on the shared window");
    // And an unrelated session whose own window carries the same local index 0.
    state
        .sessions
        .create_session(name("gamma"), SIZE)
        .expect("an unrelated session");
    assert_ne!(window_id(&state, "gamma", 0), shared);

    // Two clients on alpha count twice; the duplicate links multiply nothing.
    let two = [
        client("/dev/ttys019", 1, "alpha"),
        client("/dev/ttys020", 2, "alpha"),
    ];
    assert_eq!(read(&state, "zulu", 0, &two), "2");
    assert_eq!(
        read(&state, "zulu", 2, &two),
        "2",
        "the same window, read through its duplicate link"
    );
    assert_eq!(read(&state, "zulu", 1, &two), "0");
    // The client of the session in no group with them counts for that window
    // too, because it is the same canonical window.
    let mixed = [
        client("/dev/ttys019", 1, "alpha"),
        client("/dev/ttys021", 3, "beta"),
    ];
    assert_eq!(read(&state, "zulu", 0, &mixed), "2");
    // A client whose session is current on an unrelated window at the same
    // local index does not count.
    let elsewhere = [
        client("/dev/ttys019", 1, "alpha"),
        client("/dev/ttys022", 4, "gamma"),
    ];
    assert_eq!(read(&state, "zulu", 0, &elsewhere), "1");
    assert_eq!(read(&state, "gamma", 0, &elsewhere), "1");
    // A row with no session at all, and a row naming a session that is gone,
    // are both skipped rather than counted or refused.
    let ignored = [
        client("/dev/ttys019", 1, "alpha"),
        client("/dev/ttys023", 5, ""),
        client("/dev/ttys024", 6, "vanished"),
    ];
    assert_eq!(read(&state, "zulu", 0, &ignored), "1");
}

#[test]
fn moving_the_peer_active_window_moves_the_count_without_replacing_the_readers() {
    let mut state = world();
    let a = window_id(&state, "zulu", 0);
    let b = window_id(&state, "zulu", 1);
    let readers = (pane_id(&state, "zulu", 0), pane_id(&state, "zulu", 1));
    let clients = [client("/dev/ttys019", 1, "alpha")];
    // Measured: 1/0, then 0/1 once the peer moves, then 1/0 when it returns.
    assert_eq!(
        (
            read(&state, "zulu", 0, &clients),
            read(&state, "zulu", 1, &clients)
        ),
        ("1".to_owned(), "0".to_owned())
    );
    state
        .sessions
        .select_window(&window("alpha", 1))
        .expect("the peer moves to B");
    assert_eq!(
        (
            read(&state, "zulu", 0, &clients),
            read(&state, "zulu", 1, &clients)
        ),
        ("0".to_owned(), "1".to_owned())
    );
    state
        .sessions
        .select_window(&window("alpha", 0))
        .expect("the peer returns to A");
    assert_eq!(
        (
            read(&state, "zulu", 0, &clients),
            read(&state, "zulu", 1, &clients)
        ),
        ("1".to_owned(), "0".to_owned())
    );
    // Measured: with the client gone, both answer zero.
    assert_eq!(read(&state, "zulu", 0, &[]), "0");
    assert_eq!(read(&state, "zulu", 1, &[]), "0");
    // Nothing of the readers moved while the count did, and zulu never left B.
    assert_eq!(window_id(&state, "zulu", 0), a);
    assert_eq!(window_id(&state, "zulu", 1), b);
    assert_eq!(
        (pane_id(&state, "zulu", 0), pane_id(&state, "zulu", 1)),
        readers
    );
    assert_eq!(
        state
            .sessions
            .session(&name("zulu"))
            .expect("zulu exists")
            .active_window_index(),
        1
    );
}

#[test]
fn an_incomplete_runtime_keeps_its_local_fallback_without_claiming_a_global_count() {
    // This runtime's own boundary, read through the public format only. It is
    // NOT a parity claim: the primary answers nothing at all without a winlink,
    // and the public 0 a bare runtime prints is deliberately kept.
    let state = world();
    let view = state
        .sessions
        .session(&name("zulu"))
        .expect("the session exists");
    let target = view.window_at(1).expect("the window exists");
    // WindowActive=1 and SessionAttached=2, supplied exactly as a caller does.
    let base = FormatContext::new()
        .with_window(1, target, true, false)
        .with_session_attached(2);
    // Incomplete: no store, no session and no client inventory ever reached
    // this runtime, so no global count can be supplied and the historical local
    // calculation is what answers.
    assert_eq!(
        render_runtime_template(COUNT, &RuntimeFormatContext::new(base.clone()), false),
        "2"
    );
    // Complete, with an inventory that really holds no client: a global count
    // CAN be supplied here, and it is zero.
    let complete = RuntimeFormatContext::new(base)
        .with_state(&state)
        .with_session(view)
        .with_window(1, target)
        .with_client_loop_snapshots(&[]);
    assert_eq!(render_runtime_template(COUNT, &complete, false), "0");
}
