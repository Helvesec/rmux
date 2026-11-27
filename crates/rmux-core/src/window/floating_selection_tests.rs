use super::*;
use crate::{PaneSwapOptions, Session, SessionPaneTarget};
use rmux_proto::SessionName;

fn setup(window: &mut Window, base: u32) -> [PaneId; 3] {
    window.split_after_position_with_id(0, PaneId::new(base));
    window.split_after_position_with_id(1, PaneId::new(base + 1));
    for index in [1, 2] {
        window
            .float_pane(index, FloatingPaneRect::new(2, 3, 30, 10).unwrap())
            .unwrap();
    }
    window.select_pane(1);
    window.select_pane(0);
    window.raise_floating_pane(2).unwrap();
    std::array::from_fn(|i| window.pane(i as u32).unwrap().id())
}

fn order(window: &Window) -> Vec<PaneId> {
    window
        .floating_stack()
        .placements()
        .iter()
        .map(|p| p.pane_id())
        .collect()
}

#[test]
fn floating_implicit_removal_selection_raises_with_history_or_fallback() {
    for history in [false, true] {
        for visible in [false, true] {
            let mut window = Window::new(TerminalSize::new(120, 40));
            let [tile, rear, front] = setup(&mut window, 10);
            if !history {
                window.last_pane = None;
            }
            window.set_floating_pane_visible(1, visible).unwrap();
            let placement = window.floating_stack().placement(rear).unwrap();
            assert_eq!(order(&window), vec![rear, front]);
            assert_eq!(window.remove_pane(0).unwrap().id(), tile);
            assert_eq!(window.active_pane().unwrap().id(), rear);
            assert_eq!(window.last_pane_index(), None);
            assert_eq!(order(&window), vec![front, rear]);
            assert_eq!(window.floating_stack().placement(rear), Some(placement));
        }
    }
}

#[test]
fn floating_inactive_removal_preserves_order_and_selection() {
    let mut window = Window::new(TerminalSize::new(120, 40));
    let [_, rear, front] = setup(&mut window, 10);
    window.select_pane(1);
    window.raise_floating_pane(2).unwrap();
    window.remove_pane(0).unwrap();
    assert_eq!(window.active_pane().unwrap().id(), rear);
    assert_eq!(order(&window), vec![rear, front]);
    assert_eq!(window.last_pane_index(), None);
}

// tmux 3.7b, source-bound L1-034/035 oracle: distinct floating swaps
// refuse after unzooming. Selection, stack order and pane ownership survive.
#[test]
fn floating_implicit_swap_selection_raises_in_both_windows() {
    for detached in [false, true] {
        for active in [false, true] {
            for zoomed in [false, true] {
                let mut session = Session::new(
                    SessionName::new("swap").unwrap(),
                    TerminalSize::new(120, 40),
                );
                let other = session.create_window(TerminalSize::new(120, 40)).unwrap().0;
                for (index, base) in [(0, 10), (other, 20)] {
                    let window = session.window_at_mut(index).unwrap();
                    setup(window, base);
                    if active {
                        window.select_pane(1);
                        window.raise_floating_pane(2).unwrap();
                    }
                }
                if zoomed {
                    for index in [0, other] {
                        let window = session.window_at_mut(index).unwrap();
                        window.toggle_zoom(window.active_pane_index());
                    }
                }
                let mut expected = session.clone();
                for index in [0, other] {
                    expected.window_at_mut(index).unwrap().auto_unzoom();
                }
                assert_eq!(
                    session.swap_panes(
                        SessionPaneTarget::new(0, 1),
                        SessionPaneTarget::new(other, 1),
                        PaneSwapOptions::new(detached, false),
                    ),
                    Err(rmux_proto::RmuxError::Server(
                        "cannot swap floating panes".to_owned()
                    ))
                );
                assert_eq!(
                    session, expected,
                    "owners, selection, history and placements survive"
                );
            }
        }
    }
}

#[test]
fn floating_within_window_swap_raises_final_selection() {
    for detached in [false, true] {
        for zoomed in [false, true] {
            let mut session = Session::new(
                SessionName::new("swap").unwrap(),
                TerminalSize::new(120, 40),
            );
            let [tile, rear, front] = setup(session.window_at_mut(0).unwrap(), 10);
            if zoomed {
                session.window_at_mut(0).unwrap().toggle_zoom(0);
            }
            let mut expected = session.clone();
            expected.window_at_mut(0).unwrap().auto_unzoom();
            assert_eq!(
                session.swap_panes(
                    SessionPaneTarget::new(0, 0),
                    SessionPaneTarget::new(0, 1),
                    PaneSwapOptions::new(detached, false),
                ),
                Err(rmux_proto::RmuxError::Server(
                    "cannot swap floating panes".to_owned()
                ))
            );
            assert_eq!(session, expected);
            let window = session.window_at(0).unwrap();
            assert_eq!(window.active_pane().unwrap().id(), tile);
            assert_eq!(order(window), vec![rear, front]);
        }
    }
}

#[test]
fn floating_active_removal_raises_last_float_and_single_survivor_keeps_placement() {
    let mut window = Window::new(TerminalSize::new(120, 40));
    let [tile, rear, front] = setup(&mut window, 10);
    window
        .float_pane(0, FloatingPaneRect::new(4, 5, 25, 8).unwrap())
        .unwrap();
    window.raise_floating_pane(2).unwrap();
    window.remove_pane(0).unwrap();
    assert_eq!(window.active_pane().unwrap().id(), rear);
    assert_eq!(order(&window), vec![front, rear]);
    assert!(window.floating_stack().placement(tile).is_none());
    let placement = window.floating_stack().placement(front).unwrap();
    window.remove_pane(0).unwrap();
    assert_eq!(window.active_pane().unwrap().id(), front);
    assert_eq!(window.active_pane_index(), 0);
    assert_eq!(window.last_pane_index(), None);
    assert_eq!(window.floating_stack().placements(), &[placement]);
}
