use super::{apply_layout, layout_checksum, LayoutTree};
use crate::{Pane, PaneGeometry};
use rmux_proto::{LayoutName, TerminalSize};

fn pane(index: u32) -> Pane {
    Pane::new(index, PaneGeometry::new(0, 0, 0, 0))
}

fn assert_layout(
    layout: LayoutName,
    size: TerminalSize,
    requested_main_width: Option<u16>,
    expected: Vec<PaneGeometry>,
) {
    let mut panes = (0..expected.len() as u32).map(pane).collect::<Vec<_>>();

    apply_layout(&mut panes, layout, size, requested_main_width);

    assert_eq!(
        panes.iter().map(|pane| pane.geometry()).collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn custom_layout_rejects_excessive_nesting_before_stack_growth() {
    let mut body = "1x1,0,0".to_owned();
    for _ in 0..150 {
        body = format!("1x1,0,0{{{body}}}");
    }
    let layout = format!("{:04x},{body}", layout_checksum(&body));

    let error = LayoutTree::parse(&layout, 1).expect_err("deep layout must be rejected");

    assert!(
        error.to_string().contains("too deeply nested"),
        "unexpected error: {error}"
    );
}

#[test]
fn single_pane_uses_full_geometry_without_border_overhead() {
    assert_layout(
        LayoutName::MainVertical,
        TerminalSize {
            cols: 120,
            rows: 40,
        },
        None,
        vec![PaneGeometry::new(0, 0, 120, 40)],
    );
}

#[test]
fn two_panes_split_columns_with_a_single_border() {
    assert_layout(
        LayoutName::MainVertical,
        TerminalSize {
            cols: 120,
            rows: 40,
        },
        None,
        vec![
            PaneGeometry::new(0, 0, 80, 40),
            PaneGeometry::new(81, 0, 39, 40),
        ],
    );
}

#[test]
fn two_panes_split_rows_with_a_single_border() {
    assert_layout(
        LayoutName::MainHorizontal,
        TerminalSize {
            cols: 120,
            rows: 40,
        },
        Some(999),
        vec![
            PaneGeometry::new(0, 0, 120, 24),
            PaneGeometry::new(0, 25, 120, 15),
        ],
    );
}

#[test]
fn three_panes_spread_the_secondary_column_using_tmux_order() {
    assert_layout(
        LayoutName::MainVertical,
        TerminalSize {
            cols: 100,
            rows: 50,
        },
        Some(34),
        vec![
            PaneGeometry::new(0, 0, 34, 50),
            PaneGeometry::new(35, 0, 65, 24),
            PaneGeometry::new(35, 25, 65, 25),
        ],
    );
}

#[test]
fn three_panes_spread_the_secondary_row_using_tmux_defaults() {
    assert_layout(
        LayoutName::MainHorizontal,
        TerminalSize {
            cols: 100,
            rows: 50,
        },
        None,
        vec![
            PaneGeometry::new(0, 0, 100, 24),
            PaneGeometry::new(0, 25, 49, 25),
            PaneGeometry::new(50, 25, 50, 25),
        ],
    );
}

#[test]
fn remainder_rows_are_distributed_to_the_bottom_in_tmux_secondary_columns() {
    assert_layout(
        LayoutName::MainVertical,
        TerminalSize { cols: 90, rows: 10 },
        Some(30),
        vec![
            PaneGeometry::new(0, 0, 30, 10),
            PaneGeometry::new(31, 0, 59, 2),
            PaneGeometry::new(31, 3, 59, 2),
            PaneGeometry::new(31, 6, 59, 4),
        ],
    );
}

#[test]
fn main_horizontal_preserves_a_minimum_secondary_row_on_small_windows() {
    assert_layout(
        LayoutName::MainHorizontal,
        TerminalSize { cols: 10, rows: 9 },
        None,
        vec![
            PaneGeometry::new(0, 0, 10, 7),
            PaneGeometry::new(0, 8, 2, 1),
            PaneGeometry::new(3, 8, 2, 1),
            PaneGeometry::new(6, 8, 4, 1),
        ],
    );
}

#[test]
fn main_vertical_geometry_case_is_exact() {
    assert_layout(
        LayoutName::MainVertical,
        TerminalSize {
            cols: 200,
            rows: 50,
        },
        Some(34),
        vec![
            PaneGeometry::new(0, 0, 34, 50),
            PaneGeometry::new(35, 0, 165, 24),
            PaneGeometry::new(35, 25, 165, 25),
        ],
    );
}

#[test]
fn oversized_requested_main_width_is_clamped() {
    assert_layout(
        LayoutName::MainVertical,
        TerminalSize { cols: 80, rows: 20 },
        Some(500),
        vec![
            PaneGeometry::new(0, 0, 78, 20),
            PaneGeometry::new(79, 0, 1, 20),
        ],
    );
}

#[test]
fn mirrored_main_vertical_keeps_main_pane_in_large_column() {
    // Pane 0 (the main pane) gets the large right column, matching tmux.
    assert_layout(
        LayoutName::MainVerticalMirrored,
        TerminalSize {
            cols: 120,
            rows: 40,
        },
        Some(34),
        vec![
            PaneGeometry::new(86, 0, 34, 40),
            PaneGeometry::new(0, 0, 85, 19),
            PaneGeometry::new(0, 20, 85, 20),
        ],
    );
}

#[test]
fn mirrored_main_horizontal_keeps_main_pane_in_large_row() {
    // Pane 0 (the main pane) gets the large bottom row, matching tmux.
    assert_layout(
        LayoutName::MainHorizontalMirrored,
        TerminalSize {
            cols: 100,
            rows: 50,
        },
        None,
        vec![
            PaneGeometry::new(0, 26, 100, 24),
            PaneGeometry::new(0, 0, 49, 25),
            PaneGeometry::new(50, 0, 50, 25),
        ],
    );
}

#[test]
fn even_layouts_single_pane_use_full_geometry_without_border_overhead() {
    for layout in [LayoutName::EvenHorizontal, LayoutName::EvenVertical] {
        assert_layout(
            layout,
            TerminalSize {
                cols: 101,
                rows: 41,
            },
            Some(1),
            vec![PaneGeometry::new(0, 0, 101, 41)],
        );
    }
}

#[test]
fn even_horizontal_two_panes_split_columns_with_one_border() {
    assert_layout(
        LayoutName::EvenHorizontal,
        TerminalSize {
            cols: 120,
            rows: 40,
        },
        None,
        vec![
            PaneGeometry::new(0, 0, 59, 40),
            PaneGeometry::new(60, 0, 60, 40),
        ],
    );
}

#[test]
fn even_vertical_two_panes_split_rows_with_one_border() {
    assert_layout(
        LayoutName::EvenVertical,
        TerminalSize {
            cols: 120,
            rows: 40,
        },
        None,
        vec![
            PaneGeometry::new(0, 0, 120, 19),
            PaneGeometry::new(0, 20, 120, 20),
        ],
    );
}

#[test]
fn even_horizontal_three_panes_with_101_columns_has_no_remainder() {
    assert_layout(
        LayoutName::EvenHorizontal,
        TerminalSize {
            cols: 101,
            rows: 40,
        },
        None,
        vec![
            PaneGeometry::new(0, 0, 33, 40),
            PaneGeometry::new(34, 0, 33, 40),
            PaneGeometry::new(68, 0, 33, 40),
        ],
    );
}

#[test]
fn even_horizontal_three_panes_with_100_columns_gives_remainder_to_the_last_pane() {
    assert_layout(
        LayoutName::EvenHorizontal,
        TerminalSize {
            cols: 100,
            rows: 40,
        },
        None,
        vec![
            PaneGeometry::new(0, 0, 32, 40),
            PaneGeometry::new(33, 0, 32, 40),
            PaneGeometry::new(66, 0, 34, 40),
        ],
    );
}

#[test]
fn even_vertical_three_panes_gives_remainder_rows_to_the_last_pane() {
    assert_layout(
        LayoutName::EvenVertical,
        TerminalSize {
            cols: 100,
            rows: 40,
        },
        None,
        vec![
            PaneGeometry::new(0, 0, 100, 12),
            PaneGeometry::new(0, 13, 100, 12),
            PaneGeometry::new(0, 26, 100, 14),
        ],
    );
}

#[test]
fn even_horizontal_four_panes_gives_remainder_to_the_last_pane() {
    assert_layout(
        LayoutName::EvenHorizontal,
        TerminalSize { cols: 80, rows: 20 },
        None,
        vec![
            PaneGeometry::new(0, 0, 19, 20),
            PaneGeometry::new(20, 0, 19, 20),
            PaneGeometry::new(40, 0, 19, 20),
            PaneGeometry::new(60, 0, 20, 20),
        ],
    );
}

#[test]
fn even_vertical_four_panes_gives_remainder_to_the_last_pane() {
    assert_layout(
        LayoutName::EvenVertical,
        TerminalSize { cols: 80, rows: 20 },
        None,
        vec![
            PaneGeometry::new(0, 0, 80, 4),
            PaneGeometry::new(0, 5, 80, 4),
            PaneGeometry::new(0, 10, 80, 4),
            PaneGeometry::new(0, 15, 80, 5),
        ],
    );
}

#[test]
fn even_horizontal_five_panes_keeps_one_cell_separators() {
    assert_layout(
        LayoutName::EvenHorizontal,
        TerminalSize { cols: 12, rows: 6 },
        None,
        vec![
            PaneGeometry::new(0, 0, 1, 6),
            PaneGeometry::new(2, 0, 1, 6),
            PaneGeometry::new(4, 0, 1, 6),
            PaneGeometry::new(6, 0, 1, 6),
            PaneGeometry::new(8, 0, 4, 6),
        ],
    );
}

#[test]
fn even_vertical_five_panes_keeps_one_cell_separators() {
    assert_layout(
        LayoutName::EvenVertical,
        TerminalSize { cols: 6, rows: 12 },
        None,
        vec![
            PaneGeometry::new(0, 0, 6, 1),
            PaneGeometry::new(0, 2, 6, 1),
            PaneGeometry::new(0, 4, 6, 1),
            PaneGeometry::new(0, 6, 6, 1),
            PaneGeometry::new(0, 8, 6, 4),
        ],
    );
}

#[test]
fn even_horizontal_two_panes_minimum_viable_width_gives_each_pane_one_column() {
    // 3 cols = 1 col + 1 border + 1 col — the tightest fit where both panes are visible.
    assert_layout(
        LayoutName::EvenHorizontal,
        TerminalSize { cols: 3, rows: 10 },
        None,
        vec![
            PaneGeometry::new(0, 0, 1, 10),
            PaneGeometry::new(2, 0, 1, 10),
        ],
    );
}

#[test]
fn even_vertical_two_panes_minimum_viable_height_gives_each_pane_one_row() {
    assert_layout(
        LayoutName::EvenVertical,
        TerminalSize { cols: 10, rows: 3 },
        None,
        vec![
            PaneGeometry::new(0, 0, 10, 1),
            PaneGeometry::new(0, 2, 10, 1),
        ],
    );
}

#[test]
fn even_horizontal_degenerate_width_gives_earlier_panes_zero_columns() {
    // 2 cols with 2 panes: usable = 2 - 1 = 1, each = 0, remainder = 1.
    // tmux assigns the remainder to the last pane.
    assert_layout(
        LayoutName::EvenHorizontal,
        TerminalSize { cols: 2, rows: 10 },
        None,
        vec![
            PaneGeometry::new(0, 0, 0, 10),
            PaneGeometry::new(1, 0, 1, 10),
        ],
    );
}

#[test]
fn even_vertical_degenerate_height_gives_earlier_panes_zero_rows() {
    assert_layout(
        LayoutName::EvenVertical,
        TerminalSize { cols: 10, rows: 2 },
        None,
        vec![
            PaneGeometry::new(0, 0, 10, 0),
            PaneGeometry::new(0, 1, 10, 1),
        ],
    );
}

#[test]
fn even_horizontal_ignores_requested_main_width() {
    assert_layout(
        LayoutName::EvenHorizontal,
        TerminalSize { cols: 80, rows: 20 },
        Some(79),
        vec![
            PaneGeometry::new(0, 0, 39, 20),
            PaneGeometry::new(40, 0, 40, 20),
        ],
    );
}

#[test]
fn even_vertical_ignores_requested_main_width() {
    assert_layout(
        LayoutName::EvenVertical,
        TerminalSize { cols: 80, rows: 20 },
        Some(79),
        vec![
            PaneGeometry::new(0, 0, 80, 9),
            PaneGeometry::new(0, 10, 80, 10),
        ],
    );
}
