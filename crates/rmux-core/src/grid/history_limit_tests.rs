use super::*;

fn scroll_label(grid: &mut Grid, label: usize, lower: u32) {
    let row = grid.visible_line_mut(0).unwrap();
    row.clear(COLOUR_DEFAULT);
    assert!(row.write_plain_ascii_run(0, format!("L{label:03}").as_bytes()));
    grid.scroll_region_up(0, lower, COLOUR_DEFAULT, true);
}

fn history_labels(grid: &Grid) -> Vec<String> {
    grid.history.iter().map(GridLine::render_text).collect()
}

fn assert_bytes(grid: &Grid) {
    assert_eq!(
        grid.history_byte_size(),
        grid.history
            .iter()
            .map(|line| line.render_text().len() + 1)
            .sum::<usize>()
    );
}

#[test]
fn normal_history_collection_matches_tmux_batches_and_small_limits() {
    // Fixed results after 81 transfers from tmux3.7b grid_collect_history(all=0).
    // At20/40 the old one-row policy incorrectly leaves20/40 rows.
    for (limit, first, size) in [
        (0, 81, 0),
        (1, 80, 1),
        (3, 78, 3),
        (9, 72, 9),
        (10, 71, 10),
        (20, 62, 19),
        (30, 51, 30),
        (40, 44, 37),
    ] {
        for lower in [0, 1] {
            let mut grid = Grid::new(TerminalSize { cols: 8, rows: 2 }, limit);
            for label in 0..81 {
                scroll_label(&mut grid, label, lower);
                assert_bytes(&grid);
                assert!(grid.hsize() <= limit);
                assert!(grid.hscrolled() <= grid.hsize());
            }
            assert_eq!(grid.hsize(), size, "limit={limit}, lower={lower}");
            assert_eq!(
                history_labels(&grid),
                (first..81).map(|i| format!("L{i:03}")).collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn collecting_at_thirty_discards_three_before_adding() {
    let mut grid = Grid::new(TerminalSize { cols: 8, rows: 2 }, 30);
    for label in 0..31 {
        scroll_label(&mut grid, label, 1);
    }
    assert_eq!(grid.hsize(), 28);
    assert_eq!(grid.hscrolled(), 28);
    assert_eq!(
        history_labels(&grid),
        (3..31).map(|i| format!("L{i:03}")).collect::<Vec<_>>()
    );
    assert_bytes(&grid);
}

#[test]
fn clear_to_history_uses_the_same_normal_collection_batch() {
    let mut grid = Grid::new(TerminalSize { cols: 8, rows: 2 }, 20);
    for label in 0..20 {
        scroll_label(&mut grid, label, 1);
    }
    assert!(grid
        .visible_line_mut(0)
        .unwrap()
        .write_plain_ascii_run(0, b"clear"));
    grid.clear_visible_to_history(COLOUR_DEFAULT);
    let mut expected: Vec<_> = (2..20).map(|i| format!("L{i:03}")).collect();
    expected.push("clear".to_owned());
    assert_eq!(history_labels(&grid), expected);
    assert_eq!(grid.hsize(), 19);
    assert_bytes(&grid);
}

#[test]
fn height_transfer_preserves_all_rows_at_the_configured_limit() {
    let mut grid = Grid::new(TerminalSize { cols: 8, rows: 3 }, 20);
    for label in 0..20 {
        scroll_label(&mut grid, label, 2);
    }
    assert!(grid
        .visible_line_mut(0)
        .unwrap()
        .write_plain_ascii_run(0, b"resize"));
    let mut cursor_y = 2;
    grid.resize_height(2, &mut cursor_y, COLOUR_DEFAULT);
    // Native tmux016 retains the transferred row above hlimit (21, not20).
    let mut expected: Vec<_> = (0..20).map(|i| format!("L{i:03}")).collect();
    expected.push("resize".to_owned());
    assert_eq!(history_labels(&grid), expected);
    assert_eq!(grid.hsize(), 21);
    assert_eq!(grid.hlimit(), 20);
    assert_bytes(&grid);
}

#[test]
fn existing_grid_limit_mutation_preserves_exact_tail_and_zero_policy() {
    let mut grid = Grid::new(TerminalSize { cols: 8, rows: 2 }, 40);
    for label in 0..37 {
        scroll_label(&mut grid, label, 1);
    }
    let visible = grid.visible.clone();
    grid.set_hlimit(30);
    let tail: Vec<_> = (7..37).map(|i| format!("L{i:03}")).collect();
    assert_eq!(history_labels(&grid), tail);
    assert_eq!(grid.hscrolled(), 30);
    assert_eq!(grid.visible, visible);
    assert_bytes(&grid);
    grid.set_hlimit(40);
    assert_eq!(history_labels(&grid), tail);
    grid.set_hlimit(0);
    assert_eq!(grid.hsize(), 0);
    assert_eq!(grid.hscrolled(), 0);
    assert_bytes(&grid);
    scroll_label(&mut grid, 80, 1);
    assert_eq!(history_labels(&grid), ["L080"]);
    assert_bytes(&grid);
    let mut disabled = Grid::new(TerminalSize { cols: 8, rows: 2 }, 0);
    disabled.set_hlimit(20);
    scroll_label(&mut disabled, 81, 1);
    assert_eq!(disabled.hsize(), 0);
}

fn saturated_height_grid() -> Grid {
    let mut grid = Grid::new(TerminalSize { cols: 80, rows: 12 }, 20);
    for label in 50..70 {
        scroll_label(&mut grid, label, 11);
    }
    for (row, label) in (70..81).enumerate() {
        assert!(grid
            .visible_line_mut(row as u32)
            .unwrap()
            .write_plain_ascii_run(0, format!("L{label:03}").as_bytes()));
    }
    grid
}

fn assert_height_capture(grid: &Grid, first: usize, last: usize) {
    let actual: Vec<_> = grid
        .history
        .iter()
        .chain(grid.visible.iter())
        .map(GridLine::render_text)
        .collect();
    let mut expected: Vec<_> = (first..=last).map(|i| format!("L{i:03}")).collect();
    expected.push(String::new());
    assert_eq!(actual, expected);
    assert_eq!(grid.hlimit(), 20);
    assert!(grid.hscrolled() <= grid.hsize());
    assert_bytes(grid);
}

fn append_height_line(grid: &mut Grid, label: usize) {
    let lower = grid.sy() - 1;
    let row = grid.visible_line_mut(lower).unwrap();
    row.clear(COLOUR_DEFAULT);
    assert!(row.write_plain_ascii_run(0, format!("L{label:03}").as_bytes()));
    grid.scroll_region_up(0, lower, COLOUR_DEFAULT, true);
}

#[test]
fn saturated_height_round_trips_preserve_rows_then_resume_normal_batches() {
    let mut grid = saturated_height_grid();
    let mut cursor_y = 11;
    assert_eq!(grid.hsize(), 20);
    assert_height_capture(&grid, 50, 80);
    for (height, size) in [(11, 21), (12, 20), (8, 24), (12, 20)] {
        let rows_before = grid.history.len() + grid.visible.len();
        grid.resize_height(height, &mut cursor_y, COLOUR_DEFAULT);
        assert_eq!(grid.hsize(), size);
        assert_eq!(grid.hscrolled(), size);
        assert_eq!(cursor_y, height - 1);
        assert_eq!(grid.history.len() + grid.visible.len(), rows_before);
        assert_height_capture(&grid, 50, 80);
    }
    for label in 81..87 {
        append_height_line(&mut grid, label);
        assert_bytes(&grid);
    }
    assert_eq!(grid.hsize(), 20);
    assert_height_capture(&grid, 56, 86);
}

#[test]
fn ordinary_scroll_collects_two_even_while_height_history_exceeds_twenty() {
    let mut grid = saturated_height_grid();
    let mut cursor_y = 11;
    grid.resize_height(8, &mut cursor_y, COLOUR_DEFAULT);
    assert_eq!(grid.hsize(), 24);
    append_height_line(&mut grid, 81);
    assert_eq!(grid.hsize(), 23);
    assert_height_capture(&grid, 52, 81);
    grid.resize_height(12, &mut cursor_y, COLOUR_DEFAULT);
    assert_eq!(grid.hsize(), 19);
    assert_eq!(cursor_y, 11);
    assert_height_capture(&grid, 52, 81);
    append_height_line(&mut grid, 82);
    assert_eq!(grid.hsize(), 20);
    assert_height_capture(&grid, 52, 82);
}

#[test]
fn explicit_limit_mutations_still_trim_height_overflow_and_preserve_hot_zero() {
    let mut grid = saturated_height_grid();
    let mut cursor_y = 11;
    grid.resize_height(8, &mut cursor_y, COLOUR_DEFAULT);
    assert_eq!(grid.hsize(), 24);
    grid.set_hlimit(30);
    assert_eq!(
        history_labels(&grid),
        (50..74).map(|i| format!("L{i:03}")).collect::<Vec<_>>()
    );
    grid.set_hlimit(18);
    assert_eq!(
        history_labels(&grid),
        (56..74).map(|i| format!("L{i:03}")).collect::<Vec<_>>()
    );
    assert_eq!(grid.hscrolled(), 18);
    assert_bytes(&grid);
    grid.set_hlimit(0);
    assert_eq!(grid.hsize(), 0);
    assert_eq!(grid.hscrolled(), 0);
    assert_bytes(&grid);
    append_height_line(&mut grid, 81);
    assert_eq!(history_labels(&grid), ["L074"]);
    append_height_line(&mut grid, 82);
    assert_eq!(history_labels(&grid), ["L075"]);
    assert_bytes(&grid);
}

#[test]
fn option_collection_at_small_limits_matches_tmux_all_one() {
    // tmux grid_collect_history(all=1): greater trims the difference, equal
    // trims one, smaller keeps the rows. These include the native018 equality.
    for (old, new, retained) in [
        (1, 0, 0),
        (1, 1, 0),
        (1, 2, 1),
        (2, 0, 0),
        (2, 1, 1),
        (2, 2, 1),
    ] {
        let mut grid = Grid::new(TerminalSize { cols: 8, rows: 2 }, old);
        for label in 0..4 {
            scroll_label(&mut grid, label, 1);
        }
        let visible = grid.visible.clone();
        assert_eq!(grid.hsize(), old);
        grid.set_hlimit(new);
        assert_eq!(grid.hlimit(), new);
        assert_eq!(grid.hsize(), retained, "old={old}, new={new}");
        assert_eq!(grid.hscrolled(), retained);
        assert_eq!(
            history_labels(&grid),
            (4 - retained..4)
                .map(|i| format!("L{i:03}"))
                .collect::<Vec<_>>()
        );
        assert_eq!(grid.visible, visible);
        assert!(grid.history_enabled());
        assert_bytes(&grid);
    }
}

#[test]
fn repeated_small_limit_collects_only_at_equality_without_enabling_cold_zero() {
    let mut grid = Grid::new(TerminalSize { cols: 8, rows: 2 }, 2);
    scroll_label(&mut grid, 0, 1);
    grid.set_hlimit(2);
    assert_eq!(history_labels(&grid), ["L000"]);
    scroll_label(&mut grid, 1, 1);
    grid.set_hlimit(2);
    assert_eq!(history_labels(&grid), ["L001"]);
    grid.set_hlimit(2);
    assert_eq!(history_labels(&grid), ["L001"]);
    assert_bytes(&grid);
    let mut cold = Grid::new(TerminalSize { cols: 8, rows: 2 }, 0);
    for limit in [0, 1, 2, 1, 0] {
        cold.set_hlimit(limit);
        scroll_label(&mut cold, limit, 1);
        assert!(!cold.history_enabled());
        assert_eq!(cold.hsize(), 0);
        assert_eq!(cold.hscrolled(), 0);
        assert_bytes(&cold);
    }
}
