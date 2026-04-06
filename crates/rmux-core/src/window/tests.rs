use super::Window;
use crate::{PaneGeometry, WINDOW_BELL};
use rmux_proto::{
    LayoutName, ResizePaneAdjustment, RotateWindowDirection, SelectPaneDirection, SplitDirection,
    TerminalSize,
};

fn layout_string(body: &str) -> String {
    format!("{:04x},{}", crate::layout::layout_checksum(body), body)
}

#[test]
fn new_window_starts_with_a_single_full_size_pane() {
    let window = Window::new(TerminalSize {
        cols: 120,
        rows: 40,
    });

    assert_eq!(window.layout(), LayoutName::MainVertical);
    assert_eq!(window.pane_count(), 1);
    assert_eq!(
        window.pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 120, 40)
    );
}

#[test]
fn split_after_position_inserts_after_the_target_and_uses_next_index() {
    let mut window = Window::new(TerminalSize {
        cols: 120,
        rows: 40,
    });

    let new_index = window.split_after_position(0);

    assert_eq!(new_index, 1);
    assert_eq!(window.active_pane_index(), 1);
    assert_eq!(window.last_pane_index(), Some(0));
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[test]
fn repeated_split_of_pane_zero_renumbers_panes_by_position() {
    let mut window = Window::new(TerminalSize {
        cols: 120,
        rows: 40,
    });
    window.split_after_position(0);

    let pane_zero_position = window.pane_position(0).expect("pane 0 exists");
    let new_index = window.split_after_position(pane_zero_position);

    assert_eq!(new_index, 1);
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}

#[test]
fn pane_lookup_uses_renumbered_indices_after_repeated_split() {
    let mut window = Window::new(TerminalSize {
        cols: 120,
        rows: 40,
    });
    window.split_after_position(0);
    let pane_zero_position = window.pane_position(0).expect("pane 0 exists");
    window.split_after_position(pane_zero_position);

    assert_eq!(window.pane_position(1), Some(1));
    assert_eq!(window.pane_position(2), Some(2));
}

#[test]
fn horizontal_split_uses_top_bottom_geometry() {
    let mut window = Window::new(TerminalSize {
        cols: 120,
        rows: 40,
    });

    let new_index = window.split_after_position_with_id_and_direction(
        0,
        crate::PaneId::new(1),
        SplitDirection::Horizontal,
    );

    assert_eq!(new_index, 1);
    assert_eq!(window.layout(), LayoutName::MainHorizontal);
    assert_eq!(
        window.pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 120, 20)
    );
    assert_eq!(
        window.pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(0, 21, 120, 19)
    );
}

#[test]
fn repeated_horizontal_split_of_pane_zero_uses_main_horizontal_geometry() {
    let mut window = Window::new(TerminalSize {
        cols: 100,
        rows: 50,
    });
    window.split_after_position_with_id_and_direction(
        0,
        crate::PaneId::new(1),
        SplitDirection::Horizontal,
    );

    let pane_zero_position = window.pane_position(0).expect("pane 0 exists");
    let new_index = window.split_after_position_with_id_and_direction(
        pane_zero_position,
        crate::PaneId::new(2),
        SplitDirection::Horizontal,
    );

    assert_eq!(new_index, 1);
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(
        window.pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 100, 12)
    );
    assert_eq!(
        window.pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(0, 13, 100, 12)
    );
    assert_eq!(
        window.pane(2).expect("pane 2 exists").geometry(),
        PaneGeometry::new(0, 26, 100, 24)
    );
}

#[test]
fn resize_main_pane_recalculates_existing_geometry() {
    let mut window = Window::new(TerminalSize {
        cols: 200,
        rows: 50,
    });
    window.split_after_position(0);
    window.split_after_position(1);

    window.resize_main_pane(ResizePaneAdjustment::AbsoluteWidth { columns: 34 });

    assert_eq!(
        window.pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 34, 50)
    );
    assert_eq!(
        window.pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(35, 0, 165, 24)
    );
    assert_eq!(
        window.pane(2).expect("pane 2 exists").geometry(),
        PaneGeometry::new(35, 25, 165, 25)
    );
}

#[test]
fn set_size_preserves_existing_layout_proportions() {
    let mut window = Window::new(TerminalSize {
        cols: 120,
        rows: 40,
    });
    window.split_after_position(0);
    window.resize_main_pane(ResizePaneAdjustment::AbsoluteWidth { columns: 34 });

    window.set_size(TerminalSize {
        cols: 200,
        rows: 50,
    });

    assert_eq!(
        window.size(),
        TerminalSize {
            cols: 200,
            rows: 50
        }
    );
    assert_eq!(
        window.pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 74, 50)
    );
    assert_eq!(
        window.pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(75, 0, 125, 50)
    );
}

#[test]
fn set_size_recomputes_the_default_main_width_when_none_was_requested() {
    let mut window = Window::new(TerminalSize {
        cols: 120,
        rows: 40,
    });
    window.split_after_position(0);

    window.set_size(TerminalSize {
        cols: 200,
        rows: 50,
    });

    assert_eq!(
        window.pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 100, 50)
    );
    assert_eq!(
        window.pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(101, 0, 99, 50)
    );
}

#[test]
fn explicit_main_width_preserves_the_clamped_ratio_across_terminal_resizes() {
    let mut window = Window::new(TerminalSize { cols: 80, rows: 20 });
    window.split_after_position(0);

    window.resize_main_pane(ResizePaneAdjustment::AbsoluteWidth { columns: 90 });
    assert_eq!(
        window.pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 78, 20)
    );

    window.set_size(TerminalSize {
        cols: 100,
        rows: 20,
    });

    assert_eq!(
        window.pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 88, 20)
    );
    assert_eq!(
        window.pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(89, 0, 11, 20)
    );
}

#[test]
fn explicit_main_width_set_on_a_single_pane_is_used_after_a_future_split() {
    let mut window = Window::new(TerminalSize { cols: 70, rows: 22 });

    window.resize_main_pane(ResizePaneAdjustment::AbsoluteWidth { columns: 34 });
    window.split_after_position(0);

    assert_eq!(
        window.pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 35, 22)
    );
    assert_eq!(
        window.pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(36, 0, 34, 22)
    );
}

#[test]
fn custom_layout_resize_keeps_the_existing_tree_shape() {
    let mut window = Window::new(TerminalSize {
        cols: 100,
        rows: 40,
    });
    window.split_after_position(0);
    window.split_after_position(1);

    let custom_layout =
        layout_string("100x40,0,0{60x40,0,0,0,39x40,61,0[39x19,61,0,1,39x20,61,20,2]}");
    window
        .apply_custom_layout(&custom_layout)
        .expect("custom layout applies");

    window.resize_main_pane(ResizePaneAdjustment::AbsoluteWidth { columns: 34 });

    assert_eq!(
        window.layout_dump(),
        layout_string("100x40,0,0{34x40,0,0,0,65x40,35,0[65x19,35,0,1,65x20,35,20,2]}")
    );
    assert_eq!(
        window.pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 34, 40)
    );
    assert_eq!(
        window.pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(35, 0, 65, 19)
    );
    assert_eq!(
        window.pane(2).expect("pane 2 exists").geometry(),
        PaneGeometry::new(35, 20, 65, 20)
    );
}

#[test]
fn selecting_a_pane_tracks_active_and_last_pane_per_window() {
    let mut window = Window::new(TerminalSize {
        cols: 120,
        rows: 40,
    });
    window.split_after_position(0);

    assert!(window.select_pane(1));
    assert_eq!(window.active_pane_index(), 1);
    assert_eq!(window.last_pane_index(), Some(0));
}

fn four_pane_tiled_window() -> Window {
    let mut window = Window::new(TerminalSize { cols: 80, rows: 24 });
    window.split_after_position_with_id_and_direction(
        0,
        crate::PaneId::new(1),
        SplitDirection::Vertical,
    );
    window.split_after_position_with_id_and_direction(
        window.pane_position(1).expect("pane 1 exists"),
        crate::PaneId::new(2),
        SplitDirection::Horizontal,
    );
    window.select_pane(0);
    window.split_after_position_with_id_and_direction(
        window.pane_position(0).expect("pane 0 exists"),
        crate::PaneId::new(3),
        SplitDirection::Horizontal,
    );
    window.set_layout(LayoutName::Tiled);
    window
}

#[test]
fn selecting_right_from_right_edge_wraps_to_left_edge_like_tmux() {
    let mut window = four_pane_tiled_window();

    assert_eq!(window.active_pane_index(), 1);
    assert_eq!(
        window.select_adjacent_pane(1, SelectPaneDirection::Right),
        Some(0)
    );
    assert_eq!(window.active_pane_index(), 0);
    assert_eq!(window.last_pane_index(), Some(1));
}

#[test]
fn selecting_from_other_edges_wraps_like_tmux() {
    let mut window = four_pane_tiled_window();
    window.select_pane(0);
    assert_eq!(
        window.select_adjacent_pane(0, SelectPaneDirection::Left),
        Some(1)
    );

    let mut window = four_pane_tiled_window();
    window.select_pane(3);
    assert_eq!(
        window.select_adjacent_pane(3, SelectPaneDirection::Down),
        Some(1)
    );

    let mut window = four_pane_tiled_window();
    window.select_pane(0);
    assert_eq!(
        window.select_adjacent_pane(0, SelectPaneDirection::Up),
        Some(2)
    );
}

#[test]
fn selecting_adjacent_pane_prefers_most_recent_adjacent_pane_like_tmux() {
    let mut window = Window::new(TerminalSize {
        cols: 132,
        rows: 40,
    });
    window.split_after_position_with_id_and_direction(
        0,
        crate::PaneId::new(1),
        SplitDirection::Vertical,
    );
    let right_pane_position = window.pane_position(1).expect("pane 1 exists");
    window.split_after_position_with_id_and_direction(
        right_pane_position,
        crate::PaneId::new(2),
        SplitDirection::Horizontal,
    );

    assert_eq!(window.active_pane_index(), 2);
    assert!(window.toggle_zoom(2));
    assert!(!window.toggle_zoom(2));
    assert_eq!(
        window.select_adjacent_pane(2, SelectPaneDirection::Left),
        Some(0)
    );

    assert_eq!(
        window.select_adjacent_pane(0, SelectPaneDirection::Right),
        Some(2)
    );
    assert_eq!(window.active_pane_index(), 2);
}

#[test]
fn remove_pane_grows_the_previous_sibling_in_place() {
    let mut window = Window::new(TerminalSize {
        cols: 120,
        rows: 40,
    });
    window.split_after_position(0);
    window.split_after_position(1);

    let removed = window.remove_pane(1).expect("pane 1 should be removed");

    assert_eq!(removed.index(), 1);
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(
        window.pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 90, 40)
    );
    assert_eq!(
        window.pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(91, 0, 29, 40)
    );
}

#[test]
fn removing_the_active_pane_prefers_the_last_pane_then_clears_it() {
    let mut window = Window::new(TerminalSize {
        cols: 120,
        rows: 40,
    });
    window.split_after_position(0);
    assert!(window.select_pane(1));
    assert!(window.select_pane(0));

    let removed = window.remove_pane(0).expect("pane 0 should be removed");

    assert_eq!(removed.index(), 0);
    assert_eq!(window.active_pane_index(), 0);
    assert_eq!(window.last_pane_index(), None);
}

#[test]
fn removing_the_last_tracked_pane_clears_last_pane_without_changing_active() {
    let mut window = Window::new(TerminalSize {
        cols: 120,
        rows: 40,
    });
    window.split_after_position(0);
    assert!(window.select_pane(1));
    assert!(window.select_pane(0));

    let removed = window.remove_pane(1).expect("pane 1 should be removed");

    assert_eq!(removed.index(), 1);
    assert_eq!(window.active_pane_index(), 0);
    assert_eq!(window.last_pane_index(), None);
}

#[test]
fn removing_the_highest_index_does_not_allow_reuse_on_a_future_split() {
    let mut window = Window::new(TerminalSize {
        cols: 120,
        rows: 40,
    });
    window.split_after_position(0);
    window.remove_pane(1).expect("pane 1 should be removed");

    let new_index = window.split_after_position(0);

    assert_eq!(new_index, 1);
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[test]
fn rotate_up_moves_first_pane_to_the_tail_and_tracks_previous_active_pane() {
    let mut window = Window::new(TerminalSize {
        cols: 120,
        rows: 40,
    });
    window.split_after_position(0);
    let pane_zero_position = window.pane_position(0).expect("pane 0 exists");
    window.split_after_position(pane_zero_position);

    let previous_geometries = window
        .panes()
        .iter()
        .map(|pane| pane.geometry())
        .collect::<Vec<_>>();
    let previous_pane_ids = window
        .panes()
        .iter()
        .map(|pane| pane.id())
        .collect::<Vec<_>>();

    window.rotate_panes(RotateWindowDirection::Up);

    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.id())
            .collect::<Vec<_>>(),
        vec![
            previous_pane_ids[1],
            previous_pane_ids[2],
            previous_pane_ids[0]
        ]
    );
    assert_eq!(window.active_pane_index(), 1);
    assert_eq!(window.last_pane_index(), Some(0));
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.geometry())
            .collect::<Vec<_>>(),
        previous_geometries
    );
}

#[test]
fn rotate_down_moves_last_pane_to_the_head_and_tracks_previous_active_pane() {
    let mut window = Window::new(TerminalSize {
        cols: 120,
        rows: 40,
    });
    window.split_after_position(0);
    let pane_zero_position = window.pane_position(0).expect("pane 0 exists");
    window.split_after_position(pane_zero_position);
    assert!(window.select_pane(1));

    let previous_geometries = window
        .panes()
        .iter()
        .map(|pane| pane.geometry())
        .collect::<Vec<_>>();
    let previous_pane_ids = window
        .panes()
        .iter()
        .map(|pane| pane.id())
        .collect::<Vec<_>>();

    window.rotate_panes(RotateWindowDirection::Down);

    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.id())
            .collect::<Vec<_>>(),
        vec![
            previous_pane_ids[2],
            previous_pane_ids[0],
            previous_pane_ids[1]
        ]
    );
    assert_eq!(window.active_pane_index(), 1);
    assert_eq!(window.last_pane_index(), Some(2));
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.geometry())
            .collect::<Vec<_>>(),
        previous_geometries
    );
}

#[test]
fn rotate_up_on_a_two_pane_window_swaps_the_panes() {
    let mut window = Window::new(TerminalSize {
        cols: 120,
        rows: 40,
    });
    window.split_after_position(0);

    let previous_geometries = window
        .panes()
        .iter()
        .map(|pane| pane.geometry())
        .collect::<Vec<_>>();

    window.rotate_panes(RotateWindowDirection::Up);

    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(window.active_pane_index(), 1);
    assert_eq!(window.last_pane_index(), Some(0));
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.geometry())
            .collect::<Vec<_>>(),
        previous_geometries
    );
}

#[test]
fn rotate_single_pane_window_is_a_noop() {
    let mut window = Window::new(TerminalSize {
        cols: 120,
        rows: 40,
    });

    let previous_geometry = window.pane(0).expect("pane 0 exists").geometry();

    window.rotate_panes(RotateWindowDirection::Up);

    assert_eq!(window.active_pane_index(), 0);
    assert_eq!(window.last_pane_index(), None);
    assert_eq!(window.pane_count(), 1);
    assert_eq!(
        window.pane(0).expect("pane 0 exists").geometry(),
        previous_geometry
    );
}

#[test]
fn new_window_starts_unnamed_with_automatic_rename_enabled() {
    let window = Window::new(TerminalSize { cols: 80, rows: 24 });

    assert_eq!(window.name(), None);
    assert!(window.automatic_rename());
}

#[test]
fn set_name_persists_the_name_and_disables_automatic_rename() {
    let mut window = Window::new(TerminalSize { cols: 80, rows: 24 });

    window.set_name("logs".to_owned());

    assert_eq!(window.name(), Some("logs"));
    assert!(!window.automatic_rename());
}

#[test]
fn set_automatic_name_preserves_automatic_rename() {
    let mut window = Window::new(TerminalSize { cols: 80, rows: 24 });

    window.set_automatic_name("tmux".to_owned());

    assert_eq!(window.name(), Some("tmux"));
    assert!(window.automatic_rename());
}

#[test]
fn respawn_resets_runtime_state_to_a_single_default_pane() {
    let mut window = Window::new(TerminalSize { cols: 80, rows: 24 });
    let original_id = window.id();
    window.set_name("logs".to_owned());
    window.split_after_position(0);
    window.set_layout(LayoutName::EvenHorizontal);
    window.save_old_layout();
    window.queue_alerts(WINDOW_BELL);
    window.set_alerts_queued(true);
    window.resize_main_pane(ResizePaneAdjustment::AbsoluteWidth { columns: 20 });
    assert!(window.toggle_zoom(1));

    let pane_id = crate::PaneId::new(77);
    let returned = window.respawn(pane_id);

    assert_eq!(returned, pane_id);
    assert_eq!(window.id(), original_id);
    assert_eq!(window.name(), Some("logs"));
    assert!(window.automatic_rename());
    assert_eq!(window.size(), TerminalSize { cols: 80, rows: 24 });
    assert_eq!(window.pane_count(), 1);
    assert_eq!(window.active_pane_index(), 0);
    assert_eq!(window.last_pane_index(), None);
    assert_eq!(window.layout(), LayoutName::MainVertical);
    assert_eq!(window.last_layout, None);
    assert!(!window.custom_layout);
    assert_eq!(window.old_layout(), None);
    assert!(!window.is_zoomed());
    assert!(!window.zoom_restore_pending);
    assert!(window.alert_flags().is_empty());
    assert!(!window.alerts_queued());
    assert_eq!(window.requested_main_width, None);
    assert_eq!(window.pane_id(0), Some(pane_id));
    assert_eq!(
        window.pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 80, 24)
    );
}
