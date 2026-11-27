//! `+N` and `-N` offsets wrap with whole-number arithmetic, whatever `N`.
//!
//! tmux steps through the winlinks (`winlink_next_by_number`) or the panes
//! one by one, wrapping at the end, so `+N` lands on `(position + N) mod len`
//! for any `N` up to `INT_MAX`. Measured on tmux 3.7b with windows `0 1 2`
//! (current `2`) and panes `0 1 2` (current `2`): `a:+2147483647` is window
//! `0`, `a:-2147483647` window `1`, `a:2.+2147483647` pane `0`,
//! `a:2.-2147483647` pane `1`, and `kill-window -t a:+2147483647` leaves `1 2`.
//! The position plus the offset must therefore never be summed in `i32`.
//!
//! A window-index target (`new-window -t a:+N`) is the current index plus `N`,
//! and tmux indices are `int`: from window `2`, `a:+2147483645` creates window
//! `2147483647` while `a:+2147483646` and `a:+2147483647` answer
//! `can't find window`, and `a:-3` likewise finds nothing.

use super::{TargetFindContext, TargetFindFlags, TargetFindType, UnresolvedTarget};
use crate::SessionStore;
use rmux_proto::{PaneTarget, RmuxError, SessionName, Target, TerminalSize, WindowTarget};

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };

fn name() -> SessionName {
    SessionName::new("a").expect("valid session name")
}

/// Windows `0 1 2` with `2` current; window `2` holds panes `0 1 2` with `2` active.
fn last_window_and_pane_current() -> SessionStore {
    let mut store = SessionStore::new();
    store
        .create_session(name(), SIZE)
        .expect("session create succeeds");
    let session = store.session_mut(&name()).expect("session exists");
    for index in [1, 2] {
        session
            .insert_window_with_initial_pane(index, SIZE)
            .expect("window insert succeeds");
    }
    session.select_window(2).expect("select window 2 succeeds");
    session.split_active_pane().expect("first split succeeds");
    session.split_active_pane().expect("second split succeeds");
    session.select_pane(2).expect("select pane 2 succeeds");
    store
}

fn resolve_with(
    store: &SessionStore,
    raw: &str,
    find_type: TargetFindType,
    flags: TargetFindFlags,
) -> Result<Target, RmuxError> {
    store.resolve_unresolved_target(
        &UnresolvedTarget::new(raw),
        find_type,
        flags,
        &TargetFindContext::from_target(Target::Pane(PaneTarget::with_window(name(), 2, 2))),
    )
}

fn resolve(store: &SessionStore, raw: &str, find_type: TargetFindType) -> Target {
    resolve_with(store, raw, find_type, TargetFindFlags::NONE)
        .unwrap_or_else(|error| panic!("{raw} resolves: {error:?}"))
}

fn resolve_index(store: &SessionStore, raw: &str) -> Result<Target, RmuxError> {
    resolve_with(
        store,
        raw,
        TargetFindType::Window,
        TargetFindFlags::WINDOW_INDEX,
    )
}

fn window(index: u32) -> Target {
    Target::Window(WindowTarget::with_window(name(), index))
}

fn pane(index: u32) -> Target {
    Target::Pane(PaneTarget::with_window(name(), 2, index))
}

#[test]
fn window_offsets_at_the_i32_bounds_wrap_like_tmux() {
    let store = last_window_and_pane_current();

    assert_eq!(
        resolve(&store, "a:+2147483647", TargetFindType::Window),
        window(0)
    );
    assert_eq!(
        resolve(&store, "a:-2147483647", TargetFindType::Window),
        window(1)
    );
    assert_eq!(resolve(&store, "a:+4", TargetFindType::Window), window(0));
    assert_eq!(resolve(&store, "a:-4", TargetFindType::Window), window(1));
}

#[test]
fn pane_offsets_at_the_i32_bounds_wrap_like_tmux() {
    let store = last_window_and_pane_current();

    assert_eq!(
        resolve(&store, "a:2.+2147483647", TargetFindType::Pane),
        pane(0)
    );
    assert_eq!(
        resolve(&store, "a:2.-2147483647", TargetFindType::Pane),
        pane(1)
    );
    assert_eq!(resolve(&store, "a:2.+4", TargetFindType::Pane), pane(0));
}

#[test]
fn window_index_offsets_stop_at_the_tmux_int_bound() {
    let store = last_window_and_pane_current();

    assert_eq!(
        resolve_index(&store, "a:+2147483645").expect("index i32::MAX is a window index"),
        window(2_147_483_647)
    );
    for raw in ["a:+2147483646", "a:+2147483647", "a:-3"] {
        let value = raw.trim_start_matches("a:");
        assert_eq!(
            resolve_index(&store, raw),
            Err(RmuxError::invalid_target(
                value,
                format!("can't find window: {value}")
            )),
            "{raw}"
        );
    }
}
