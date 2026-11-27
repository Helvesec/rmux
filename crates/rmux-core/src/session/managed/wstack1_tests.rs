//! The windows a session really visited.
//!
//! tmux keeps a per-session stack of the links it has left, most recent first:
//! selecting a window takes it out of that stack, pushes the window being left
//! onto the head and only then moves (`session.c:475-487`, `window.c:244-252`).
//! Selecting the window that is already current returns early and changes
//! nothing, and a window created detached is never pushed, so a window that was
//! never visited is not in the stack at all. Unlinking a window drops its entry
//! (`session.c:350`), which is why no dead or reused index can come back.
//!
//! Those readings were taken on the pinned tmux 3.7b, and the sequence below is
//! the one this parcours measured there: 0 / 3,0 / 1,3,0 / 2,1,3,0 / 1,2,3,0.

use super::*;
use rmux_proto::{SessionName, TerminalSize, WindowTarget};

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn target(name: &SessionName, index: u32) -> WindowTarget {
    WindowTarget::with_window(name.clone(), index)
}

/// A session with window 0 active and windows 1, 2 and 3 created detached, so
/// none of them has ever been visited.
fn four_windows(store: &mut ManagedSessionStore, name: &str) -> SessionName {
    let name = session_name(name);
    store
        .create_session(name.clone(), SIZE)
        .expect("the fixture creates its session");
    for _ in 0..3 {
        let _created = store
            .create_window(&name, SIZE)
            .expect("the fixture creates a detached window");
    }
    name
}

fn stack(store: &ManagedSessionStore, name: &SessionName) -> Vec<u32> {
    store
        .session(name)
        .expect("the session exists")
        .window_visit_stack()
        .to_vec()
}

fn select(store: &mut ManagedSessionStore, name: &SessionName, index: u32) {
    store
        .select_window(&target(name, index))
        .expect("the fixture selects its window");
}

#[test]
fn a_window_created_detached_was_never_visited() {
    let mut store = ManagedSessionStore::new();
    let name = four_windows(&mut store, "stack");

    assert_eq!(store.session(&name).unwrap().active_window_index(), 0);
    assert!(
        stack(&store, &name).is_empty(),
        "three windows exist and none of them has been visited"
    );
}

#[test]
fn the_history_follows_the_real_visits_and_the_return() {
    let mut store = ManagedSessionStore::new();
    let name = four_windows(&mut store, "stack");

    select(&mut store, &name, 3);
    assert_eq!(stack(&store, &name), [0]);
    select(&mut store, &name, 1);
    assert_eq!(stack(&store, &name), [3, 0]);
    select(&mut store, &name, 2);
    assert_eq!(stack(&store, &name), [1, 3, 0]);

    // Selecting the window that is already current changes nothing at all.
    select(&mut store, &name, 2);
    assert_eq!(stack(&store, &name), [1, 3, 0]);

    // The return goes to the head of the stack and pushes what it leaves.
    let back = store.last_window(&name).expect("a window to go back to");
    assert_eq!(back, 1);
    assert_eq!(stack(&store, &name), [2, 3, 0]);
    assert_eq!(store.session(&name).unwrap().active_window_index(), 1);
}

#[test]
fn each_session_keeps_its_own_history() {
    let mut store = ManagedSessionStore::new();
    let first = four_windows(&mut store, "stack");
    let second = four_windows(&mut store, "other");

    select(&mut store, &first, 3);
    select(&mut store, &first, 1);

    assert_eq!(stack(&store, &first), [3, 0]);
    assert!(
        stack(&store, &second).is_empty(),
        "the other session visited nothing of its own"
    );
}

#[test]
fn removing_a_window_drops_it_from_the_history() {
    let mut store = ManagedSessionStore::new();
    let name = four_windows(&mut store, "stack");
    select(&mut store, &name, 3);
    select(&mut store, &name, 1);
    select(&mut store, &name, 2);
    assert_eq!(stack(&store, &name), [1, 3, 0]);

    let _removed = store
        .remove_window(&target(&name, 3))
        .expect("the fixture removes the older occurrence");

    assert_eq!(
        stack(&store, &name),
        [1, 0],
        "the occurrence that is gone is not remembered"
    );
}

#[test]
fn a_rollback_restores_the_history_it_captured() {
    let mut store = ManagedSessionStore::new();
    let name = four_windows(&mut store, "stack");
    select(&mut store, &name, 3);
    select(&mut store, &name, 1);
    assert_eq!(stack(&store, &name), [3, 0]);

    let checkpoint = store
        .checkpoint_window(&target(&name, 1))
        .expect("the fixture checkpoints its window");
    select(&mut store, &name, 2);
    assert_eq!(stack(&store, &name), [1, 3, 0]);

    store
        .restore_window_checkpoint(&target(&name, 1), checkpoint)
        .expect("the fixture rolls back");
    assert_eq!(
        stack(&store, &name),
        [3, 0],
        "the rollback brings back the history it captured"
    );
}

// The mutations the parcours has to survive. Each expectation below was measured
// on the pinned tmux 3.7b before this block existed: after the visits 3, 1, 2 a
// renumber keeps every visit on the occurrence that moved, killing the window the
// return would take leaves the return on the surviving head, moving a window is a
// link plus an unlink so its visit goes and nothing is invented at the new slot,
// a swap leaves the history with the slots, and a new peer of a group has visited
// nothing of its own.

#[test]
fn a_renumber_carries_every_visit_to_the_index_it_moved_to() {
    let mut store = ManagedSessionStore::new();
    let name = four_windows(&mut store, "stack");
    for index in [3, 1, 2] {
        select(&mut store, &name, index);
    }
    assert_eq!(stack(&store, &name), [1, 3, 0]);

    let _change = store
        .reindex_windows_from(&name, 10)
        .expect("the fixture renumbers its windows");

    assert_eq!(
        store.session(&name).unwrap().active_window_index(),
        12,
        "the active occurrence moved with the renumber"
    );
    assert_eq!(
        stack(&store, &name),
        [11, 13, 10],
        "every visit followed the occurrence it names"
    );
}

#[test]
fn the_return_takes_the_surviving_head_after_a_kill() {
    let mut store = ManagedSessionStore::new();
    let name = four_windows(&mut store, "stack");
    for index in [3, 1, 2] {
        select(&mut store, &name, index);
    }
    assert_eq!(stack(&store, &name), [1, 3, 0]);

    // 1 is the window the return would take: killing it must not leave the
    // session without one while the history still names a live window.
    let _removed = store
        .remove_window(&target(&name, 1))
        .expect("the fixture kills the previous window");
    assert_eq!(stack(&store, &name), [3, 0]);

    let back = store.last_window(&name).expect("a window to go back to");
    assert_eq!(back, 3, "the return took the surviving head");
    assert_eq!(stack(&store, &name), [2, 0]);
}

#[test]
fn a_move_is_a_link_and_an_unlink_not_a_renumber() {
    let mut store = ManagedSessionStore::new();
    let name = four_windows(&mut store, "stack");
    for index in [3, 1, 2] {
        select(&mut store, &name, index);
    }

    // `move-window -d -s 3 -t 7` unlinks 3 and links its window at 7
    // (`cmd-move-window.c:103-109`, `server-fn.c:278-298`, `session.c:350`), so
    // the visit of 3 goes and 7 arrives with none of its own.
    let _moved = store
        .move_window(&name, 3, 7, false, false)
        .expect("the fixture moves its window");

    assert_eq!(stack(&store, &name), [1, 0]);
}

#[test]
fn a_swap_leaves_the_history_with_the_slots() {
    let mut store = ManagedSessionStore::new();
    let name = four_windows(&mut store, "stack");
    for index in [3, 1, 2] {
        select(&mut store, &name, index);
    }
    assert_eq!(stack(&store, &name), [1, 3, 0]);

    // A swap keeps both winlinks and their slots and exchanges the windows they
    // hold (`cmd-swap-window.c:75-85`), so no visit moves with a window.
    let _swapped = store
        .swap_windows(&name, 0, 3)
        .expect("the fixture swaps two windows");

    assert_eq!(stack(&store, &name), [1, 3, 0]);
}

#[test]
fn a_new_group_peer_has_visited_nothing_of_its_own() {
    let mut store = ManagedSessionStore::new();
    let name = four_windows(&mut store, "stack");
    for index in [3, 1, 2] {
        select(&mut store, &name, index);
    }
    assert_eq!(stack(&store, &name), [1, 3, 0]);

    let peer = session_name("peer");
    let _grouped = store
        .create_grouped_session_with_base_index(peer.clone(), SIZE, 0, name.clone())
        .expect("the fixture groups a new session on this one");

    assert!(
        stack(&store, &peer).is_empty(),
        "a new peer of the group has visited none of these links"
    );
    assert_eq!(
        stack(&store, &name),
        [1, 3, 0],
        "and the session it joined keeps its own"
    );
}

#[test]
fn a_standalone_session_forgets_and_remaps_too() {
    let mut session = crate::Session::new(session_name("solo"), SIZE);
    for _ in 0..3 {
        let _created = session
            .create_window(SIZE)
            .expect("the fixture creates a detached window");
    }
    for index in [3, 1, 2] {
        session
            .select_window(index)
            .expect("the fixture selects its window");
    }
    assert_eq!(session.window_visit_stack(), [1, 3, 0]);

    let _removed = session
        .remove_window(3)
        .expect("the fixture removes an older occurrence");
    assert_eq!(
        session.window_visit_stack(),
        [1, 0],
        "the occurrence that is gone is not remembered"
    );

    let _map = session
        .reindex_windows_from(10)
        .expect("the fixture renumbers its windows");
    assert_eq!(
        session.window_visit_stack(),
        [11, 10],
        "the visits followed the occurrences they name"
    );
}

// The callers R2 left wrong. Each expectation is the primary's: a swap keeps the
// visits on the slots for the peers as for the source, a move omits the links it
// destroys, an insertion shifts the same links, and the return always takes the
// live head of what the session really visited.

/// zeta and its peer both hold the same four links; only zeta has visited any.
fn grouped_pair(store: &mut ManagedSessionStore) -> (SessionName, SessionName) {
    let source = four_windows(store, "src");
    for index in [3, 1, 2] {
        select(store, &source, index);
    }
    let peer = session_name("peer");
    let _grouped = store
        .create_grouped_session_with_base_index(peer.clone(), SIZE, 0, source.clone())
        .expect("the fixture groups a peer on it");
    for index in [3, 1, 2] {
        select(store, &peer, index);
    }
    (source, peer)
}

#[test]
fn a_swap_leaves_a_peer_s_visits_on_their_slots() {
    let mut store = ManagedSessionStore::new();
    let (source, peer) = grouped_pair(&mut store);
    // The peer starts on the window the source was on, so its own history is what
    // it visited from there; this witness is about what a swap does to it, not
    // about where a peer starts.
    let before_peer = stack(&store, &peer);
    let before_source = stack(&store, &source);
    assert_eq!(before_source, [1, 3, 0]);

    let _swapped = store
        .swap_windows(&source, 0, 3)
        .expect("the fixture swaps two windows");

    assert_eq!(
        stack(&store, &peer),
        before_peer,
        "the windows exchanged, the slots and their visits did not"
    );
    assert_eq!(stack(&store, &source), before_source);
}

#[test]
fn a_move_does_not_carry_a_peer_s_visit_onto_the_new_link() {
    let mut store = ManagedSessionStore::new();
    let (source, peer) = grouped_pair(&mut store);
    let before_peer = stack(&store, &peer);
    assert!(before_peer.contains(&3), "the peer really visited 3");

    let _moved = store
        .move_window(&source, 3, 7, false, false)
        .expect("the fixture moves a window");

    let after_peer = stack(&store, &peer);
    assert!(
        !after_peer.contains(&3),
        "the peer's visit of the link that left is gone"
    );
    assert!(
        !after_peer.contains(&7),
        "and the link that appeared has no visit of its own"
    );
    assert_eq!(
        after_peer,
        before_peer
            .iter()
            .copied()
            .filter(|visited| *visited != 3)
            .collect::<Vec<_>>(),
        "nothing else in the peer's history moved"
    );
}

#[test]
fn an_insertion_shifts_the_visits_with_the_links_it_moves() {
    let mut store = ManagedSessionStore::new();
    let name = four_windows(&mut store, "stack");
    for index in [1, 2, 3] {
        select(&mut store, &name, index);
    }
    select(&mut store, &name, 0);
    assert_eq!(stack(&store, &name), [3, 2, 1]);

    // Opening room at 3 shifts that link to 4: the same winlink keeps its window
    // and only changes index (`window.c:1758-1788`).
    let _room = store
        .make_room_for_window(&name, 3)
        .expect("the fixture opens room at 3");

    assert_eq!(stack(&store, &name), [4, 2, 1]);
}

#[test]
fn a_replaced_previous_window_leaves_the_return_on_the_live_head() {
    let mut store = ManagedSessionStore::new();
    let name = four_windows(&mut store, "stack");
    for index in [3, 1, 2] {
        select(&mut store, &name, index);
    }
    assert_eq!(stack(&store, &name), [1, 3, 0]);
    assert_eq!(store.session(&name).unwrap().last_window_index(), Some(1));

    // Linking another window over slot 1 destroys the occurrence the return
    // pointed at, without selecting anything.
    let other = store
        .session(&name)
        .unwrap()
        .window_at(0)
        .expect("window 0")
        .id();
    let _installed = store
        .install_window_alias(&target(&name, 1), other, true, false)
        .expect("the fixture replaces the previous window");

    assert_eq!(stack(&store, &name), [3, 0]);
    assert_eq!(
        store.session(&name).unwrap().last_window_index(),
        Some(3),
        "the return takes the live head instead of a window that is gone"
    );
}

#[test]
fn a_moved_previous_window_leaves_the_return_on_the_live_head() {
    let mut store = ManagedSessionStore::new();
    let name = four_windows(&mut store, "stack");
    for index in [3, 1, 2] {
        select(&mut store, &name, index);
    }

    let _moved = store
        .move_window(&name, 1, 8, false, false)
        .expect("the fixture moves the previous window away");

    assert_eq!(stack(&store, &name), [3, 0]);
    assert_eq!(
        store.session(&name).unwrap().last_window_index(),
        Some(3),
        "the return follows the history, not the slot that left"
    );
}

#[test]
fn a_new_group_peer_has_no_window_to_return_to() {
    let mut store = ManagedSessionStore::new();
    let name = four_windows(&mut store, "stack");
    for index in [3, 1, 2] {
        select(&mut store, &name, index);
    }

    let peer = session_name("fresh");
    let _grouped = store
        .create_grouped_session_with_base_index(peer.clone(), SIZE, 0, name.clone())
        .expect("the fixture groups a new session");

    assert!(stack(&store, &peer).is_empty());
    assert_eq!(
        store.session(&peer).unwrap().last_window_index(),
        None,
        "with no visit of its own there is nothing to return to"
    );
}

#[test]
fn a_standalone_move_forgets_the_link_it_moves_and_the_one_it_replaces() {
    let mut session = crate::Session::new(session_name("solo"), SIZE);
    for _ in 0..3 {
        let _created = session
            .create_window(SIZE)
            .expect("the fixture creates a detached window");
    }
    for index in [3, 1, 2] {
        session
            .select_window(index)
            .expect("the fixture selects its window");
    }
    assert_eq!(session.window_visit_stack(), [1, 3, 0]);

    let _removed = session
        .move_window(3, 7, false, false)
        .expect("the fixture moves a window");
    assert_eq!(
        session.window_visit_stack(),
        [1, 0],
        "the visit left with the link and 7 arrives unvisited"
    );

    // A window created back at 3 inherits nothing of what used to be there.
    let _created = session
        .create_window_at_or_above(SIZE, 3)
        .expect("the fixture recreates a window at the freed index");
    assert_eq!(session.window_visit_stack(), [1, 0]);
}

// The cases R4 measured on the oracle before any of this was written: a move
// onto the previous window, a replacing link over it, a peer that made its own
// visits, and a session with no visit at all. Each expectation below is one of
// those measurements, not a reading of this runtime.

#[test]
fn a_move_onto_the_previous_window_leaves_the_return_on_what_is_left() {
    let mut store = ManagedSessionStore::new();
    let name = four_windows(&mut store, "stack");
    for index in [3, 1, 2] {
        select(&mut store, &name, index);
    }
    assert_eq!(stack(&store, &name), [1, 3, 0]);

    // The oracle: `move-window -d -k -s 3 -t 1` leaves the stack 2,0 and the
    // return then gives 0,2 - so the history is [0] and the return is 0, never
    // the link 1 that has just been created.
    let _moved = store
        .move_window(&name, 3, 1, true, false)
        .expect("the fixture moves a window onto the previous one");

    assert_eq!(stack(&store, &name), [0]);
    assert_eq!(
        store.session(&name).unwrap().last_window_index(),
        Some(0),
        "the return is the head of what is left, not the new link"
    );
}

#[test]
fn a_standalone_move_onto_the_previous_window_answers_the_same() {
    let mut session = crate::Session::new(session_name("solo"), SIZE);
    for _ in 0..3 {
        let _created = session
            .create_window(SIZE)
            .expect("the fixture creates a detached window");
    }
    for index in [3, 1, 2] {
        session
            .select_window(index)
            .expect("the fixture selects its window");
    }

    let _removed = session
        .move_window(3, 1, true, false)
        .expect("the fixture moves a window onto the previous one");

    assert_eq!(session.window_visit_stack(), [0]);
    assert_eq!(session.last_window_index(), Some(0));
}

#[test]
fn a_peer_keeps_its_own_visits_when_the_source_swaps_or_moves() {
    let mut store = ManagedSessionStore::new();
    let name = four_windows(&mut store, "grp");
    let peer = session_name("grp-peer");
    let _grouped = store
        .create_grouped_session_with_base_index(peer.clone(), SIZE, 0, name.clone())
        .expect("the fixture groups a peer at window 0");
    for index in [3, 1, 2] {
        select(&mut store, &peer, index);
    }
    let before = stack(&store, &peer);
    assert_eq!(before, [1, 3, 0]);

    // The oracle: a swap of two of the source's windows leaves the peer's stack
    // exactly as it was, because a peer is synchronised by the indices it still
    // has and both are still there.
    let _swapped = store
        .swap_windows(&name, 1, 3)
        .expect("the fixture swaps two of the source's windows");
    assert_eq!(stack(&store, &peer), before);

    // And a move of one of them to a free index takes that index out of the
    // peer's history, without giving it to the link that appears.
    let _moved = store
        .move_window(&name, 1, 8, false, false)
        .expect("the fixture moves one away");
    let after = stack(&store, &peer);
    assert!(!after.contains(&1) && !after.contains(&8));
    assert_eq!(after, [3, 0]);
    assert_eq!(
        store.session(&peer).unwrap().last_window_index(),
        Some(3),
        "the peer's return is the head of what it still has"
    );
}

#[test]
fn a_session_that_visited_nothing_has_no_return_at_all() {
    let mut store = ManagedSessionStore::new();
    let name = four_windows(&mut store, "quiet");
    let peer = session_name("quiet-peer");
    let _grouped = store
        .create_grouped_session_with_base_index(peer.clone(), SIZE, 0, name.clone())
        .expect("the fixture groups a peer");

    assert!(stack(&store, &peer).is_empty());
    assert_eq!(store.session(&peer).unwrap().last_window_index(), None);
    // The oracle refuses the return with `no last window` and changes nothing.
    assert!(
        store.last_window(&peer).is_err(),
        "a session that has visited nothing has nowhere to go back to"
    );
    assert_eq!(store.session(&peer).unwrap().active_window_index(), 0);
    assert!(stack(&store, &peer).is_empty());
}

// R4 left three peer paths on the source's own map. The primary answers them
// together: a group synchronisation rebuilds every peer from the indices it
// already had, whatever occupies them afterwards (`session.c:663-678`), while a
// shuffle moves the addressed session's own links (`window.c:1758-1785`).

#[test]
fn a_replacing_link_leaves_a_peer_s_visit_on_the_slot_it_knows() {
    let mut store = ManagedSessionStore::new();
    let (source, peer) = grouped_pair(&mut store);
    let before_peer = stack(&store, &peer);
    assert!(before_peer.contains(&1), "the peer really visited 1");

    let donor = four_windows(&mut store, "donor");
    let donated = store
        .window(&target(&donor, 0))
        .expect("the donor holds its window")
        .id();
    let _replaced = store
        .install_window_alias(&target(&source, 1), donated, true, false)
        .expect("the fixture replaces the link at 1");

    assert_eq!(
        stack(&store, &peer),
        before_peer,
        "the peer still has that slot, so it still has the visit it made there"
    );
    assert!(
        !stack(&store, &source).contains(&1),
        "the addressed session lost the occurrence it visited"
    );
}

#[test]
fn a_compound_insertion_leaves_a_peer_s_visits_on_its_own_slots() {
    let mut store = ManagedSessionStore::new();
    let (source, peer) = grouped_pair(&mut store);
    select(&mut store, &peer, 0);
    let before_peer = stack(&store, &peer);
    assert_eq!(before_peer, [2, 1, 3]);

    // `move-window -a -s src:3 -t src:0`: the server snapshots its peers, opens
    // the room, moves the link into it and only then resolves them.
    let navigation = store
        .group_peer_navigation(&source)
        .expect("the fixture snapshots the peers");
    let _room = store
        .make_room_for_window(&source, 1)
        .expect("the fixture opens room at 1");
    let _moved = store
        .move_window(&source, 4, 1, false, false)
        .expect("the fixture moves the shifted link into the room");
    store
        .reconcile_group_peer_navigation(&navigation)
        .expect("the fixture resolves the peers");

    assert_eq!(
        stack(&store, &peer),
        before_peer,
        "every slot the peer knew is still there, so no visit moved"
    );
    assert_eq!(
        store
            .session(&peer)
            .expect("the peer exists")
            .active_window_index(),
        0,
        "and the slot it was on is one the insertion never touched"
    );
    assert_eq!(
        stack(&store, &source),
        [2, 0],
        "the addressed session's own visits followed its links"
    );
}

#[test]
fn a_standalone_insertion_keeps_the_links_it_shifts() {
    let mut session = crate::Session::new(session_name("solo"), SIZE);
    for _ in 0..3 {
        let _created = session
            .create_window(SIZE)
            .expect("the fixture creates a detached window");
    }
    for index in [3, 1, 2] {
        session
            .select_window(index)
            .expect("the fixture selects its window");
    }
    assert_eq!(session.window_visit_stack(), [1, 3, 0]);
    assert_eq!(session.active_window_index(), 2);

    let _map = session
        .make_room_for_window(1)
        .expect("the fixture opens room at 1");

    assert_eq!(
        session.active_window_index(),
        3,
        "the selection stayed on the link it was on"
    );
    assert_eq!(
        session.window_visit_stack(),
        [2, 4, 0],
        "and every visit followed the link it named"
    );
    assert_eq!(session.last_window_index(), Some(2));
}
