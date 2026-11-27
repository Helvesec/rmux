use super::*;
use crate::SessionStore;
use rmux_proto::{ResizePaneAdjustment, SessionName, SplitDirection, Target};

const SIZE: TerminalSize = TerminalSize {
    cols: 120,
    rows: 40,
};

#[test]
fn public_pane_swap_reproduction_and_standalone_resolver_siblings() {
    for route in [
        "window",
        "exact",
        "effective-pane",
        "effective-window",
        "effective-session",
    ] {
        let mut left = SessionStore::new();
        let mut right = SessionStore::new();
        for store in [&mut left, &mut right] {
            store
                .create_session(SessionName::new("test").unwrap(), SIZE)
                .unwrap();
        }
        let session_target = Target::parse("test").unwrap();
        let session = right.resolve_session_mut(&session_target).unwrap();
        session.split_active_pane().unwrap();
        session.kill_pane(0).unwrap();
        let before_left = left.resolve_window(&session_target).unwrap().layout_dump();
        let before_right = right.resolve_window(&session_target).unwrap().layout_dump();
        std::mem::swap(
            borrow_pane(&mut left, route),
            borrow_pane(&mut right, route),
        );
        assert_eq!(
            left.resolve_window(&session_target).unwrap().layout_dump(),
            before_right,
            "{route}"
        );
        assert_eq!(
            right.resolve_window(&session_target).unwrap().layout_dump(),
            before_left,
            "{route}"
        );
        for store in [&mut left, &mut right] {
            let id = store
                .resolve_window(&session_target)
                .unwrap()
                .pane(0)
                .unwrap()
                .id();
            store
                .resolve_session_mut(&session_target)
                .unwrap()
                .resize_terminal(TerminalSize {
                    cols: 121,
                    rows: 41,
                });
            let window = store.resolve_window(&session_target).unwrap();
            assert_eq!(
                window.pane(0).unwrap().geometry(),
                PaneGeometry::new(0, 0, 121, 41)
            );
            assert!(window.layout_dump().ends_with(&format!(",{}", id.as_u32())));
            assert!(window.borrowed_pane_owners.is_none());
        }
    }
}

fn borrow_pane<'a>(store: &'a mut SessionStore, route: &str) -> &'a mut Pane {
    let target = Target::parse(match route {
        "effective-window" => "test:0",
        "effective-session" => "test",
        _ => "test:0.0",
    })
    .unwrap();
    match route {
        "window" => store
            .resolve_session_mut(&target)
            .unwrap()
            .window_at_mut(0)
            .unwrap()
            .pane_mut(0)
            .unwrap(),
        "exact" => store.resolve_pane_mut(&target).unwrap(),
        _ => store.resolve_effective_pane_mut(&target).unwrap(),
    }
}

fn fixture(layout: LayoutName) -> Window {
    let mut window = Window::new(SIZE);
    window.split_after_position_with_id(0, PaneId::new(10));
    window.split_after_position_with_id(1, PaneId::new(20));
    window.set_layout(layout);
    window
}

#[test]
fn public_replacement_preserves_cells_across_layout_mutations_and_rollback() {
    for layout in [
        LayoutName::EvenHorizontal,
        LayoutName::EvenVertical,
        LayoutName::MainHorizontal,
        LayoutName::MainHorizontalMirrored,
        LayoutName::MainVertical,
        LayoutName::MainVerticalMirrored,
        LayoutName::Tiled,
    ] {
        for operation in 0..10 {
            let mut actual = fixture(layout);
            let original = actual.checkpoint();
            let original_dump = actual.layout_dump();
            let mut expected = actual.clone();
            let replacement =
                Pane::new_with_id(PaneId::new(99), 0, actual.pane(0).unwrap().geometry());
            expected
                .replace_pane_for_swap(0, replacement.clone())
                .unwrap();
            *actual.pane_mut(0).unwrap() = replacement;
            assert_eq!(actual.layout_dump(), expected.layout_dump());
            actual.save_old_layout();
            assert_eq!(actual.old_layout(), Some(expected.layout_dump().as_str()));
            let replacement_checkpoint = actual.checkpoint();
            for window in [&mut actual, &mut expected] {
                match operation {
                    0 => window.set_size(TerminalSize {
                        cols: 135,
                        rows: 47,
                    }),
                    1 => {
                        window.resize_pane_by(0, ResizePaneAdjustment::Right { cells: 2 });
                    }
                    2 => {
                        window.spread_layout(0);
                    }
                    3 => {
                        window.split_after_position_with_id(0, PaneId::new(100));
                    }
                    4 => {
                        window.extract_pane(0).unwrap();
                    }
                    5 => {
                        window.swap_panes(0, 2);
                    }
                    6 => window.rotate_panes(RotateWindowDirection::Down),
                    7 => {
                        window.toggle_zoom(0);
                        window.set_size(TerminalSize {
                            cols: 135,
                            rows: 47,
                        });
                        window.auto_unzoom();
                    }
                    8 => {
                        window
                            .move_pane_by_splitting_target(
                                0,
                                2,
                                2,
                                SplitDirection::Horizontal,
                                false,
                            )
                            .unwrap();
                    }
                    _ => {
                        window
                            .insert_pane_full_size(
                                Pane::new_with_id(
                                    PaneId::new(100),
                                    3,
                                    PaneGeometry::new(0, 0, 1, 1),
                                ),
                                SplitDirection::Vertical,
                                true,
                            )
                            .unwrap();
                    }
                }
            }
            assert_eq!(
                actual.layout_dump(),
                expected.layout_dump(),
                "{layout:?}, operation {operation}"
            );
            for (a, e) in actual.panes().iter().zip(expected.panes()) {
                assert_eq!((a.id(), a.geometry()), (e.id(), e.geometry()));
            }
            actual.restore_checkpoint(replacement_checkpoint);
            actual.set_size(SIZE);
            assert!(actual.layout_dump().contains(",99"));
            actual.restore_checkpoint(original);
            assert_eq!(actual.layout_dump(), original_dump);
            actual.set_size(SIZE);
            assert_eq!(actual.layout_dump(), original_dump);
        }
    }
}

#[test]
fn consecutive_public_replacements_remap_simultaneously_and_clone_independently() {
    let mut window = fixture(LayoutName::MainVerticalMirrored);
    let original = window.layout_dump();
    let mut first = window.pane(0).unwrap().clone();
    first.set_index(1);
    let mut second = window.pane(1).unwrap().clone();
    second.set_index(0);
    *window.pane_mut(0).unwrap() = second;
    // The intermediate duplicate is not committed as layout ownership.
    *window.pane_mut(1).unwrap() = first;
    let mut cloned = window.clone();
    assert_ne!(window.layout_dump(), original);
    window.set_size(TerminalSize {
        cols: 130,
        rows: 50,
    });
    cloned.set_size(SIZE);
    assert_eq!(cloned.panes()[0].id(), PaneId::new(10));
    assert_eq!(cloned.panes()[1].id(), PaneId::new(0));
    assert!(cloned.borrowed_pane_owners.is_none());
    assert!(window.borrowed_pane_owners.is_none());
}

#[test]
fn public_borrow_bookkeeping_preserves_window_equality() {
    let mut window = fixture(LayoutName::MainVerticalMirrored);
    let original = window.clone();
    assert!(window.pane_mut(0).is_some());
    assert_eq!(window, original);
    let checkpoint = window.checkpoint();
    let borrowed = window.clone();
    window.restore_checkpoint(checkpoint);
    assert_eq!(window, borrowed);
    *window.pane_mut(0).unwrap() =
        Pane::new_with_id(PaneId::new(99), 0, window.pane(0).unwrap().geometry());
    let replacement = window.clone();
    let checkpoint = window.checkpoint();
    window.restore_checkpoint(checkpoint);
    assert_eq!(window, replacement);
}
