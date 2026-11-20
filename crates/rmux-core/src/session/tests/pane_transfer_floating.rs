use super::*;
use crate::{FloatingPaneRect, WindowId};

// FMT1 R2: a destination whose pane-list predecessor is floating used to be
// refused, because the positional helper derived the split target from
// `panes[position - 1]` instead of the named destination. The three facts are
// separate now, so the named tiled destination is split and the floating
// neighbour is simply left alone. The refusals kept below are the ones that
// are still refusals: a floating *destination*.
#[test]
fn join_before_floating_predecessor_transfers_into_the_named_target() {
    for cross_session in [false, true] {
        for direction in [SplitDirection::Vertical, SplitDirection::Horizontal] {
            for detached in [false, true] {
                for zoom in [false, true] {
                    let size = TerminalSize::new(120, 40);
                    let mut destination = Session::new(session_name("destination"), size);
                    let floating = destination.split_pane_in_window(0, 0).unwrap();
                    let target = destination.split_pane_in_window(0, floating).unwrap();
                    destination
                        .window_at_mut(0)
                        .unwrap()
                        .float_pane(floating, FloatingPaneRect::new(3, 2, 20, 8).unwrap())
                        .unwrap();
                    let mut source = Session::new_with_initial_window(
                        session_name("source"),
                        size,
                        0,
                        PaneId::new(100),
                        WindowId::new(100),
                    );
                    let source_window = if cross_session {
                        0
                    } else {
                        destination.create_window(size).unwrap().0
                    };
                    if zoom {
                        destination
                            .window_layout_context_mut(0)
                            .unwrap()
                            .toggle_zoom(target)
                            .unwrap();
                        if cross_session {
                            source
                                .window_layout_context_mut(0)
                                .unwrap()
                                .toggle_zoom(0)
                                .unwrap();
                        } else {
                            destination
                                .window_layout_context_mut(source_window)
                                .unwrap()
                                .toggle_zoom(0)
                                .unwrap();
                        }
                    }
                    let floating_id = destination
                        .window_at(0)
                        .expect("destination window")
                        .pane(floating)
                        .expect("floating neighbour")
                        .id();
                    let target_id = destination
                        .window_at(0)
                        .expect("destination window")
                        .pane(target)
                        .expect("named destination")
                        .id();
                    let moved_id = if cross_session {
                        source.window_at(0).expect("source window").pane(0)
                    } else {
                        destination
                            .window_at(source_window)
                            .expect("source window")
                            .pane(0)
                    }
                    .expect("source pane")
                    .id();
                    let destination_before = destination.clone();
                    let source_before = source.clone();
                    let options = PaneJoinOptions::new(direction, detached, true, false, None);
                    let result = if cross_session {
                        destination.join_pane_from_session(
                            SessionPaneTarget::new(0, target),
                            &mut source,
                            SessionPaneTarget::new(0, 0),
                            options,
                        )
                    } else {
                        destination.join_pane(
                            SessionPaneTarget::new(source_window, 0),
                            SessionPaneTarget::new(0, target),
                            options,
                        )
                    };
                    result.expect("the named tiled destination is split");
                    let window = destination
                        .window_at(0)
                        .expect("destination window survives");
                    let moved = window
                        .panes()
                        .iter()
                        .position(|pane| pane.id() == moved_id)
                        .expect("the moved pane reaches the destination window");
                    let anchor = window
                        .panes()
                        .iter()
                        .position(|pane| pane.id() == target_id)
                        .expect("the named destination survives");
                    assert_eq!(
                        moved,
                        anchor + 1,
                        "tmux reinserts the moved pane right after its destination"
                    );
                    assert!(
                        window.floating_stack().contains(floating_id),
                        "the floating neighbour keeps its placement"
                    );
                    assert_ne!(
                        destination, destination_before,
                        "the destination really took the pane"
                    );
                    let _ = &source_before;
                }
            }
        }
    }
}

// Expected refusal/unzoom and self-swap behavior comes from the qualified
// tmux3.7b034 cases and cmd-swap-pane.c, not from the implementation predicate.
#[test]
fn l1_035_standalone_swaps_preserve_owners_and_exact_nonzoom_state() {
    for route in 0..3 {
        for (source_float, target_float, self_swap) in [
            (true, false, false),
            (false, true, false),
            (true, true, false),
            (true, true, true),
        ] {
            if self_swap && route != 0 {
                continue;
            }
            for zoom in [false, true] {
                let size = TerminalSize::new(80, 12);
                let mut a = Session::new(session_name("a"), size);
                let mut b = Session::new_with_initial_window(
                    session_name("b"),
                    size,
                    0,
                    PaneId::new(100),
                    WindowId::new(100),
                );
                for session in [&mut a, &mut b] {
                    session.split_pane_in_window(0, 0).unwrap();
                    session.split_pane_in_window(0, 0).unwrap();
                }
                if route == 1 {
                    a.create_window(size).unwrap();
                    a.split_pane_in_window(1, 0).unwrap();
                    a.split_pane_in_window(1, 0).unwrap();
                }
                let source_index = if source_float { 1 } else { 0 };
                let target_index = if self_swap {
                    source_index
                } else if target_float {
                    2
                } else {
                    0
                };
                let target_window = if route == 1 { 1 } else { 0 };
                if source_float {
                    a.window_at_mut(0)
                        .unwrap()
                        .float_pane(source_index, FloatingPaneRect::new(-3, 1, 12, 4).unwrap())
                        .unwrap();
                }
                if target_float && !self_swap {
                    let w = if route == 2 {
                        b.window_at_mut(0)
                    } else {
                        a.window_at_mut(target_window)
                    }
                    .unwrap();
                    w.float_pane(target_index, FloatingPaneRect::new(4, 2, 12, 4).unwrap())
                        .unwrap();
                }
                if zoom {
                    a.window_at_mut(0)
                        .unwrap()
                        .toggle_zoom(if source_float { 0 } else { 1 });
                    if route == 1 {
                        a.window_at_mut(1).unwrap().toggle_zoom(0);
                    }
                    if route == 2 {
                        b.window_at_mut(0).unwrap().toggle_zoom(0);
                    }
                }
                let mut expected_a = a.clone();
                let mut expected_b = b.clone();
                expected_a.window_at_mut(0).unwrap().auto_unzoom();
                if route == 1 {
                    expected_a.window_at_mut(1).unwrap().auto_unzoom();
                }
                if route == 2 {
                    expected_b.window_at_mut(0).unwrap().auto_unzoom();
                }
                let source = SessionPaneTarget::new(0, source_index);
                let target = SessionPaneTarget::new(target_window, target_index);
                let options = PaneSwapOptions::new(true, false);
                let result = if route == 2 {
                    a.swap_panes_with_session(source, &mut b, target, options)
                } else {
                    a.swap_panes(source, target, options)
                };
                if self_swap {
                    result.unwrap();
                } else {
                    assert_eq!(
                        result.unwrap_err(),
                        RmuxError::Server("cannot swap floating panes".into())
                    );
                }
                assert_eq!(a, expected_a, "route={route}, zoom={zoom}");
                assert_eq!(b, expected_b, "route={route}, zoom={zoom}");
                // Owner retention and subsequent ordinary operations are part
                // of the regression: a rejected swap cannot remove a window.
                assert!(a.window_at(0).is_some());
                if route == 1 {
                    assert!(a.window_at(1).is_some());
                }
                a.window_layout_context_mut(0)
                    .unwrap()
                    .resize_pane(
                        source_index,
                        rmux_proto::ResizePaneAdjustment::AbsoluteHeight { rows: 3 },
                    )
                    .unwrap();
            }
        }
    }
}

#[test]
fn l1_035_managed_alias_and_distinct_owner_swaps_match_public_core_contract() {
    use crate::{ManagedPaneSplit, ManagedSessionStore};
    use rmux_proto::WindowTarget;
    for aliases in [false, true] {
        for (source_float, target_float, self_swap) in [
            (true, false, false),
            (false, true, false),
            (true, true, false),
            (true, true, true),
        ] {
            if self_swap && !aliases {
                continue;
            }
            for zoom in [false, true] {
                let size = TerminalSize::new(80, 12);
                let mut store = ManagedSessionStore::new();
                let a = WindowTarget::with_window(session_name("a"), 0);
                let mut b = WindowTarget::with_window(session_name("b"), 0);
                store.create_session(session_name("a"), size).unwrap();
                store.create_session(session_name("b"), size).unwrap();
                for target in [&a, &b] {
                    for _ in 0..2 {
                        let _ = store
                            .split_pane(
                                target,
                                0,
                                ManagedPaneSplit {
                                    direction: SplitDirection::Vertical,
                                    before: false,
                                    detached: true,
                                    full_size: false,
                                },
                            )
                            .unwrap();
                    }
                }
                if aliases {
                    let id = store.window(&a).unwrap().id();
                    b = WindowTarget::with_window(session_name("b"), 7);
                    store.link_window(&b, id).unwrap();
                }
                let source_index = if source_float { 1 } else { 0 };
                let target_index = if self_swap {
                    source_index
                } else if target_float {
                    2
                } else {
                    0
                };
                if source_float {
                    store
                        .window_layout_context_mut(&a)
                        .unwrap()
                        .float_pane(source_index, FloatingPaneRect::new(-3, 1, 12, 4).unwrap())
                        .unwrap();
                }
                if target_float && !self_swap {
                    store
                        .window_layout_context_mut(&b)
                        .unwrap()
                        .float_pane(target_index, FloatingPaneRect::new(4, 2, 12, 4).unwrap())
                        .unwrap();
                }
                if zoom {
                    store
                        .window_layout_context_mut(&a)
                        .unwrap()
                        .toggle_zoom(if source_float { 0 } else { 1 })
                        .unwrap();
                    if !aliases {
                        store
                            .window_layout_context_mut(&b)
                            .unwrap()
                            .toggle_zoom(0)
                            .unwrap();
                    }
                }
                let mut before_a = store.window(&a).unwrap().clone();
                let mut before_b = store.window(&b).unwrap().clone();
                let pointer_a = std::ptr::from_ref(store.window(&a).unwrap());
                let pointer_b = std::ptr::from_ref(store.window(&b).unwrap());
                let links_a = store
                    .session(&session_name("a"))
                    .unwrap()
                    .windows()
                    .map(|(i, w)| (i, w.id()))
                    .collect::<Vec<_>>();
                let links_b = store
                    .session(&session_name("b"))
                    .unwrap()
                    .windows()
                    .map(|(i, w)| (i, w.id()))
                    .collect::<Vec<_>>();
                before_a.auto_unzoom();
                before_b.auto_unzoom();
                let result = store.swap_panes(
                    &a,
                    source_index,
                    &b,
                    target_index,
                    PaneSwapOptions::new(true, false),
                );
                if self_swap {
                    let _ = result.unwrap();
                } else {
                    assert_eq!(
                        result.unwrap_err(),
                        RmuxError::Server("cannot swap floating panes".into())
                    );
                }
                assert_eq!(store.window(&a).unwrap(), &before_a);
                assert_eq!(store.window(&b).unwrap(), &before_b);
                assert_eq!(std::ptr::from_ref(store.window(&a).unwrap()), pointer_a);
                assert_eq!(std::ptr::from_ref(store.window(&b).unwrap()), pointer_b);
                assert_eq!(
                    store
                        .session(&session_name("a"))
                        .unwrap()
                        .windows()
                        .map(|(i, w)| (i, w.id()))
                        .collect::<Vec<_>>(),
                    links_a
                );
                assert_eq!(
                    store
                        .session(&session_name("b"))
                        .unwrap()
                        .windows()
                        .map(|(i, w)| (i, w.id()))
                        .collect::<Vec<_>>(),
                    links_b
                );
            }
        }
    }
}

#[test]
fn l1_035_preserve_zoom_error_and_self_exception_match_native_oracle() {
    //035native1: -Z does not rezoom a rejected distinct swap, but it does
    //restore zoom for the same-pane success path. Compare complete windows:
    //the V2 exporter intentionally rejects the zoomed before-state.
    for cross_window in [false, true] {
        let size = TerminalSize::new(80, 12);
        let mut session = Session::new(session_name("zoom035"), size);
        session.split_pane_in_window(0, 0).unwrap();
        session
            .window_at_mut(0)
            .unwrap()
            .float_pane(1, FloatingPaneRect::new(-3, 1, 12, 4).unwrap())
            .unwrap();
        if cross_window {
            session.create_window(size).unwrap();
            session.split_pane_in_window(1, 0).unwrap();
            session.window_at_mut(1).unwrap().toggle_zoom(0);
        }
        session.window_at_mut(0).unwrap().toggle_zoom(0);
        let snapshot = |session: &Session, index| session.window_at(index).unwrap().clone();
        let before = snapshot(&session, 0);
        assert!(
            before.is_zoomed(),
            "the self-swap starts from a zoomed window"
        );
        session
            .swap_panes(
                SessionPaneTarget::new(0, 1),
                SessionPaneTarget::new(0, 1),
                PaneSwapOptions::new(true, true),
            )
            .unwrap();
        assert_eq!(
            snapshot(&session, 0),
            before,
            "same float with -Z keeps zoom and all public state"
        );
        let other = if cross_window { 1 } else { 0 };
        let mut expected_source = snapshot(&session, 0);
        // Complete Window equality also observes push/pop bookkeeping, which
        // V2 does not export. A refused -Z has pushed zoom but does not pop it.
        // Keep that field explicit instead of masking it in the comparison.
        assert!(expected_source.push_zoom(true));
        let mut expected_target = snapshot(&session, other);
        assert!(expected_target.push_zoom(true));
        assert_eq!(
            session
                .swap_panes(
                    SessionPaneTarget::new(0, 1),
                    SessionPaneTarget::new(other, 0),
                    PaneSwapOptions::new(true, true)
                )
                .unwrap_err(),
            RmuxError::Server("cannot swap floating panes".into())
        );
        assert_eq!(snapshot(&session, 0), expected_source);
        assert_eq!(snapshot(&session, other), expected_target);
        assert!(!session.window_at(0).unwrap().is_zoomed());
        assert!(!session.window_at(other).unwrap().is_zoomed());
        // The next operation starts a fresh push/pop cycle. On an already
        // unzoomed source it clears the internal pending bit without rezooming.
        assert!(!expected_source.push_zoom(false));
        session
            .swap_panes(
                SessionPaneTarget::new(0, 1),
                SessionPaneTarget::new(0, 1),
                PaneSwapOptions::new(true, false),
            )
            .unwrap();
        assert_eq!(
            snapshot(&session, 0),
            expected_source,
            "subsequent self-swap has no latent exchange or zoom effect"
        );
        if cross_window {
            assert_eq!(snapshot(&session, other), expected_target);
        }
        eprintln!(
            "swap zoom oracle cross_window={cross_window}: self -Z retained full window; \
             distinct -Z refused and unzoomed; following self retained unzoomed window"
        );
    }
}
