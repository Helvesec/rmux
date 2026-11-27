//! A session group answers for the clients of ALL its members, by name.
//!
//! The expectations come from the primary text, never from the accessor under
//! test. tmux keeps ONE global client list and inserts each new client at its
//! tail (`server-client.c:327`), so "the clients of this group" is a walk of
//! that list keeping every client whose session belongs to the group, in the
//! order the server created them (`format.c:1093-1131`). The count is the same
//! set counted, as the sum of `s->attached` over the group's sessions
//! (`session.c:580-591`, read by `format.c:2577-2586`), and `many` is that count
//! above one (`format.c:2588-2600`). With no group at all the three answer
//! nothing.
//!
//! Measured once on a new tmux 3.7b built from those sources, LC_ALL=C, before
//! these witnesses were written: from a DETACHED member of a two-session group
//! holding two clients on its peer and one client on a session outside the
//! group, `#{session_attached}|#{session_group_attached}|`
//! `#{session_group_many_attached}|#{session_group_attached_list}` answered
//! `0|2|1|/dev/ttys019,/dev/ttys020`, then `0|1|0|/dev/ttys020` with one client
//! left, then `0|0|0|` with none, and `||` from the session outside the group
//! (`gatt1/cells/gatt1-oracle-a3/`).
//!
//! Scope of each witness, stated plainly. The five that a provider answering
//! this session's own attachment falsifies are the detached member, the two
//! members with a third looking on, the creation order, the excluded outsider
//! and the two-one-none walk. The rest are guards on what must NOT move: the
//! local `#{session_attached}`, the other `#{session_group_*}` variables, and a
//! runtime that was never handed the client list.

use rmux_core::formats::FormatContext;
use rmux_proto::{SessionName, TerminalSize};

use crate::client_activity::ClientActivityTimestamp;
use crate::client_format::{ClientFormatBindings, ClientLoopSnapshot};
use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::HandlerState;

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
/// What the oracle was read with, in the same order.
const GROUP: &str = "#{session_attached}|#{session_group_attached}\
|#{session_group_many_attached}|#{session_group_attached_list}";
const THREE: &str = "#{session_group_attached}|#{session_group_many_attached}\
|#{session_group_attached_list}";

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

/// One client the server has already created, on the session it is attached to.
fn client(name: &str, order: u64, session: &str) -> ClientLoopSnapshot {
    ClientLoopSnapshot {
        bindings: ClientFormatBindings {
            name: name.to_owned(),
            session: session.to_owned(),
            width: "80".to_owned(),
            height: "24".to_owned(),
            ..ClientFormatBindings::default()
        },
        order,
        activity_at: ClientActivityTimestamp::from_parts(0, 0),
    }
}

/// A store whose first named session is the group's template and whose other
/// named sessions join it; every `outside` session stays standalone.
fn world(members: &[&str], outside: &[&str]) -> HandlerState {
    let mut state = HandlerState::default();
    let mut names = members.iter();
    let template = session_name(names.next().expect("a group has a first member"));
    state
        .sessions
        .create_session(template.clone(), SIZE)
        .expect("the template session creates");
    for peer in names {
        state
            .sessions
            .create_grouped_session_with_base_index(session_name(peer), SIZE, 0, template.clone())
            .expect("the peer joins the group");
    }
    for alone in outside {
        state
            .sessions
            .create_session(session_name(alone), SIZE)
            .expect("the standalone session creates");
    }
    state
}

/// Reads `template` from `session`, with `attached` clients on that session
/// itself and `clients` known to the server.
fn read(
    state: &HandlerState,
    session: &str,
    attached: usize,
    clients: &[ClientLoopSnapshot],
    template: &str,
) -> String {
    let view = state
        .sessions
        .session(&session_name(session))
        .expect("the session exists");
    let runtime = RuntimeFormatContext::new(FormatContext::new().with_session_attached(attached))
        .with_state(state)
        .with_session(view)
        .with_client_loop_snapshots(clients);
    render_runtime_template(template, &runtime, false)
}

#[test]
fn a_detached_member_answers_for_the_clients_attached_to_its_peer() {
    let state = world(&["s0", "s1"], &[]);
    let clients = [
        client("/dev/ttys019", 1, "s0"),
        client("/dev/ttys020", 2, "s0"),
    ];
    // The measured oracle reading, from the member that has no client of its own.
    assert_eq!(
        read(&state, "s1", 0, &clients, GROUP),
        "0|2|1|/dev/ttys019,/dev/ttys020"
    );
}

#[test]
fn clients_spread_over_two_members_are_seen_by_the_third() {
    let state = world(&["s0", "s1", "s2"], &[]);
    let clients = [client("first", 1, "s0"), client("second", 2, "s1")];
    // Read from the one member nobody is attached to: the group's answer is the
    // whole group's, not this session's and not one peer's.
    assert_eq!(read(&state, "s2", 0, &clients, GROUP), "0|2|1|first,second");
    // And the same answer is given from either member that does hold a client,
    // whose own `#{session_attached}` stays its own.
    assert_eq!(read(&state, "s0", 1, &clients, GROUP), "1|2|1|first,second");
    assert_eq!(read(&state, "s1", 1, &clients, GROUP), "1|2|1|first,second");
}

#[test]
fn the_list_follows_the_order_the_server_created_the_clients() {
    let state = world(&["s0", "s1"], &[]);
    // `zulu` was created first and `alpha` second, on two different members:
    // the walk is the server's client list, not the alphabet and not the
    // sessions.
    let clients = [client("zulu", 1, "s0"), client("alpha", 2, "s1")];
    assert_eq!(read(&state, "s1", 1, &clients, GROUP), "1|2|1|zulu,alpha");
    // The same two clients created the other way round answer the other way
    // round, which is what says the order is read and not sorted.
    let reversed = [client("zulu", 2, "s0"), client("alpha", 1, "s1")];
    assert_eq!(read(&state, "s1", 1, &reversed, GROUP), "1|2|1|alpha,zulu");
}

#[test]
fn a_client_attached_outside_the_group_is_not_one_of_the_group_s() {
    let state = world(&["s0", "s1"], &["apart"]);
    let clients = [
        client("inside", 1, "s0"),
        client("elsewhere", 2, "apart"),
        client("unattached", 3, ""),
    ];
    assert_eq!(read(&state, "s1", 0, &clients, GROUP), "0|1|0|inside");
    // And the session outside the group answers nothing for the three, as tmux
    // does when `session_group_contains` finds no group.
    assert_eq!(read(&state, "apart", 1, &clients, THREE), "||");
}

#[test]
fn two_clients_then_one_then_none() {
    let state = world(&["s0", "s1"], &[]);
    let both = [client("first", 1, "s0"), client("second", 2, "s0")];
    assert_eq!(read(&state, "s1", 0, &both, GROUP), "0|2|1|first,second");
    assert_eq!(read(&state, "s1", 0, &both[1..], GROUP), "0|1|0|second");
    assert_eq!(read(&state, "s1", 0, &[], GROUP), "0|0|0|");
}

#[test]
fn the_other_group_variables_and_the_local_attachment_do_not_move() {
    let state = world(&["s0", "s1"], &["apart"]);
    let clients = [client("only", 1, "s0")];
    // `#{session_attached}` and `#{session_many_attached}` stay this session's
    // own, which is the whole reason the group needed its own reading.
    assert_eq!(
        read(
            &state,
            "s0",
            2,
            &clients,
            "#{session_attached}|#{session_many_attached}",
        ),
        "2|1"
    );
    assert_eq!(
        read(
            &state,
            "s1",
            0,
            &clients,
            "#{session_attached}|#{session_many_attached}",
        ),
        "0|0"
    );
    // The group's own identity, membership and size are untouched.
    assert_eq!(
        read(
            &state,
            "s1",
            0,
            &clients,
            "#{session_grouped}|#{session_group}|#{session_group_size}|#{session_group_list}",
        ),
        "1|s0|2|s0,s1"
    );
    assert_eq!(
        read(
            &state,
            "apart",
            1,
            &clients,
            "#{session_grouped}|#{session_group}|#{session_group_size}|#{session_group_list}",
        ),
        "0|||"
    );
}

#[test]
fn a_runtime_that_was_never_given_the_client_list_keeps_its_local_count_and_names_nobody() {
    let state = world(&["s0", "s1"], &[]);
    let view = state
        .sessions
        .session(&session_name("s0"))
        .expect("the session exists");
    let runtime = RuntimeFormatContext::new(FormatContext::new().with_session_attached(2))
        .with_state(&state)
        .with_session(view);
    // No caller handed this runtime the server's clients. The count is the one
    // it has, unchanged from before this lot; the list names nobody rather than
    // naming this session, which was never a client's name.
    assert_eq!(render_runtime_template(GROUP, &runtime, false), "2|2|1|");
}
