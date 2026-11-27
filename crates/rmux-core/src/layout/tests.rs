use super::{apply_layout, layout_checksum, LayoutOptions, LayoutTree};
use crate::{Pane, PaneGeometry};
use rmux_proto::{LayoutName, TerminalSize};

fn pane(index: u32) -> Pane {
    Pane::new(index, PaneGeometry::new(0, 0, 0, 0))
}

fn layout_geometries(
    layout: LayoutName,
    pane_count: usize,
    size: TerminalSize,
    requested_main_width: Option<u16>,
) -> Vec<PaneGeometry> {
    let mut panes = (0..pane_count as u32).map(pane).collect::<Vec<_>>();
    apply_layout(&mut panes, layout, size, requested_main_width);
    panes.iter().map(|pane| pane.geometry()).collect()
}

fn assert_layout(
    layout: LayoutName,
    size: TerminalSize,
    requested_main_width: Option<u16>,
    expected: Vec<PaneGeometry>,
) {
    assert_eq!(
        layout_geometries(layout, expected.len(), size, requested_main_width),
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

    let error = LayoutTree::parse(&layout, &[crate::PaneId::new(0)])
        .expect_err("deep layout must be rejected");

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
            PaneGeometry::new(35, 0, 65, 25),
            PaneGeometry::new(35, 26, 65, 24),
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
            PaneGeometry::new(0, 25, 50, 25),
            PaneGeometry::new(51, 25, 49, 25),
        ],
    );
}

#[test]
fn remainder_rows_are_distributed_from_the_top_in_tmux_secondary_columns() {
    assert_layout(
        LayoutName::MainVertical,
        TerminalSize { cols: 90, rows: 10 },
        Some(30),
        vec![
            PaneGeometry::new(0, 0, 30, 10),
            PaneGeometry::new(31, 0, 59, 3),
            PaneGeometry::new(31, 4, 59, 3),
            PaneGeometry::new(31, 8, 59, 2),
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
            PaneGeometry::new(0, 8, 3, 1),
            PaneGeometry::new(4, 8, 3, 1),
            PaneGeometry::new(8, 8, 2, 1),
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
            PaneGeometry::new(35, 0, 165, 25),
            PaneGeometry::new(35, 26, 165, 24),
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
            PaneGeometry::new(0, 0, 85, 20),
            PaneGeometry::new(0, 21, 85, 19),
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
            PaneGeometry::new(0, 0, 50, 25),
            PaneGeometry::new(51, 0, 49, 25),
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
            PaneGeometry::new(0, 0, 60, 40),
            PaneGeometry::new(61, 0, 59, 40),
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
            PaneGeometry::new(0, 0, 120, 20),
            PaneGeometry::new(0, 21, 120, 19),
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
fn even_horizontal_three_panes_with_100_columns_spreads_remainder_from_the_first_pane() {
    assert_layout(
        LayoutName::EvenHorizontal,
        TerminalSize {
            cols: 100,
            rows: 40,
        },
        None,
        vec![
            PaneGeometry::new(0, 0, 33, 40),
            PaneGeometry::new(34, 0, 33, 40),
            PaneGeometry::new(68, 0, 32, 40),
        ],
    );
}

#[test]
fn even_vertical_three_panes_spreads_remainder_rows_from_the_first_pane() {
    assert_layout(
        LayoutName::EvenVertical,
        TerminalSize {
            cols: 100,
            rows: 40,
        },
        None,
        vec![
            PaneGeometry::new(0, 0, 100, 13),
            PaneGeometry::new(0, 14, 100, 13),
            PaneGeometry::new(0, 28, 100, 12),
        ],
    );
}

#[test]
fn even_horizontal_four_panes_gives_remainder_to_the_first_pane() {
    assert_layout(
        LayoutName::EvenHorizontal,
        TerminalSize { cols: 80, rows: 20 },
        None,
        vec![
            PaneGeometry::new(0, 0, 20, 20),
            PaneGeometry::new(21, 0, 19, 20),
            PaneGeometry::new(41, 0, 19, 20),
            PaneGeometry::new(61, 0, 19, 20),
        ],
    );
}

#[test]
fn even_vertical_four_panes_gives_remainder_to_the_first_pane() {
    assert_layout(
        LayoutName::EvenVertical,
        TerminalSize { cols: 80, rows: 20 },
        None,
        vec![
            PaneGeometry::new(0, 0, 80, 5),
            PaneGeometry::new(0, 6, 80, 4),
            PaneGeometry::new(0, 11, 80, 4),
            PaneGeometry::new(0, 16, 80, 4),
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
            PaneGeometry::new(0, 0, 2, 6),
            PaneGeometry::new(3, 0, 2, 6),
            PaneGeometry::new(6, 0, 2, 6),
            PaneGeometry::new(9, 0, 1, 6),
            PaneGeometry::new(11, 0, 1, 6),
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
            PaneGeometry::new(0, 0, 6, 2),
            PaneGeometry::new(0, 3, 6, 2),
            PaneGeometry::new(0, 6, 6, 2),
            PaneGeometry::new(0, 9, 6, 1),
            PaneGeometry::new(0, 11, 6, 1),
        ],
    );
}

#[test]
fn named_layout_remainders_follow_tmux_spatial_order_for_two_to_six_panes() {
    fn expected_sizes(count: usize, remainder: usize) -> Vec<u16> {
        (0..count)
            .map(|index| 5 + u16::from(index < remainder))
            .collect()
    }

    for pane_count in 2_usize..=6 {
        for remainder in 0..pane_count {
            let total = (5 * pane_count + (pane_count - 1) + remainder) as u16;
            let expected = expected_sizes(pane_count, remainder);

            let horizontal = layout_geometries(
                LayoutName::EvenHorizontal,
                pane_count,
                TerminalSize {
                    cols: total,
                    rows: 40,
                },
                None,
            );
            assert_eq!(
                horizontal
                    .iter()
                    .map(PaneGeometry::cols)
                    .collect::<Vec<_>>(),
                expected,
                "even-horizontal pane_count={pane_count} remainder={remainder}"
            );

            let vertical = layout_geometries(
                LayoutName::EvenVertical,
                pane_count,
                TerminalSize {
                    cols: 80,
                    rows: total,
                },
                None,
            );
            assert_eq!(
                vertical.iter().map(PaneGeometry::rows).collect::<Vec<_>>(),
                expected,
                "even-vertical pane_count={pane_count} remainder={remainder}"
            );
        }

        let secondary_count = pane_count - 1;
        for remainder in 0..secondary_count {
            let total = (5 * secondary_count + (secondary_count - 1) + remainder) as u16;
            let expected = expected_sizes(secondary_count, remainder);

            for layout in [
                LayoutName::MainHorizontal,
                LayoutName::MainHorizontalMirrored,
            ] {
                let geometries = layout_geometries(
                    layout,
                    pane_count,
                    TerminalSize {
                        cols: total,
                        rows: 50,
                    },
                    None,
                );
                assert_eq!(
                    geometries
                        .iter()
                        .skip(1)
                        .map(PaneGeometry::cols)
                        .collect::<Vec<_>>(),
                    expected,
                    "{layout} pane_count={pane_count} remainder={remainder}"
                );
            }

            for layout in [LayoutName::MainVertical, LayoutName::MainVerticalMirrored] {
                let geometries = layout_geometries(
                    layout,
                    pane_count,
                    TerminalSize {
                        cols: 100,
                        rows: total,
                    },
                    Some(34),
                );
                assert_eq!(
                    geometries
                        .iter()
                        .skip(1)
                        .map(PaneGeometry::rows)
                        .collect::<Vec<_>>(),
                    expected,
                    "{layout} pane_count={pane_count} remainder={remainder}"
                );
            }
        }
    }
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
fn even_horizontal_undersized_width_clamps_to_the_viable_minimum() {
    assert_layout(
        LayoutName::EvenHorizontal,
        TerminalSize { cols: 2, rows: 10 },
        None,
        vec![
            PaneGeometry::new(0, 0, 1, 10),
            PaneGeometry::new(2, 0, 1, 10),
        ],
    );
}

#[test]
fn even_vertical_undersized_height_clamps_to_the_viable_minimum() {
    assert_layout(
        LayoutName::EvenVertical,
        TerminalSize { cols: 10, rows: 2 },
        None,
        vec![
            PaneGeometry::new(0, 0, 10, 1),
            PaneGeometry::new(0, 2, 10, 1),
        ],
    );
}

#[test]
fn all_named_layouts_clamp_impossible_geometry_to_nonzero_valid_trees() {
    for layout in [
        LayoutName::EvenHorizontal,
        LayoutName::EvenVertical,
        LayoutName::MainHorizontal,
        LayoutName::MainHorizontalMirrored,
        LayoutName::MainVertical,
        LayoutName::MainVerticalMirrored,
        LayoutName::Tiled,
    ] {
        for pane_count in 2..=6 {
            let tree = LayoutTree::named(
                layout,
                &(0..pane_count as u32)
                    .map(crate::PaneId::new)
                    .collect::<Vec<_>>(),
                TerminalSize { cols: 1, rows: 1 },
                LayoutOptions::default(),
            );
            let mut panes = (0..pane_count as u32).map(pane).collect::<Vec<_>>();
            tree.apply_to_panes(&mut panes);

            assert!(tree.root.check(), "layout={layout} pane_count={pane_count}");
            assert!(
                panes.iter().all(|pane| {
                    let geometry = pane.geometry();
                    geometry.cols() >= 1 && geometry.rows() >= 1
                }),
                "layout={layout} pane_count={pane_count} panes={panes:?}"
            );
        }
    }
}

#[test]
fn even_horizontal_ignores_requested_main_width() {
    assert_layout(
        LayoutName::EvenHorizontal,
        TerminalSize { cols: 80, rows: 20 },
        Some(79),
        vec![
            PaneGeometry::new(0, 0, 40, 20),
            PaneGeometry::new(41, 0, 39, 20),
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
            PaneGeometry::new(0, 0, 80, 10),
            PaneGeometry::new(0, 11, 80, 9),
        ],
    );
}
