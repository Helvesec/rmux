use rmux_proto::LayoutName;

use super::Window;

/// The order of tmux 3.7b `layout_sets` (`layout-set.c`), which
/// `next-layout` and `previous-layout` walk, mirrored variants included.
const LAYOUT_CYCLE: [LayoutName; 7] = [
    LayoutName::EvenHorizontal,
    LayoutName::EvenVertical,
    LayoutName::MainHorizontal,
    LayoutName::MainHorizontalMirrored,
    LayoutName::MainVertical,
    LayoutName::MainVerticalMirrored,
    LayoutName::Tiled,
];

impl Window {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn set_layout(&mut self, layout: LayoutName) {
        self.set_layout_with_main_pane_size(layout, None, None);
    }

    pub(crate) fn set_layout_with_main_pane_size(
        &mut self,
        layout: LayoutName,
        main_width: Option<u16>,
        main_height: Option<u16>,
    ) {
        self.set_layout_with_tiled_max_columns(layout, main_width, main_height, None);
    }

    /// Applies a named layout, keeping the `tiled-layout-max-columns` bound the
    /// caller resolved so later rebuilds of this window reuse it.
    ///
    /// `tiled_max_columns` is `None` for no bound, which is what the option's
    /// `0` means and what every unbounded caller passes.
    pub(crate) fn set_layout_with_tiled_max_columns(
        &mut self,
        layout: LayoutName,
        main_width: Option<u16>,
        main_height: Option<u16>,
        tiled_max_columns: Option<u16>,
    ) {
        self.auto_unzoom();
        self.layout = layout;
        self.last_layout = Some(layout);
        self.requested_main_width = main_width;
        self.requested_main_height = main_height;
        self.tiled_max_columns = tiled_max_columns;
        self.rebuild_named_layout_tree(layout);
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn next_layout(&mut self) -> LayoutName {
        self.next_layout_with_main_pane_size(None, None)
    }

    pub(crate) fn next_layout_with_main_pane_size(
        &mut self,
        main_width: Option<u16>,
        main_height: Option<u16>,
    ) -> LayoutName {
        self.next_layout_with_tiled_max_columns(main_width, main_height, None)
    }

    /// Selects the next layout of the cycle, carrying the resolved bound.
    pub(crate) fn next_layout_with_tiled_max_columns(
        &mut self,
        main_width: Option<u16>,
        main_height: Option<u16>,
        tiled_max_columns: Option<u16>,
    ) -> LayoutName {
        let layout = next_cycle_layout(self.last_layout);
        self.set_layout_with_tiled_max_columns(layout, main_width, main_height, tiled_max_columns);
        layout
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn previous_layout(&mut self) -> LayoutName {
        self.previous_layout_with_main_pane_size(None, None)
    }

    pub(crate) fn previous_layout_with_main_pane_size(
        &mut self,
        main_width: Option<u16>,
        main_height: Option<u16>,
    ) -> LayoutName {
        self.previous_layout_with_tiled_max_columns(main_width, main_height, None)
    }

    /// Selects the previous layout of the cycle, carrying the resolved bound.
    pub(crate) fn previous_layout_with_tiled_max_columns(
        &mut self,
        main_width: Option<u16>,
        main_height: Option<u16>,
        tiled_max_columns: Option<u16>,
    ) -> LayoutName {
        let layout = previous_cycle_layout(self.last_layout);
        self.set_layout_with_tiled_max_columns(layout, main_width, main_height, tiled_max_columns);
        layout
    }

    pub(crate) fn recalculate_geometry(&mut self) {
        self.reconcile_borrowed_pane_owners();
        if let Some(tree) = &mut self.layout_tree {
            tree.resize(self.size);
            self.size = tree.size();
            tree.apply_to_matching_panes(&mut self.panes);
        }
        self.apply_floating_stack_to_panes();
    }
}

fn next_cycle_layout(last_layout: Option<LayoutName>) -> LayoutName {
    match last_layout {
        None => LAYOUT_CYCLE[0],
        Some(layout) => {
            let position = cycle_position(layout);
            LAYOUT_CYCLE[(position + 1) % LAYOUT_CYCLE.len()]
        }
    }
}

fn previous_cycle_layout(last_layout: Option<LayoutName>) -> LayoutName {
    match last_layout {
        None => *LAYOUT_CYCLE.last().expect("layout cycle is non-empty"),
        Some(layout) => {
            let position = cycle_position(layout);
            LAYOUT_CYCLE[(position + LAYOUT_CYCLE.len() - 1) % LAYOUT_CYCLE.len()]
        }
    }
}

/// Index of `layout` in [`LAYOUT_CYCLE`]. The match is exhaustive so a new
/// `LayoutName` variant must be placed in the cycle before it compiles.
fn cycle_position(layout: LayoutName) -> usize {
    match layout {
        LayoutName::EvenHorizontal => 0,
        LayoutName::EvenVertical => 1,
        LayoutName::MainHorizontal => 2,
        LayoutName::MainHorizontalMirrored => 3,
        LayoutName::MainVertical => 4,
        LayoutName::MainVerticalMirrored => 5,
        LayoutName::Tiled => 6,
    }
}

#[cfg(test)]
mod tests {
    use rmux_proto::{LayoutName, ResizePaneAdjustment, SplitDirection, TerminalSize};

    use crate::{PaneGeometry, PaneId};

    use super::Window;

    fn window_with_two_panes() -> Window {
        let mut window = Window::new(TerminalSize { cols: 80, rows: 24 });
        window.split_after_position(0);
        window
    }

    #[test]
    fn set_size_reconciles_undersized_horizontal_tree_and_is_idempotent() {
        let mut window = Window::new(TerminalSize { cols: 3, rows: 10 });
        window.split_after_position(0);

        for _ in 0..2 {
            window.set_size(TerminalSize { cols: 2, rows: 1 });

            assert_eq!(window.size(), TerminalSize { cols: 3, rows: 1 });
            assert_eq!(
                window.pane(0).expect("pane 0 exists").geometry(),
                PaneGeometry::new(0, 0, 1, 1)
            );
            assert_eq!(
                window.pane(1).expect("pane 1 exists").geometry(),
                PaneGeometry::new(2, 0, 1, 1)
            );
        }
    }

    #[test]
    fn set_size_reconciles_undersized_vertical_tree_and_is_idempotent() {
        let mut window = Window::new(TerminalSize { cols: 10, rows: 3 });
        window.split_after_position_with_id_and_direction(
            0,
            PaneId::new(1),
            SplitDirection::Horizontal,
        );

        for _ in 0..2 {
            window.set_size(TerminalSize { cols: 1, rows: 2 });

            assert_eq!(window.size(), TerminalSize { cols: 1, rows: 3 });
            assert_eq!(
                window.pane(0).expect("pane 0 exists").geometry(),
                PaneGeometry::new(0, 0, 1, 1)
            );
            assert_eq!(
                window.pane(1).expect("pane 1 exists").geometry(),
                PaneGeometry::new(0, 2, 1, 1)
            );
        }
    }

    #[test]
    fn named_layout_expands_the_deficient_axis_and_is_idempotent() {
        let mut window = Window::new(TerminalSize { cols: 3, rows: 1 });
        window.split_after_position(0);

        for _ in 0..2 {
            window.set_layout(LayoutName::EvenVertical);

            assert_eq!(window.size(), TerminalSize { cols: 3, rows: 3 });
            assert_eq!(
                window.pane(0).expect("pane 0 exists").geometry(),
                PaneGeometry::new(0, 0, 3, 1)
            );
            assert_eq!(
                window.pane(1).expect("pane 1 exists").geometry(),
                PaneGeometry::new(0, 2, 3, 1)
            );
        }
    }

    #[test]
    fn next_layout_from_tiled_wraps_to_even_horizontal() {
        let mut window = window_with_two_panes();

        window.set_layout(LayoutName::Tiled);

        assert_eq!(window.next_layout(), LayoutName::EvenHorizontal);
        assert_eq!(window.layout(), LayoutName::EvenHorizontal);
    }

    #[test]
    fn previous_layout_from_even_horizontal_wraps_to_tiled() {
        let mut window = window_with_two_panes();

        window.set_layout(LayoutName::EvenHorizontal);

        assert_eq!(window.previous_layout(), LayoutName::Tiled);
        assert_eq!(window.layout(), LayoutName::Tiled);
    }

    /// tmux 3.7b oracle: 80x24, two `split-window`, then eight `next-layout`
    /// visit the seven `layout_sets`, mirrored variants included, and the
    /// eighth step wraps to even-horizontal.
    #[test]
    fn next_layout_walks_all_seven_tmux_layouts_and_wraps_on_the_eighth_step() {
        let mut window = window_with_two_panes();
        window.split_after_position(1);

        for (step, expected) in [
            LayoutName::EvenHorizontal,
            LayoutName::EvenVertical,
            LayoutName::MainHorizontal,
            LayoutName::MainHorizontalMirrored,
            LayoutName::MainVertical,
            LayoutName::MainVerticalMirrored,
            LayoutName::Tiled,
            LayoutName::EvenHorizontal,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                window.next_layout(),
                expected,
                "next-layout step {}",
                step + 1
            );
            assert_eq!(window.layout(), expected);
        }
    }

    /// tmux 3.7b oracle: `previous-layout` walks the same seven layouts
    /// backward, mirrored variants included, and wraps to tiled.
    #[test]
    fn previous_layout_walks_all_seven_tmux_layouts_backward_and_wraps() {
        let mut window = window_with_two_panes();
        window.split_after_position(1);
        window.set_layout(LayoutName::EvenHorizontal);

        for (step, expected) in [
            LayoutName::Tiled,
            LayoutName::MainVerticalMirrored,
            LayoutName::MainVertical,
            LayoutName::MainHorizontalMirrored,
            LayoutName::MainHorizontal,
            LayoutName::EvenVertical,
            LayoutName::EvenHorizontal,
            LayoutName::Tiled,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                window.previous_layout(),
                expected,
                "previous-layout step {}",
                step + 1
            );
            assert_eq!(window.layout(), expected);
        }
    }

    #[test]
    fn cycle_position_indexes_every_layout_of_the_cycle() {
        for (index, layout) in super::LAYOUT_CYCLE.into_iter().enumerate() {
            assert_eq!(super::cycle_position(layout), index, "{layout}");
        }
    }

    #[test]
    fn split_layout_does_not_move_the_lastlayout_cycle_cursor() {
        let mut window = Window::new(TerminalSize { cols: 80, rows: 24 });
        assert_eq!(window.next_layout(), LayoutName::EvenHorizontal);

        window.split_after_position_with_id_and_direction(
            0,
            PaneId::new(1),
            SplitDirection::Horizontal,
        );

        assert_eq!(window.layout(), LayoutName::MainHorizontal);
        assert_eq!(window.next_layout(), LayoutName::EvenVertical);
    }

    #[test]
    fn selecting_named_layout_resets_resized_split_tree_main_size_like_tmux() {
        let mut window = Window::new(TerminalSize {
            cols: 120,
            rows: 35,
        });
        window.split_after_position_with_id_and_direction(
            0,
            PaneId::new(1),
            SplitDirection::Vertical,
        );
        window.split_after_position_with_id_and_direction(
            0,
            PaneId::new(2),
            SplitDirection::Horizontal,
        );
        window.split_after_position_with_id_and_direction(
            1,
            PaneId::new(3),
            SplitDirection::Vertical,
        );
        window.select_pane(2);

        assert!(window.resize_pane_by(2, ResizePaneAdjustment::Right { cells: 7 }));
        assert_eq!(window.pane(0).expect("pane exists").geometry().cols(), 60);

        window.set_layout(LayoutName::MainHorizontalMirrored);
        assert_eq!(window.next_layout(), LayoutName::MainVertical);

        assert_eq!(window.pane(0).expect("pane exists").geometry().cols(), 80);
    }

    #[test]
    fn next_layout_auto_unzooms_before_recalculating() {
        let mut window = window_with_two_panes();
        window.set_layout(LayoutName::Tiled);
        assert!(window.toggle_zoom(1));

        assert_eq!(window.next_layout(), LayoutName::EvenHorizontal);

        assert!(!window.is_zoomed());
        assert_eq!(
            window.pane(1).expect("pane 1 exists").geometry(),
            PaneGeometry::new(41, 0, 39, 24)
        );
    }

    #[test]
    fn previous_layout_auto_unzooms_before_recalculating() {
        let mut window = window_with_two_panes();
        window.set_layout(LayoutName::EvenHorizontal);
        assert!(window.toggle_zoom(1));

        assert_eq!(window.previous_layout(), LayoutName::Tiled);

        assert!(!window.is_zoomed());
        assert_eq!(
            window.pane(1).expect("pane 1 exists").geometry(),
            PaneGeometry::new(0, 12, 80, 12)
        );
    }
}
