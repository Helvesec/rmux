//! `#{window_active_clients_list}` names the clients whose session is currently
//! on this window, in the order the server created them.
//!
//! The expectations come from the primary text, never from the accessor under
//! test. tmux answers nothing without a winlink, and otherwise walks its ONE
//! global client list, skips every client with no session, and appends the NAME
//! of each remaining client whose `session->curw->window` is this format's
//! window, joined by a single comma; an empty result renders empty
//! (`format.c:797-831`). The order is the global client list's own, which every
//! new client joins at the tail (`server-client.c:327`). There is no group
//! restriction, no winlink multiplier and no branch on the asking session - the
//! same walk `#{window_active_clients}` counts (`format.c:772-795`).
//!
//! Measured on this lot's new tmux 3.7b image, LC_ALL=C, in ONE oracle cell with
//! TWO private clients and the two readers A and B, BEFORE this file was written
//! (`wacl1/cells/o1/INVENTORIES.json`). `zulu` held reader A in
//! window 0 and reader B in window 1, `alpha` joined it, `zulu` was made current
//! on B and `alpha` on A, `cB` attached to zulu FIRST and `cA` to alpha, and
//! `list-windows -t zulu -F '#{pane_id}|#{pane_title}|#{window_active_clients}|`
//! `#{window_active_clients_list}'` answered:
//!
//!   no client            `A 0 []          B 0 []`
//!   cB then cA           `A 1 [cA]        B 1 [cB]`     - the counts are EQUAL
//!   alpha moved to B     `A 0 []          B 2 [cB,cA]`  - creation order
//!   alpha back on A      `A 1 [cA]        B 1 [cB]`     - the row extracted
//!   cA closed            `A 0 []          B 1 [cB]`
//!   cB closed            `A 0 []          B 0 []`
//!
//! `cA` and `cB` are the names that cell measured from `list-clients` against
//! each holder's own pid and tty; they are engine-local data and no literal of
//! them is carried here.
//!
//! Scope of each witness, stated plainly. MEASURED: those six inventories, held
//! by the first and third witnesses. SOURCE-DERIVED and explicitly NOT measured
//! by that two-client table: snapshots handed over unsorted, two clients on one
//! session, a duplicate link, an unrelated window at the same local index, and
//! rows for clients with no session or a session that is gone - the second
//! witness names each of them as such. The fourth is a compatibility guard on
//! this runtime's own boundary, not a parity claim.

use rmux_core::formats::{FormatContext, FormatVariables};
use rmux_core::{PaneId, WindowId};
use rmux_proto::{SessionName, TerminalSize, WindowTarget};

use crate::client_activity::ClientActivityTimestamp;
use crate::client_format::{ClientFormatBindings, ClientLoopSnapshot};
use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::HandlerState;

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
/// The two columns of the measured inventory this lot is about.
const ROW: &str = "#{window_active_clients}|#{window_active_clients_list}";
const LIST: &str = "#{window_active_clients_list}";

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

fn attached_locally(clients: &[ClientLoopSnapshot], session: &str) -> usize {
    clients
        .iter()
        .filter(|entry| entry.bindings.session == session)
        .count()
}

/// Reads a template for one occurrence, exactly as the list-windows and
/// display-message callers build their context.
fn read(
    state: &HandlerState,
    session: &str,
    index: u32,
    clients: &[ClientLoopSnapshot],
    template: &str,
) -> String {
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
    render_runtime_template(template, &runtime, false)
}

#[test]
fn two_equal_counts_are_discriminated_by_the_real_client_name() {
    let state = world();
    // The two windows are two distinct canonical windows, and both sessions
    // name the SAME ones.
    let a = window_id(&state, "zulu", 0);
    let b = window_id(&state, "zulu", 1);
    assert_ne!(a, b, "A and B are two different canonical windows");
    assert_eq!(window_id(&state, "alpha", 0), a);
    assert_eq!(window_id(&state, "alpha", 1), b);
    assert_eq!(pane_id(&state, "alpha", 0), pane_id(&state, "zulu", 0));
    // cB attached first, to zulu; cA second, to alpha.
    let clients = [client("cB-name", 1, "zulu"), client("cA-name", 2, "alpha")];
    // Measured: from the detached-from-A session, both counts are 1 and only
    // the names say which window each client is on.
    assert_eq!(read(&state, "zulu", 0, &clients, ROW), "1|cA-name");
    assert_eq!(read(&state, "zulu", 1, &clients, ROW), "1|cB-name");
    // The same answer through the peer that holds cA.
    assert_eq!(read(&state, "alpha", 0, &clients, ROW), "1|cA-name");
    assert_eq!(read(&state, "alpha", 1, &clients, ROW), "1|cB-name");
}

#[test]
fn client_names_keep_global_order_without_duplicate_window_links() {
    // SOURCE-DERIVED, and explicitly NOT measured by the two-client oracle
    // table: every clause below follows from `format.c:797-831` walking the
    // global client list once and comparing one canonical window.
    let mut state = world();
    let shared = window_id(&state, "zulu", 0);
    // zulu links the SAME canonical window a second time, at another index.
    state
        .sessions
        .link_window(&window("zulu", 2), shared)
        .expect("a duplicate link of the shared window");
    assert_eq!(window_id(&state, "zulu", 2), shared);
    // An unrelated session whose own window carries the same local index 0.
    state
        .sessions
        .create_session(name("gamma"), SIZE)
        .expect("an unrelated session");
    assert_ne!(window_id(&state, "gamma", 0), shared);

    // Handed over unsorted, the answer is still the global creation order.
    let unsorted = [
        client("later", 9, "alpha"),
        client("earlier", 2, "alpha"),
        client("middle", 5, "alpha"),
    ];
    assert_eq!(
        read(&state, "zulu", 0, &unsorted, LIST),
        "earlier,middle,later"
    );
    // Two clients of ONE session both contribute, and the duplicate link
    // multiplies neither of them.
    assert_eq!(
        read(&state, "zulu", 2, &unsorted, ROW),
        "3|earlier,middle,later"
    );
    assert_eq!(read(&state, "zulu", 1, &unsorted, ROW), "0|");
    // A client whose session is current on an unrelated window at the same
    // local index is not named here, and is named for its own window.
    let elsewhere = [
        client("on-alpha", 1, "alpha"),
        client("on-gamma", 2, "gamma"),
    ];
    assert_eq!(read(&state, "zulu", 0, &elsewhere, LIST), "on-alpha");
    assert_eq!(read(&state, "gamma", 0, &elsewhere, LIST), "on-gamma");
    // A row with no session at all, and a row naming a session that is gone,
    // are both skipped rather than named or refused.
    let ignored = [
        client("real", 1, "alpha"),
        client("detached", 2, ""),
        client("vanished-session", 3, "vanished"),
    ];
    assert_eq!(read(&state, "zulu", 0, &ignored, ROW), "1|real");
}

#[test]
fn moving_and_closing_clients_moves_the_names_without_replacing_readers() {
    let mut state = world();
    let a = window_id(&state, "zulu", 0);
    let b = window_id(&state, "zulu", 1);
    let readers = (pane_id(&state, "zulu", 0), pane_id(&state, "zulu", 1));
    let both = [client("cB-name", 1, "zulu"), client("cA-name", 2, "alpha")];
    // Measured: split, then both on B in creation order, then split again.
    assert_eq!(
        (
            read(&state, "zulu", 0, &both, ROW),
            read(&state, "zulu", 1, &both, ROW)
        ),
        ("1|cA-name".to_owned(), "1|cB-name".to_owned())
    );
    state
        .sessions
        .select_window(&window("alpha", 1))
        .expect("alpha moves to B");
    assert_eq!(
        (
            read(&state, "zulu", 0, &both, ROW),
            read(&state, "zulu", 1, &both, ROW)
        ),
        ("0|".to_owned(), "2|cB-name,cA-name".to_owned())
    );
    state
        .sessions
        .select_window(&window("alpha", 0))
        .expect("alpha returns to A");
    assert_eq!(
        (
            read(&state, "zulu", 0, &both, ROW),
            read(&state, "zulu", 1, &both, ROW)
        ),
        ("1|cA-name".to_owned(), "1|cB-name".to_owned())
    );
    // Measured: cA leaves, then cB; no stale name survives either close.
    let only_cb = [client("cB-name", 1, "zulu")];
    assert_eq!(read(&state, "zulu", 0, &only_cb, ROW), "0|");
    assert_eq!(read(&state, "zulu", 1, &only_cb, ROW), "1|cB-name");
    assert_eq!(read(&state, "zulu", 0, &[], ROW), "0|");
    assert_eq!(read(&state, "zulu", 1, &[], ROW), "0|");
    // Nothing of the readers moved while the names did, and zulu never left B.
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
fn unavailable_context_keeps_supplied_and_empty_name_boundaries() {
    // This runtime's own boundary, read through the public format only. It is
    // NOT a parity claim: the primary answers nothing at all without a winlink,
    // and the empty this registered name renders is deliberately kept.
    let state = world();
    let view = state
        .sessions
        .session(&name("zulu"))
        .expect("the session exists");
    let target = view.window_at(1).expect("the window exists");
    let base = FormatContext::new().with_window(1, target, true, false);
    // No store, no session and no client inventory ever reached this runtime:
    // no list can be supplied, and the registered name keeps its empty answer.
    let bare = RuntimeFormatContext::new(base.clone());
    assert_eq!(
        bare.format_value_by_name("window_active_clients_list"),
        Some(String::new())
    );
    // Complete, with an inventory that really holds no client: the same empty.
    let complete = RuntimeFormatContext::new(base.clone())
        .with_state(&state)
        .with_session(view)
        .with_window(1, target)
        .with_client_loop_snapshots(&[]);
    assert_eq!(
        complete.format_value_by_name("window_active_clients_list"),
        Some(String::new())
    );
    assert_eq!(render_runtime_template(LIST, &complete, false), "");
    // A value supplied by the caller still wins over anything this runtime
    // would answer for the same name.
    let one = [client("cB-name", 1, "zulu")];
    let supplied =
        RuntimeFormatContext::new(base.with_named_value("window_active_clients_list", "supplied"))
            .with_state(&state)
            .with_session(view)
            .with_window(1, target)
            .with_client_loop_snapshots(&one);
    assert_eq!(
        supplied
            .format_value_by_name("window_active_clients_list")
            .as_deref(),
        Some("supplied")
    );
    // A name this table does not know stays absent, not empty.
    assert_eq!(
        bare.format_value_by_name("window_active_clients_lists"),
        None
    );
}

/// Flags this runtime computes from the base context are values, even when no
/// runtime window was ever handed over.
///
/// `#{window_flags}` and `#{window_raw_flags}` are built from the base
/// context's own `window_active` and `window_last_flag`, so a caller that
/// supplies a window in its base and no runtime window still gets the `*` or
/// `-` it has always got. What is empty there is the flags themselves, not the
/// answer, and only an empty computation with no runtime window at all is this
/// server's historical empty - which keeps its own answer and gets no folder.
///
/// A compatibility and presence guard on this runtime's boundary; no
/// contextless parity with the primary is claimed for it.
#[test]
fn derived_window_flags_keep_nonempty_values_without_runtime_window() {
    let state = world();
    let view = state
        .sessions
        .session(&name("zulu"))
        .expect("the session exists");
    let target = view.window_at(0).expect("the window exists");

    // Active, with no runtime window: the star is computed from the base and is
    // a value, so it survives and has a folder of its own.
    let active =
        RuntimeFormatContext::new(FormatContext::new().with_window(0, target, true, false));
    for flag in ["window_flags", "window_raw_flags"] {
        assert_eq!(
            active.format_value_by_name(flag).as_deref(),
            Some("*"),
            "{flag} keeps the star it computed"
        );
        assert_eq!(
            render_runtime_template(&format!("#{{{flag}}}"), &active, false),
            "*"
        );
        assert_eq!(
            render_runtime_template(&format!("#{{b:{flag}}}"), &active, false),
            "*"
        );
        assert_eq!(
            render_runtime_template(&format!("#{{d:{flag}}}"), &active, false),
            "."
        );
    }

    // The last window, again with no runtime window: the same, for the dash.
    let last = RuntimeFormatContext::new(FormatContext::new().with_window(0, target, false, true));
    for flag in ["window_flags", "window_raw_flags"] {
        assert_eq!(
            last.format_value_by_name(flag).as_deref(),
            Some("-"),
            "{flag} keeps the dash it computed"
        );
        assert_eq!(
            render_runtime_template(&format!("#{{{flag}}}"), &last, false),
            "-"
        );
        assert_eq!(
            render_runtime_template(&format!("#{{b:{flag}}}"), &last, false),
            "-"
        );
        assert_eq!(
            render_runtime_template(&format!("#{{d:{flag}}}"), &last, false),
            "."
        );
    }

    // Nothing to compute and no runtime window: the historical empty answer,
    // which is not a value and has no folder.
    let neither =
        RuntimeFormatContext::new(FormatContext::new().with_window(0, target, false, false));
    for flag in ["window_flags", "window_raw_flags"] {
        assert_eq!(neither.format_value_by_name(flag).as_deref(), Some(""));
        assert_eq!(
            render_runtime_template(&format!("#{{d:{flag}}}"), &neither, false),
            ""
        );
    }

    // Nothing to compute, but a real runtime window: the empty flags of a
    // window that is really there are a value, and a value has a folder.
    let real = RuntimeFormatContext::new(FormatContext::new().with_window(0, target, false, false))
        .with_state(&state)
        .with_session(view)
        .with_window(0, target);
    for flag in ["window_flags", "window_raw_flags"] {
        assert_eq!(real.format_value_by_name(flag).as_deref(), Some(""));
        assert_eq!(
            render_runtime_template(&format!("#{{d:{flag}}}"), &real, false),
            "."
        );
    }
}

/// An inventory that really holds no client is a complete answer that renders
/// empty - and an empty render is not a path.
///
/// tmux starts that value at NULL and writes it only when its buffer has a
/// size, so nobody on this window leaves nothing at all, and `format_find`
/// returns before `dirname` when there is no value (`format.c:797-831`,
/// `4070-4195`). This server keeps rendering the empty string publicly, as it
/// always has; what it must not do is hand that empty to a path modifier as
/// though a client name had been found.
#[test]
fn empty_client_name_lists_are_compatibility_values_for_path_modifiers() {
    let state = world();
    let view = state
        .sessions
        .session(&name("zulu"))
        .expect("the session exists");
    let target = view.window_at(1).expect("the window exists");
    let base = FormatContext::new().with_window(1, target, true, false);

    // Complete, and really holding no client: the public answer is the empty
    // string it always was, and no folder comes of it.
    let complete = RuntimeFormatContext::new(base.clone())
        .with_state(&state)
        .with_session(view)
        .with_window(1, target)
        .with_client_loop_snapshots(&[]);
    assert_eq!(
        complete
            .format_value_by_name("window_active_clients_list")
            .as_deref(),
        Some("")
    );
    assert_eq!(render_runtime_template(LIST, &complete, false), "");
    assert_eq!(
        render_runtime_template("#{b:window_active_clients_list}", &complete, false),
        ""
    );
    assert_eq!(
        render_runtime_template("#{d:window_active_clients_list}", &complete, false),
        ""
    );

    // A caller that supplies the name itself, even empty, is supplying a value:
    // it wins where it always won, and a value has a folder.
    let supplied = RuntimeFormatContext::new(
        base.clone()
            .with_named_value("window_active_clients_list", ""),
    )
    .with_state(&state)
    .with_session(view)
    .with_window(1, target)
    .with_client_loop_snapshots(&[]);
    assert_eq!(
        supplied
            .format_value_by_name("window_active_clients_list")
            .as_deref(),
        Some("")
    );
    assert_eq!(
        render_runtime_template("#{d:window_active_clients_list}", &supplied, false),
        "."
    );

    // A client that is really on this window keeps its exact name, and the
    // folder of a name holding no separator is the current one.
    let one = [client("cB-name", 1, "zulu")];
    let named = RuntimeFormatContext::new(base)
        .with_state(&state)
        .with_session(view)
        .with_window(1, target)
        .with_client_loop_snapshots(&one);
    assert_eq!(
        named
            .format_value_by_name("window_active_clients_list")
            .as_deref(),
        Some("cB-name")
    );
    assert_eq!(render_runtime_template(LIST, &named, false), "cB-name");
    assert_eq!(
        render_runtime_template("#{d:window_active_clients_list}", &named, false),
        "."
    );

    // The inputs never reached this runtime at all: its historical fallback is
    // untouched, and it is not a path either.
    let bare = RuntimeFormatContext::new(FormatContext::new().with_window(1, target, true, false));
    assert_eq!(
        bare.format_value_by_name("window_active_clients_list")
            .as_deref(),
        Some("")
    );
    assert_eq!(
        render_runtime_template("#{d:window_active_clients_list}", &bare, false),
        ""
    );
}
