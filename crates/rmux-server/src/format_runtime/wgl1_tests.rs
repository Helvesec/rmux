//! `#{window_linked_sessions_list}` names a window's links in the order they were made.
//!
//! The expectations come from the primary text, never from the accessor under
//! test. tmux walks the WINDOW's own winlinks and prints one session name per
//! winlink (`format.c:686-714`); each winlink is appended at the tail of that
//! list as it is made (`window.c:183-193`). So a window created by one session
//! and reached by a peer that joined its group afterwards names the creator
//! first, whichever link it is read through, and a rename leaves a member where
//! its link put it.
//!
//! Measured once on this lot's new tmux 3.7b image, LC_ALL=C, before these
//! witnesses were written (`wgl1/cells/om2/ORACLE-MEASUREMENT.json`):
//! with `zulu` holding two windows and `alpha` grouped onto it and NO explicit
//! link-window anywhere, window 0 answered `zulu,alpha` through the peer's
//! inactive link, through the creator's own link, and window 1 answered the
//! same; renaming the peer answered `zulu,bravo` through both links with its own
//! current window untouched; and a session outside the group named only itself.
//!
//! Scope of each witness, stated plainly. The two that the previous order
//! falsifies are the shared window read through either link, and the rename. The
//! other two are guards on what must NOT move: a window outside the group, and
//! the shared family list the runtime, resize, clipboard and navigation
//! aggregators read, which still answers the context first.
//!
//! A window created by `zulu`, linked explicitly into an outside session `E`, and
//! only then joined by `alpha`, which tmux names `zulu,E,alpha`, is held by
//! `handler_wgi1_tests`, through the real link-window path.

use rmux_core::formats::FormatContext;
use rmux_proto::{SessionName, TerminalSize};

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::HandlerState;

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const LIST: &str = "#{window_linked_sessions_list}";

fn name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

/// `zulu` holds windows 0 and 1; `alpha` is grouped onto it. Nothing is linked
/// explicitly, which is the shape this lot is about.
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
    state
        .sessions
        .create_grouped_session_with_base_index(name("alpha"), SIZE, 0, name("zulu"))
        .expect("the peer joins the group");
    state
}

fn read(state: &HandlerState, session: &str, window_index: u32, template: &str) -> String {
    let view = state
        .sessions
        .session(&name(session))
        .expect("the session exists");
    let window = view.window_at(window_index).expect("the window exists");
    let runtime = RuntimeFormatContext::new(FormatContext::new())
        .with_state(state)
        .with_session(view)
        .with_window(window_index, window);
    render_runtime_template(template, &runtime, false)
}

#[test]
fn a_shared_window_names_its_sessions_in_the_order_their_links_were_made() {
    let state = world();
    // Read through the peer's link - the one that is not its current window -
    // and through the creator's own: the window answers the same either way.
    assert_eq!(read(&state, "alpha", 0, LIST), "zulu,alpha");
    assert_eq!(read(&state, "zulu", 0, LIST), "zulu,alpha");
    // The other shared window answers the same, for the same reason.
    assert_eq!(read(&state, "alpha", 1, LIST), "zulu,alpha");
    assert_eq!(read(&state, "zulu", 1, LIST), "zulu,alpha");
}

#[test]
fn a_rename_leaves_a_member_where_its_link_put_it() {
    let mut state = world();
    state
        .sessions
        .rename_session(&name("alpha"), name("bravo"))
        .expect("the peer is renamed");
    assert_eq!(read(&state, "bravo", 0, LIST), "zulu,bravo");
    assert_eq!(read(&state, "zulu", 0, LIST), "zulu,bravo");
}

#[test]
fn a_window_of_a_session_outside_the_group_names_only_itself() {
    let mut state = world();
    state
        .sessions
        .create_session(name("outside"), SIZE)
        .expect("the standalone session");
    assert_eq!(read(&state, "outside", 0, LIST), "outside");
    // And the group's own window still names only its members.
    assert_eq!(read(&state, "alpha", 0, LIST), "zulu,alpha");
}

#[test]
fn the_shared_family_list_and_the_current_list_do_not_move() {
    let state = world();
    // The aggregators - runtime, resize, clipboard, navigation - read this one,
    // and it still answers the context session first.
    assert_eq!(
        state.window_linked_session_family_list(&name("alpha"), 0),
        vec![name("alpha"), name("zulu")]
    );
    assert_eq!(
        state.window_linked_session_family_list(&name("zulu"), 0),
        vec![name("zulu"), name("alpha")]
    );
    // The count that list feeds is untouched, and so is the sessions-where-this-
    // window-is-current list. Window 0 is the current one in BOTH members here -
    // neither has been sent anywhere else - so both are named, and this guard
    // holds that reading on either side of the correction.
    assert_eq!(state.window_linked_session_count(&name("alpha"), 0), 2);
    assert_eq!(
        state.window_linked_current_sessions_list(&name("zulu"), 0),
        vec![name("zulu"), name("alpha")]
    );
}

#[test]
fn the_runtime_accessor_keeps_its_own_order_while_the_format_names_the_links() {
    let state = world();
    // `window_linked_sessions_list` is not only the format's: `handler_window`
    // walks it to reconcile, emit and refresh the attached sessions in turn, and
    // the attach resize policy reads it too. Those callers have always been
    // given the asking session first, then its group peers by name, and that is
    // what they are still given.
    assert_eq!(
        state.window_linked_sessions_list(&name("alpha"), 0),
        vec![name("alpha"), name("zulu")]
    );
    assert_eq!(
        state.window_linked_sessions_list(&name("zulu"), 0),
        vec![name("zulu"), name("alpha")]
    );
    // What the format publishes is the WINDOW's links, in the order they were
    // made, whichever link it is read through. The two readings are different
    // questions, and only the second one moved.
    assert_eq!(read(&state, "alpha", 0, LIST), "zulu,alpha");
    assert_eq!(read(&state, "zulu", 0, LIST), "zulu,alpha");
}

// `#{window_linked}` asks the same window's winlinks a simpler question: is
// there more than one? tmux 3.7b walks every session's winlinks and answers 1
// at the second one that holds the window (`format.c:2871-2890`), and a group
// member holds its own winlink to every shared window. Measured on tmux 3.7b
// (S2350, private sockets, `new -t p -s g`, no link-window anywhere): every
// window of the group answered 1 through every member, a session outside the
// group answered 0, and once the peer was killed the remaining member answered
// 0 again.
const LINKED: &str = "#{window_linked}";

#[test]
fn a_window_shared_by_a_session_group_is_linked_through_every_member() {
    let state = world();
    assert_eq!(read(&state, "zulu", 0, LINKED), "1");
    assert_eq!(read(&state, "alpha", 0, LINKED), "1");
    assert_eq!(read(&state, "zulu", 1, LINKED), "1");
    assert_eq!(read(&state, "alpha", 1, LINKED), "1");
}

#[test]
fn a_window_held_by_one_session_only_is_not_linked() {
    let mut state = world();
    state
        .sessions
        .create_session(name("outside"), SIZE)
        .expect("the standalone session");
    assert_eq!(read(&state, "outside", 0, LINKED), "0");
    // A group reduced to one member shares nothing any more.
    let _removed = state
        .sessions
        .remove_session(&name("alpha"))
        .expect("the peer goes");
    assert_eq!(read(&state, "zulu", 0, LINKED), "0");
    assert_eq!(read(&state, "zulu", 1, LINKED), "0");
}

// `#{window_linked_sessions}` counts each session group holding the window once,
// and each session in no group once (`format.c`,
// `format_cb_window_linked_sessions`). Measured on tmux 3.7b (S2350 `linked.sh`,
// private sockets): a group of two answered 1 and a group of three answered 1,
// through every member and every shared window.
const SESSIONS: &str = "#{window_linked_sessions}";

#[test]
fn a_session_group_counts_once_however_many_members_it_has() {
    let mut state = world();
    for (session, index) in [("zulu", 0), ("alpha", 0), ("zulu", 1), ("alpha", 1)] {
        assert_eq!(
            read(&state, session, index, SESSIONS),
            "1",
            "{session}:{index}"
        );
    }
    state
        .sessions
        .create_grouped_session_with_base_index(name("mike"), SIZE, 0, name("zulu"))
        .expect("a third member joins");
    assert_eq!(read(&state, "mike", 0, SESSIONS), "1");
    assert_eq!(read(&state, "zulu", 1, SESSIONS), "1");
    // The kill paths still count the members.
    assert_eq!(state.window_linked_session_count(&name("zulu"), 0), 3);
}

#[test]
fn a_window_made_by_a_peer_names_that_peer_first() {
    // tmux 3.7b, S2350 `linked.sh` case 4 (`new -s p; new -t p -s g;
    // neww -t g:3`): both windows answered `g,p`. The new winlink is appended,
    // then the group is synchronised from `g`, which recreates `p`'s winlinks
    // at the tail of every window's list.
    let mut state = world();
    let opened = state
        .sessions
        .insert_window_with_initial_pane(&name("alpha"), 3, SIZE)
        .expect("the peer opens a window");
    assert_eq!(opened.window_index, 3);
    for (session, index) in [("alpha", 3), ("zulu", 3), ("alpha", 0), ("zulu", 1)] {
        assert_eq!(
            read(&state, session, index, LIST),
            "alpha,zulu",
            "{session}:{index}"
        );
    }
}

const ACTIVE: &str = "#{window_active_sessions_list}";

#[test]
fn the_active_list_follows_the_link_order_after_a_partial_detach() {
    // tmux 3.7b, S2365 `linked2.sh` case 6 (`link-window -s p:0 -t p:5;
    // unlink-window -t g:5` in a group): every window answers `g,p`, and the
    // window both members are on names them in that order too.
    let mut state = world();
    let shared = state
        .sessions
        .session(&name("zulu"))
        .and_then(|view| view.window_at(0).map(|window| window.id()))
        .expect("the shared window");
    state
        .sessions
        .link_window(
            &rmux_proto::WindowTarget::with_window(name("zulu"), 5),
            shared,
        )
        .expect("the self-link");
    let _ = state
        .sessions
        .remove_window(&rmux_proto::WindowTarget::with_window(name("alpha"), 5))
        .expect("the peer unlinks it");
    for session in ["zulu", "alpha"] {
        assert_eq!(read(&state, session, 0, LIST), "alpha,zulu", "{session}");
        assert_eq!(read(&state, session, 0, ACTIVE), "alpha,zulu", "{session}");
    }
}

#[test]
fn the_active_list_follows_the_list_when_the_order_is_unknown() {
    // tmux 3.7b, S2365 `linked2.sh` case 8 (`neww -t p:3; new -t p -s g;
    // move-window -r -t g`): the list and the active list both answer `p,g`,
    // from either member. The renumber leaves this store without an order, so
    // both fall back - and the active list must not start from the reader.
    let mut state = HandlerState::default();
    state
        .sessions
        .create_session(name("zulu"), SIZE)
        .expect("the creating session");
    let _ = state
        .sessions
        .insert_window_with_initial_pane(&name("zulu"), 3, SIZE)
        .expect("its second window");
    state
        .sessions
        .create_grouped_session_with_base_index(name("alpha"), SIZE, 0, name("zulu"))
        .expect("the peer joins the group");
    let _ = state
        .sessions
        .reindex_windows(&name("alpha"))
        .expect("the peer renumbers");
    for session in ["zulu", "alpha"] {
        assert_eq!(read(&state, session, 0, LIST), "zulu,alpha", "{session}");
        assert_eq!(read(&state, session, 0, ACTIVE), "zulu,alpha", "{session}");
    }
}
