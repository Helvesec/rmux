// The order a window names its sessions in after a command that reshapes ONE
// session's links: a swap, a move (and `new-window -k`, which moves the new
// window over the killed one), a slot opened by `-a`/`-b`, a pane broken out.
// Each expectation was measured on tmux 3.7b on private sockets, in R3495
// `linked3.sh` or S2376 `linked4.sh`, whose case each test names.
use super::*;
use crate::BreakPaneOptions;

fn order_of(store: &ManagedSessionStore, session: &str, index: u32) -> Option<String> {
    let names = store.window_link_order(&target(session, index))?;
    Some(
        names
            .iter()
            .map(SessionName::as_str)
            .collect::<Vec<_>>()
            .join(","),
    )
}

fn assert_orders(store: &ManagedSessionStore, expected: &[(&str, u32, &str)]) {
    for (session, index, names) in expected {
        assert_eq!(
            order_of(store, session, *index).as_deref(),
            Some(*names),
            "{session}:{index}"
        );
    }
}

/// `new -s p; neww -t p:1; new -t p -s <member>...`: a group whose windows all
/// name `p` first.
fn group_of_two_windows(members: &[&str]) -> ManagedSessionStore {
    let mut store = ManagedSessionStore::new();
    store.create_session(name("p"), size()).unwrap();
    let _ = store
        .insert_window_with_initial_pane(&name("p"), 1, size())
        .unwrap();
    for member in members {
        store
            .create_grouped_session_with_base_index(name(member), size(), 0, name("p"))
            .unwrap();
    }
    store
}

fn link_outside(store: &mut ManagedSessionStore, from: (&str, u32), to: (&str, u32)) {
    let id = store.window(&target(from.0, from.1)).unwrap().id();
    store.link_window(&target(to.0, to.1), id).unwrap();
}

fn split(store: &mut ManagedSessionStore, session: &str, index: u32) {
    let _ = store
        .split_pane(
            &target(session, index),
            0,
            ManagedPaneSplit {
                direction: SplitDirection::Horizontal,
                before: false,
                detached: true,
                full_size: false,
            },
        )
        .unwrap();
}

#[test]
fn a_swap_in_a_member_names_that_member_before_the_peers_it_rebuilt() {
    // linked3 case 5: `swap-window -s g:0 -t g:1` names every window `g,p`.
    let mut store = group_of_two_windows(&["g"]);
    let _ = store.swap_windows(&name("g"), 0, 1).unwrap();
    assert_orders(
        &store,
        &[
            ("g", 0, "g,p"),
            ("g", 1, "g,p"),
            ("p", 0, "g,p"),
            ("p", 1, "g,p"),
        ],
    );

    // linked4 case 5: the peers come back in the order they joined.
    let mut store = group_of_two_windows(&["h", "g"]);
    let _ = store.swap_windows(&name("h"), 0, 1).unwrap();
    assert_orders(&store, &[("p", 0, "h,p,g"), ("g", 1, "h,p,g")]);
}

#[test]
fn a_swapped_link_goes_to_the_tail_of_the_window_it_now_holds() {
    // linked4 case 2: `p:1` also linked as `o:3`, then `swap -s p:0 -t p:1`.
    let mut store = group_of_two_windows(&[]);
    store.create_session(name("o"), size()).unwrap();
    link_outside(&mut store, ("p", 1), ("o", 3));
    assert_eq!(order_of(&store, "o", 3).as_deref(), Some("p,o"));
    let _ = store.swap_windows(&name("p"), 0, 1).unwrap();
    assert_orders(&store, &[("o", 3, "o,p"), ("p", 0, "o,p"), ("p", 1, "p")]);

    // linked4 case 7: the same in a group, from the member `g`.
    let mut store = group_of_two_windows(&["g"]);
    store.create_session(name("o"), size()).unwrap();
    link_outside(&mut store, ("p", 1), ("o", 3));
    let _ = store.swap_windows(&name("g"), 0, 1).unwrap();
    assert_orders(
        &store,
        &[("o", 3, "o,g,p"), ("p", 0, "o,g,p"), ("g", 1, "g,p")],
    );
}

#[test]
fn a_swap_of_a_window_with_itself_moves_no_name() {
    // linked4 case 12: tmux returns before touching any winlink.
    let mut store = ManagedSessionStore::new();
    store.create_session(name("p"), size()).unwrap();
    link_outside(&mut store, ("p", 0), ("p", 1));
    store.create_session(name("o"), size()).unwrap();
    link_outside(&mut store, ("p", 0), ("o", 3));
    let _ = store.swap_windows(&name("p"), 0, 1).unwrap();
    assert_orders(&store, &[("p", 0, "p,p,o"), ("o", 3, "p,p,o")]);
}

#[test]
fn a_moved_link_goes_to_the_tail_and_the_peers_follow_it() {
    // linked3 case 10: `move-window -s g:1 -t g:7` names every window `g,p`.
    let mut store = group_of_two_windows(&["g"]);
    let _ = store.move_window(&name("g"), 1, 7, false, false).unwrap();
    assert_orders(&store, &[("g", 0, "g,p"), ("g", 7, "g,p"), ("p", 7, "g,p")]);

    // linked4 case 3: standalone, the moved link follows an outsider's.
    let mut store = group_of_two_windows(&[]);
    store.create_session(name("o"), size()).unwrap();
    link_outside(&mut store, ("p", 1), ("o", 3));
    let _ = store.move_window(&name("p"), 1, 5, false, false).unwrap();
    assert_orders(&store, &[("p", 5, "o,p"), ("o", 3, "o,p")]);

    // linked4 case 6: in a group, with that outsider.
    let mut store = group_of_two_windows(&["g"]);
    store.create_session(name("o"), size()).unwrap();
    link_outside(&mut store, ("p", 1), ("o", 3));
    let _ = store.move_window(&name("g"), 1, 7, false, false).unwrap();
    assert_orders(&store, &[("o", 3, "o,g,p"), ("p", 0, "g,p")]);
}

#[test]
fn a_window_moved_over_a_killed_one_takes_its_slot_from_every_member() {
    // linked3 case 9: `p:1` also linked as `o:2`, then `neww -k -t g:1`, which
    // makes the window at a free slot and moves it over `g:1`.
    let mut store = group_of_two_windows(&["g"]);
    store.create_session(name("o"), size()).unwrap();
    link_outside(&mut store, ("p", 1), ("o", 2));
    let _ = store
        .insert_window_with_initial_pane(&name("g"), 2, size())
        .unwrap();
    let _ = store.move_window(&name("g"), 2, 1, true, false).unwrap();
    assert_orders(
        &store,
        &[
            ("g", 0, "g,p"),
            ("g", 1, "g,p"),
            ("p", 1, "g,p"),
            ("o", 2, "o"),
        ],
    );

    // linked4 case 11: `move-window -k -s g:1 -t g:2` over a window `o` links.
    let mut store = group_of_two_windows(&["g"]);
    let _ = store
        .insert_window_with_initial_pane(&name("p"), 2, size())
        .unwrap();
    store.create_session(name("o"), size()).unwrap();
    link_outside(&mut store, ("p", 2), ("o", 3));
    let _ = store.move_window(&name("g"), 1, 2, true, false).unwrap();
    assert_orders(&store, &[("g", 0, "g,p"), ("p", 2, "g,p"), ("o", 3, "o")]);
}

#[test]
fn a_slot_opened_by_new_window_a_keeps_the_shifted_links_in_place() {
    // linked3 case 6: `neww -a -t g:0` names every window `g,p`.
    let mut store = group_of_two_windows(&["g"]);
    let _ = store.make_room_for_window(&name("g"), 1).unwrap();
    let _ = store
        .insert_window_with_initial_pane(&name("g"), 1, size())
        .unwrap();
    for index in 0..3 {
        assert_orders(&store, &[("g", index, "g,p"), ("p", index, "g,p")]);
    }

    // linked4 case 8: from `p` instead, every window names `p,g`.
    let mut store = group_of_two_windows(&["g"]);
    let _ = store.make_room_for_window(&name("p"), 1).unwrap();
    let _ = store
        .insert_window_with_initial_pane(&name("p"), 1, size())
        .unwrap();
    assert_orders(&store, &[("g", 0, "p,g"), ("g", 1, "p,g"), ("g", 2, "p,g")]);

    // linked4 case 1: a shifted link keeps its place before an outsider's.
    let mut store = ManagedSessionStore::new();
    store.create_session(name("o"), size()).unwrap();
    store.create_session(name("p"), size()).unwrap();
    let _ = store
        .insert_window_with_initial_pane(&name("p"), 1, size())
        .unwrap();
    link_outside(&mut store, ("p", 1), ("o", 3));
    let _ = store.make_room_for_window(&name("p"), 1).unwrap();
    let _ = store
        .insert_window_with_initial_pane(&name("p"), 1, size())
        .unwrap();
    assert_orders(&store, &[("p", 2, "p,o"), ("o", 3, "p,o"), ("p", 1, "p")]);
}

#[test]
fn a_pane_broken_out_in_a_member_names_that_member_first() {
    // linked3 case 11: `break-pane -s g:0.1 -t g:4` names every window `g,p`.
    let mut store = ManagedSessionStore::new();
    store.create_session(name("p"), size()).unwrap();
    split(&mut store, "p", 0);
    store
        .create_grouped_session_with_base_index(name("g"), size(), 0, name("p"))
        .unwrap();
    let options = BreakPaneOptions::new(Some(4), None, true, false, false);
    let broken = store
        .break_pane(&target("g", 0), 1, &name("g"), options)
        .unwrap();
    assert_eq!(broken.window_index, 4);
    assert_orders(&store, &[("g", 0, "g,p"), ("g", 4, "g,p"), ("p", 4, "g,p")]);

    // linked4 case 4: `break-pane -a` keeps a shifted link before an outsider.
    let mut store = group_of_two_windows(&[]);
    split(&mut store, "p", 0);
    store.create_session(name("o"), size()).unwrap();
    link_outside(&mut store, ("p", 1), ("o", 3));
    let options = BreakPaneOptions::new(Some(0), None, true, true, false);
    let broken = store
        .break_pane(&target("p", 0), 1, &name("p"), options)
        .unwrap();
    assert_eq!(broken.window_index, 1);
    assert_orders(&store, &[("p", 2, "p,o"), ("o", 3, "p,o"), ("p", 1, "p")]);
}
