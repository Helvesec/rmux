//! `#{session_group_list}` names a group's sessions in the order it took them.
//!
//! The expectations come from the primary text, never from the accessor under
//! test. tmux walks the group's own TAILQ (`format.c:1075-1090`), each member is
//! appended at its tail when it joins (`session.c:543-548`), and creating a
//! group adds the TEMPLATE before the session that asked for it
//! (`cmd-new-session.c:316-325`). A rename touches none of that, and a member
//! removed and created again joins at the tail like any other. Each session
//! keeps its own current window through all of it (`tmux.1:1383-1387`,
//! `session.c:663-667`).
//!
//! Measured once on this lot's new tmux 3.7b image, LC_ALL=C, before these
//! witnesses were written (`sgord1/cells/om/ORACLE-MEASUREMENT.json`):
//! `zulu` then `alpha -t zulu` answered `zulu,alpha`; a third member answered
//! `zulu,alpha,mike` and every member read the same; renaming the middle one
//! answered `zulu,bravo,mike` with its current window still 0; killing that
//! middle member answered `zulu,mike` and creating it again `zulu,mike,bravo`;
//! and a session outside the group answered nothing at all.
//!
//! Scope of each witness, stated plainly. The four that a lexical order
//! falsifies are the join order, the third member, the rename and the
//! remove-then-recreate. The last two are guards on what must NOT move: a
//! session outside the group, and the shared helper that other readings still
//! sort by name.

use rmux_core::formats::FormatContext;
use rmux_proto::{SessionName, TerminalSize};

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::HandlerState;

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const LIST: &str = "#{session_group_list}";

fn name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

/// The first name is the template; every other name joins it, in this order.
fn world(names: &[&str]) -> HandlerState {
    let mut state = HandlerState::default();
    let mut walk = names.iter();
    let template = name(walk.next().expect("a group has a first member"));
    state
        .sessions
        .create_session(template.clone(), SIZE)
        .expect("the template session creates");
    for peer in walk {
        state
            .sessions
            .create_grouped_session_with_base_index(name(peer), SIZE, 0, template.clone())
            .expect("the peer joins the group");
    }
    state
}

fn join(state: &mut HandlerState, peer: &str, target: &str) {
    state
        .sessions
        .create_grouped_session_with_base_index(name(peer), SIZE, 0, name(target))
        .expect("the peer joins the group");
}

fn read(state: &HandlerState, session: &str, template: &str) -> String {
    let view = state
        .sessions
        .session(&name(session))
        .expect("the session exists");
    let runtime = RuntimeFormatContext::new(FormatContext::new())
        .with_state(state)
        .with_session(view);
    render_runtime_template(template, &runtime, false)
}

#[test]
fn the_group_lists_its_names_in_the_order_it_took_them() {
    let mut state = world(&["zulu", "alpha"]);
    // The group was created around `zulu`, which is why `zulu` is named first
    // although it sorts second.
    assert_eq!(read(&state, "alpha", LIST), "zulu,alpha");
    join(&mut state, "mike", "zulu");
    assert_eq!(read(&state, "mike", LIST), "zulu,alpha,mike");
    // Every member reads the same list: it is the group's order, not a view.
    for member in ["zulu", "alpha", "mike"] {
        assert_eq!(read(&state, member, LIST), "zulu,alpha,mike");
    }
}

#[test]
fn a_rename_leaves_a_member_exactly_where_it_was() {
    let mut state = world(&["zulu", "alpha"]);
    join(&mut state, "mike", "zulu");
    let before = state
        .sessions
        .session(&name("alpha"))
        .expect("alpha exists")
        .active_window_index();
    state
        .sessions
        .rename_session(&name("alpha"), name("bravo"))
        .expect("the middle member is renamed");
    assert_eq!(read(&state, "bravo", LIST), "zulu,bravo,mike");
    assert_eq!(
        state
            .sessions
            .session(&name("bravo"))
            .expect("bravo exists")
            .active_window_index(),
        before
    );
}

#[test]
fn a_member_removed_and_created_again_joins_at_the_tail() {
    let mut state = world(&["zulu", "alpha"]);
    join(&mut state, "mike", "zulu");
    let survivors = ["zulu", "mike"].map(|session| {
        state
            .sessions
            .session(&name(session))
            .expect("survivor exists")
            .id()
    });
    // The middle member goes, so the one the reader is on does not move.
    let removal = state
        .sessions
        .remove_session(&name("alpha"))
        .expect("the middle member is removed");
    assert!(removal.session_id.as_u32() > 0);
    assert_eq!(read(&state, "zulu", LIST), "zulu,mike");
    join(&mut state, "alpha", "zulu");
    assert_eq!(read(&state, "zulu", LIST), "zulu,mike,alpha");
    // The survivors kept the identities they had; only the newcomer is new.
    assert_eq!(
        ["zulu", "mike"].map(|session| state
            .sessions
            .session(&name(session))
            .expect("survivor exists")
            .id()),
        survivors
    );
}

#[test]
fn a_session_outside_the_group_receives_none_of_its_names() {
    let mut state = world(&["zulu", "alpha"]);
    state
        .sessions
        .create_session(name("outside"), SIZE)
        .expect("the standalone session creates");
    assert_eq!(read(&state, "outside", LIST), "");
    assert_eq!(
        read(
            &state,
            "outside",
            "#{session_grouped}|#{session_group}|#{session_group_size}"
        ),
        "0||"
    );
}

#[test]
fn the_shared_helper_and_the_other_group_variables_do_not_move() {
    let mut state = world(&["zulu", "alpha"]);
    join(&mut state, "mike", "zulu");
    // The shared membership helper other readings use is still sorted by name.
    assert_eq!(
        state.sessions.sessions_in_group(&name("zulu")),
        vec![name("alpha"), name("mike"), name("zulu")]
    );
    assert_eq!(state.sessions.session_group_size(&name("alpha")), 3);
    assert_eq!(
        read(
            &state,
            "alpha",
            "#{session_grouped}|#{session_group}|#{session_group_size}"
        ),
        "1|zulu|3"
    );
}
