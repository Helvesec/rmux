//! A `%PaneId` names one pane, whatever number of sessions link its window.
//!
//! The expectations come from the primary text, never from the accessor under
//! test. tmux resolves a BARE `%id` by finding the pane in the one global pane
//! tree and then choosing among the sessions that hold its window: the most
//! recently active of them, the first by name on a tie (`cmd-find.c:498-512`
//! calling `cmd_find_best_session_with_window` at 176-197, whose comparator
//! `cmd_find_session_better` ends on `timercmp(&s->activity_time, ...)`). It
//! consults no current context on that path - there is no context branch in
//! `cmd_find_get_pane` at all. An explicit session stays that session
//! (`cmd-find.c:536-548`), and inside the chosen session the link is the active
//! one if it holds the window, otherwise the first index
//! (`cmd_find_best_winlink_with_window`, 205-231).
//!
//! Measured once on this lot's new tmux 3.7b image, LC_ALL=C, before these
//! witnesses were written (`gatt1/r2/cells/om/ORACLE-MEASUREMENT.json`):
//! with the same window linked at `g0:0`, `g0:4` and `g1:0`, a bare `%id`
//! answered `g1|0|0` with nobody attached; `g0|4|0` and then `g0|0|0` for
//! `g0:.%id` as `g0`'s active window held the window and then did not; `g0|0|0`
//! once a client made `g0` the most recently active; `g1|0|0` again once `g1`
//! was, WITH that client still sitting on `g0`; and `g1|0|0` still after a
//! newer session that holds no link to the window.
//!
//! Scope of each witness, stated plainly. The four that a resolver answering
//! "ambiguous" falsifies are the shared pane, the context that does not win, the
//! explicit session and its link order. The last two are guards on what must NOT
//! move: an absent id and a genuinely ambiguous name are still refused.

use super::{
    TargetFindContext, TargetFindFlags as Flags, TargetFindType as Kind, UnresolvedTarget,
};
use crate::ManagedSessionStore;
use rmux_proto::{PaneTarget, RmuxError, SessionName, Target, TerminalSize, WindowTarget};

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };

fn name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn window(session: &str, index: u32) -> WindowTarget {
    WindowTarget::with_window(name(session), index)
}

fn pane(session: &str, index: u32, pane_index: u32) -> Target {
    Target::Pane(PaneTarget::with_window(name(session), index, pane_index))
}

/// `g0` and its grouped peer `g1` share one window; `out` is created last, so it
/// is the most recently active session and it holds no link to that window.
fn world() -> ManagedSessionStore {
    let mut store = ManagedSessionStore::new();
    store.create_session(name("g0"), SIZE).expect("g0 creates");
    store
        .create_grouped_session_with_base_index(name("g1"), SIZE, 0, name("g0"))
        .expect("g1 joins g0's group");
    store
        .create_session(name("out"), SIZE)
        .expect("out creates");
    store
}

fn shared(store: &ManagedSessionStore) -> String {
    format!(
        "%{}",
        store
            .window(&window("g0", 0))
            .expect("the shared window")
            .panes()[0]
            .id()
            .as_u32()
    )
}

fn resolve(
    store: &ManagedSessionStore,
    raw: &str,
    context: &TargetFindContext,
) -> Result<Target, RmuxError> {
    store.resolve_unresolved_target(
        &UnresolvedTarget::new(raw),
        Kind::Pane,
        Flags::NONE,
        context,
    )
}

fn nowhere() -> TargetFindContext {
    TargetFindContext::new(None)
}

#[test]
fn a_shared_pane_id_answers_the_most_recently_active_session_that_holds_it() {
    let mut store = world();
    let id = shared(&store);
    // `g1` was created after `g0`, so of the two sessions holding the window it
    // is the more recently active. `out` is more recent than both and holds no
    // link to it, which is why it never appears.
    assert_eq!(resolve(&store, &id, &nowhere()).unwrap(), pane("g1", 0, 0));
    store.touch_attached(&name("g0")).expect("g0 is used");
    assert_eq!(resolve(&store, &id, &nowhere()).unwrap(), pane("g0", 0, 0));
    store.touch_attached(&name("g1")).expect("g1 is used");
    assert_eq!(resolve(&store, &id, &nowhere()).unwrap(), pane("g1", 0, 0));
    store.touch_attached(&name("out")).expect("out is used");
    assert_eq!(resolve(&store, &id, &nowhere()).unwrap(), pane("g1", 0, 0));
}

#[test]
fn the_current_context_gives_a_shared_pane_id_no_priority_of_its_own() {
    let mut store = world();
    store.touch_attached(&name("g0")).expect("g0 is used");
    store.touch_attached(&name("g1")).expect("g1 is used");
    let id = shared(&store);
    // Measured: with the only client sitting on `g0` and `g1` the more recently
    // active, tmux answered `g1`. Being on a pane does not make that pane's
    // session the answer for its own id.
    for context in [
        nowhere(),
        TargetFindContext::from_target(pane("g0", 0, 0)),
        TargetFindContext::from_target(pane("g1", 0, 0)),
    ] {
        assert_eq!(resolve(&store, &id, &context).unwrap(), pane("g1", 0, 0));
    }
    // The other way round, the same: the answer follows the recency, not the
    // context that asked.
    store.touch_attached(&name("g0")).expect("g0 is used again");
    for context in [nowhere(), TargetFindContext::from_target(pane("g1", 0, 0))] {
        assert_eq!(resolve(&store, &id, &context).unwrap(), pane("g0", 0, 0));
    }
}

#[test]
fn an_explicit_session_keeps_a_shared_pane_id_inside_that_session() {
    let mut store = world();
    store.touch_attached(&name("g1")).expect("g1 is used");
    let id = shared(&store);
    // `g1` is the most recently active, and is still not where `g0:.%id` looks.
    assert_eq!(
        resolve(&store, &format!("g0:.{id}"), &nowhere()).unwrap(),
        pane("g0", 0, 0)
    );
    assert_eq!(
        resolve(&store, &format!("g1:.{id}"), &nowhere()).unwrap(),
        pane("g1", 0, 0)
    );
    // And a session that does not hold the window has no such pane.
    assert!(resolve(&store, &format!("out:.{id}"), &nowhere()).is_err());
}

#[test]
fn inside_a_session_the_active_link_is_taken_before_the_first_index() {
    let mut store = world();
    let id = shared(&store);
    let window_id = store
        .window(&window("g0", 0))
        .expect("the shared window")
        .id();
    store
        .link_window(&window("g0", 4), window_id)
        .expect("the same window, linked again in g0");
    // g0's active window is still 0, and 0 holds the window.
    assert_eq!(
        resolve(&store, &format!("g0:.{id}"), &nowhere()).unwrap(),
        pane("g0", 0, 0)
    );
    store
        .select_window(&window("g0", 4))
        .expect("g0 looks at its other alias");
    assert_eq!(
        resolve(&store, &format!("g0:.{id}"), &nowhere()).unwrap(),
        pane("g0", 4, 0)
    );
    // A window of its own, which does not hold it: the answer falls back to the
    // FIRST index that does, not to the active one and not to the last.
    let own = store
        .insert_window_with_initial_pane(&name("g0"), 7, SIZE)
        .expect("a window of its own");
    assert_ne!(
        own.window_id, window_id,
        "that window is not the shared one"
    );
    store
        .select_window(&window("g0", 7))
        .expect("g0 looks at that one");
    assert_eq!(
        resolve(&store, &format!("g0:.{id}"), &nowhere()).unwrap(),
        pane("g0", 0, 0)
    );
    // The bare id is unaffected by which window g0 happens to be looking at.
    assert_eq!(resolve(&store, &id, &nowhere()).unwrap(), pane("g1", 0, 0));
}

#[test]
fn a_pane_id_that_exists_nowhere_is_still_refused() {
    let store = world();
    assert!(resolve(&store, "%987", &nowhere()).is_err());
    assert!(resolve(&store, "g0:.%987", &nowhere()).is_err());
    assert!(resolve(
        &store,
        "%987",
        &TargetFindContext::from_target(pane("g0", 0, 0))
    )
    .is_err());
}

#[test]
fn a_name_that_really_is_ambiguous_is_still_refused() {
    let store = world();
    // `g` is a prefix of both `g0` and `g1`: nothing about the pane-id path
    // relaxes the rule that a textual name matching several things refuses.
    let error = store
        .resolve_unresolved_target(
            &UnresolvedTarget::new("g"),
            Kind::Session,
            Flags::NONE,
            &nowhere(),
        )
        .expect_err("an ambiguous session prefix refuses");
    assert_eq!(
        error,
        RmuxError::invalid_target("g", "ambiguous session match")
    );
}
