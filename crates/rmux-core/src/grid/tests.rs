use super::*;
use crate::input::{CellState, GridAttr};

#[test]
fn render_without_trimming_preserves_explicit_trailing_spaces_but_not_cleared_cells() {
    let mut line = GridLine::new(6);
    let state = CellState::default();
    for (x, ch) in "A  ".chars().enumerate() {
        *line.cell_mut(x as u32).expect("cell exists") =
            GridCell::from_state(ch, 1, &state, GridCellFlags::default());
    }

    let mut render_state = GridStringState::default();
    let rendered = line.render_with_options(
        6,
        GridRenderOptions {
            trim_spaces: false,
            include_empty_cells: false,
            ..GridRenderOptions::default()
        },
        &mut render_state,
        None,
    );
    assert_eq!(rendered, "A  ");

    let mut render_state = GridStringState::default();
    let trimmed = line.render_with_options(
        6,
        GridRenderOptions {
            trim_spaces: true,
            include_empty_cells: false,
            ..GridRenderOptions::default()
        },
        &mut render_state,
        None,
    );
    assert_eq!(trimmed, "A");
}

#[test]
fn render_with_tmux_cell_capacity_stops_at_allocation_bucket() {
    let mut line = GridLine::new(20);
    let state = CellState::default();
    for (x, ch) in "abcde".chars().enumerate() {
        *line.cell_mut(x as u32).expect("cell exists") =
            GridCell::from_state(ch, 1, &state, GridCellFlags::default());
    }

    let mut render_state = GridStringState::default();
    let rendered = line.render_with_options(
        20,
        GridRenderOptions {
            trim_spaces: false,
            include_empty_cells: true,
            use_tmux_cell_capacity: true,
            ..GridRenderOptions::default()
        },
        &mut render_state,
        None,
    );
    assert_eq!(rendered, "abcde     ");

    let mut styled_line = GridLine::new(20);
    let mut styled = CellState::default();
    styled.cell.attr = GridAttr::BRIGHT;
    for (x, ch) in "red".chars().enumerate() {
        *styled_line.cell_mut(x as u32).expect("cell exists") =
            GridCell::from_state(ch, 1, &styled, GridCellFlags::default());
    }
    for (x, ch) in "  ".chars().enumerate() {
        *styled_line.cell_mut((x + 3) as u32).expect("cell exists") =
            GridCell::from_state(ch, 1, &state, GridCellFlags::default());
    }
    let mut render_state = GridStringState::default();
    let rendered = styled_line.render_with_options(
        20,
        GridRenderOptions {
            trim_spaces: false,
            include_empty_cells: true,
            use_tmux_cell_capacity: true,
            ..GridRenderOptions::default()
        },
        &mut render_state,
        None,
    );
    assert_eq!(rendered, "red  ");

    let empty = GridLine::new(20);
    let mut render_state = GridStringState::default();
    let rendered = empty.render_with_options(
        20,
        GridRenderOptions {
            trim_spaces: false,
            include_empty_cells: true,
            use_tmux_cell_capacity: true,
            ..GridRenderOptions::default()
        },
        &mut render_state,
        None,
    );
    assert_eq!(rendered, "");
}

#[test]
fn render_with_tmux_cell_capacity_ignores_wide_padding_when_bucketing() {
    let mut line = GridLine::new(12);
    let state = CellState::default();

    *line.cell_mut(0).expect("wide cell exists") =
        GridCell::from_state('🙂', 2, &state, GridCellFlags::default());
    *line.cell_mut(1).expect("padding cell exists") =
        GridCell::from_state(' ', 1, &state, GridCellFlags::PADDING);
    *line.cell_mut(2).expect("narrow cell exists") =
        GridCell::from_state('x', 1, &state, GridCellFlags::default());

    let mut render_state = GridStringState::default();
    let rendered = line.render_with_options(
        12,
        GridRenderOptions {
            trim_spaces: false,
            include_empty_cells: true,
            use_tmux_cell_capacity: true,
            ..GridRenderOptions::default()
        },
        &mut render_state,
        None,
    );

    assert_eq!(rendered, "🙂x");
}

#[test]
fn compacted_history_line_renders_against_logical_width() {
    let mut line = GridLine::new(12);
    let state = CellState::default();
    for (x, ch) in "abc".chars().enumerate() {
        *line.cell_mut(x as u32).expect("cell exists") =
            GridCell::from_state(ch, 1, &state, GridCellFlags::default());
    }

    line.compact_for_history();
    assert_eq!(line.cells().len(), 0);
    assert_eq!(line.plain_text(), Some("abc"));

    let mut render_state = GridStringState::default();
    let rendered = line.render_with_options(
        12,
        GridRenderOptions {
            trim_spaces: false,
            include_empty_cells: true,
            ..GridRenderOptions::default()
        },
        &mut render_state,
        None,
    );
    assert_eq!(rendered, "abc         ");
}

#[test]
fn compact_plain_line_resets_carried_sgr_state_when_rendering_sequences() {
    let mut styled_line = GridLine::new(8);
    let mut styled = CellState::default();
    styled.cell.attr = GridAttr::BRIGHT;
    for (x, ch) in "red".chars().enumerate() {
        *styled_line.cell_mut(x as u32).expect("cell exists") =
            GridCell::from_state(ch, 1, &styled, GridCellFlags::default());
    }
    let mut plain_line = GridLine::new(8);
    assert!(plain_line.write_plain_ascii_run(0, b"ok"));

    let mut render_state = GridStringState::default();
    let _ = styled_line.render_with_options(
        8,
        GridRenderOptions {
            with_sequences: true,
            include_empty_cells: false,
            ..GridRenderOptions::default()
        },
        &mut render_state,
        None,
    );
    let rendered = plain_line.render_with_options(
        8,
        GridRenderOptions {
            with_sequences: true,
            include_empty_cells: false,
            ..GridRenderOptions::default()
        },
        &mut render_state,
        None,
    );

    assert!(
        rendered.starts_with("\x1b[0m"),
        "plain compact line must reset carried SGR state: {rendered:?}"
    );
    assert!(rendered.ends_with("ok"));
}

#[test]
fn compacted_wrapped_history_line_keeps_wrap_without_cell_storage() {
    let mut line = GridLine::new(6);
    let state = CellState::default();
    for (x, ch) in "abcdef".chars().enumerate() {
        *line.cell_mut(x as u32).expect("cell exists") =
            GridCell::from_state(ch, 1, &state, GridCellFlags::default());
    }
    line.set_wrapped(true);

    line.compact_for_history();

    assert_eq!(line.cells().len(), 0);
    assert_eq!(line.plain_text(), Some("abcdef"));
    assert!(line.flags().contains(GridLineFlags::WRAPPED));

    let mut render_state = GridStringState::default();
    let rendered = line.render_with_options(
        6,
        GridRenderOptions {
            trim_spaces: false,
            include_empty_cells: true,
            ..GridRenderOptions::default()
        },
        &mut render_state,
        None,
    );
    assert_eq!(rendered, "abcdef");
}

#[test]
fn capture_join_wrapped_keeps_spaces_at_wrapped_boundaries() {
    let mut grid = Grid::new(TerminalSize { cols: 6, rows: 2 }, 0);
    let state = CellState::default();
    let first = grid.visible_line_mut(0).expect("line exists");
    for (x, ch) in "user ".chars().enumerate() {
        *first.cell_mut(x as u32).expect("cell exists") =
            GridCell::from_state(ch, 1, &state, GridCellFlags::default());
    }
    first.set_wrapped(true);
    let second = grid.visible_line_mut(1).expect("line exists");
    for (x, ch) in "root".chars().enumerate() {
        *second.cell_mut(x as u32).expect("cell exists") =
            GridCell::from_state(ch, 1, &state, GridCellFlags::default());
    }

    let mut render_state = GridStringState::default();
    let mut output = Vec::new();
    for absolute_y in 0..2 {
        let line = grid
            .render_absolute_line(
                absolute_y,
                GridRenderOptions {
                    join_wrapped: true,
                    trim_spaces: false,
                    include_empty_cells: false,
                    ..GridRenderOptions::default()
                },
                &mut render_state,
                None,
            )
            .expect("line renders");
        output.extend_from_slice(line.as_bytes());
        if !grid.absolute_line_wrapped(absolute_y).unwrap_or(false) {
            output.push(b'\n');
        }
    }

    assert_eq!(String::from_utf8(output).expect("utf8"), "user root\n");
}

#[test]
fn clear_visible_to_history_preserves_inner_blank_lines() {
    let mut grid = Grid::new(TerminalSize { cols: 8, rows: 4 }, 10);
    let state = CellState::default();
    for (y, text) in [(0, "AAA"), (2, "BBB")] {
        let line = grid.visible_line_mut(y).expect("line exists");
        for (x, ch) in text.chars().enumerate() {
            *line.cell_mut(x as u32).expect("cell exists") =
                GridCell::from_state(ch, 1, &state, GridCellFlags::default());
        }
    }

    grid.clear_visible_to_history(COLOUR_DEFAULT);

    let mut render_state = GridStringState::default();
    let lines = (0..3)
        .map(|absolute_y| {
            grid.render_absolute_line(
                absolute_y,
                GridRenderOptions::default(),
                &mut render_state,
                None,
            )
            .expect("history line renders")
        })
        .collect::<Vec<_>>();
    assert_eq!(lines, ["AAA", "", "BBB"]);
}

fn slow_history_byte_size(grid: &Grid) -> usize {
    grid.history
        .iter()
        .map(|line| line.render_text().len() + 1)
        .sum()
}

fn assert_history_byte_cache(grid: &Grid) {
    assert_eq!(grid.history_byte_size(), slow_history_byte_size(grid));
}

fn write_visible_line(grid: &mut Grid, row: u32, text: &str, wrapped: bool) {
    let line = grid.visible_line_mut(row).expect("visible line exists");
    line.clear(COLOUR_DEFAULT);
    let state = CellState::default();
    for (x, ch) in text.chars().enumerate() {
        *line
            .cell_mut(u32::try_from(x).expect("test column fits u32"))
            .expect("test text fits line") =
            GridCell::from_state(ch, 1, &state, GridCellFlags::default());
    }
    line.set_wrapped(wrapped);
}

#[test]
fn rendered_text_len_matches_rendered_text_without_allocating() {
    let mut line = GridLine::new(8);
    let state = CellState::default();
    *line.cell_mut(0).expect("cell exists") =
        GridCell::from_state('é', 1, &state, GridCellFlags::default());
    *line.cell_mut(1).expect("cell exists") =
        GridCell::from_state(' ', 1, &state, GridCellFlags::default());
    *line.cell_mut(2).expect("cell exists") =
        GridCell::from_state('🙂', 2, &state, GridCellFlags::default());
    *line.cell_mut(3).expect("cell exists") =
        GridCell::from_state(' ', 1, &state, GridCellFlags::PADDING);
    line.cell_mut(4)
        .expect("cell exists")
        .set_text("x  ".to_owned());

    assert_eq!(line.rendered_text_len(), line.render_text().len());
    line.compact_for_history();
    assert_eq!(line.rendered_text_len(), line.render_text().len());
}

#[test]
fn history_byte_cache_tracks_eviction_reflow_removal_and_clear() {
    let mut grid = Grid::new(TerminalSize { cols: 8, rows: 2 }, 8);
    for text in ["one", "two two", "é🙂", "four"] {
        write_visible_line(&mut grid, 0, text, false);
        grid.scroll_region_up(0, 1, COLOUR_DEFAULT, true);
        assert_history_byte_cache(&grid);
    }

    grid.set_hlimit(3);
    assert_history_byte_cache(&grid);
    assert!(grid.remove_absolute_line(1));
    assert_history_byte_cache(&grid);

    let cursor = grid.logical_cursor(0, 0, false);
    let _ = grid.resize_width_remapping_cursor(3, COLOUR_DEFAULT, cursor);
    assert_history_byte_cache(&grid);

    let mut cursor_y = 0;
    grid.resize_height(4, &mut cursor_y, COLOUR_DEFAULT);
    assert_history_byte_cache(&grid);
    grid.resize_height(2, &mut cursor_y, COLOUR_DEFAULT);
    assert_history_byte_cache(&grid);

    let last_history = grid.hsize().saturating_sub(1);
    assert!(grid.truncate_after_absolute_line(last_history));
    assert_history_byte_cache(&grid);
    grid.clear_history();
    assert_history_byte_cache(&grid);
}

#[test]
fn recovery_projection_retains_only_complete_newest_logical_groups() {
    let mut grid = Grid::new(TerminalSize { cols: 8, rows: 1 }, 8);
    for (text, wrapped) in [
        ("old-a", true),
        ("old-b", false),
        ("new-a", true),
        ("new-b", false),
    ] {
        write_visible_line(&mut grid, 0, text, wrapped);
        grid.scroll_region_up(0, 0, COLOUR_DEFAULT, true);
    }
    assert_eq!(grid.hsize(), 4);

    let newest_group_bytes = grid.history[2]
        .recovery_clone_bytes()
        .saturating_add(grid.history[3].recovery_clone_bytes());
    let projection = grid.clone_recovery_projection(newest_group_bytes);
    assert_eq!(projection.hsize(), 2);
    assert_eq!(
        projection
            .history
            .iter()
            .map(GridLine::render_text)
            .collect::<Vec<_>>(),
        ["new-a", "new-b"]
    );
    assert_history_byte_cache(&projection);

    let undersized = grid.clone_recovery_projection(newest_group_bytes.saturating_sub(1));
    assert_eq!(
        undersized.hsize(),
        0,
        "a recovery draft must not retain only part of a wrapped logical group"
    );
    assert_history_byte_cache(&undersized);
}
