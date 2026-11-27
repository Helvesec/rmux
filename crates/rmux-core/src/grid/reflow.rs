//! Logical line reflow and physical cursor reconstruction.

use super::{
    shell_marks, Colour, GridCell, GridCellFlags, GridLine, GridLineFlags, GridLogicalCursor,
    GridPhysicalCursor, COLOUR_DEFAULT,
};

pub(super) fn reflow_wrapped_lines_remapping_cursor(
    lines: Vec<GridLine>,
    width: u32,
    bg: Colour,
    cursor: GridLogicalCursor,
) -> (Vec<GridLine>, Option<GridPhysicalCursor>) {
    let mut output = Vec::new();
    let mut logical_cells = Vec::new();
    let mut logical_plain_text: Option<String> = None;
    let mut logical_flags = None;
    let mut logical_start_y = 0_usize;
    let mut mapped_cursor = None;

    let mut shell_marks = shell_marks::ReflowShellMarks::default();

    for (absolute_y, line) in lines.into_iter().enumerate() {
        shell_marks.observe(&line);
        let wrapped = line.flags.contains(GridLineFlags::WRAPPED);
        if logical_flags.is_none()
            && !wrapped
            && line.used_end() == 0
            && line.painted_end() > 0
            && line.trailing_reflow_gap() == 0
        {
            // An erase painted this row but nobody wrote to it: logically empty,
            // yet its colour is real. Re-width it as the one physical row it is.
            // Folding it into a logical line would rebuild it from the resize
            // background, which is the screen default, and splitting it would
            // invent wrapped rows of blanks. New columns follow the existing
            // resize policy rather than inheriting the old paint.
            let mut row = line;
            row.resize_width_preserving_wrap(width, bg);
            let mut rows = vec![row];
            shell_marks.apply(&mut rows);
            if absolute_y == cursor.logical_start_y {
                // The row still is one row, so the cursor keeps its column,
                // bounded by the new width. This mirrors what the empty-line
                // branch of the logical reflow does for the same offset.
                mapped_cursor = Some(GridPhysicalCursor {
                    absolute_y: output.len(),
                    x: u32::try_from(cursor.offset)
                        .unwrap_or(u32::MAX)
                        .min(width.saturating_sub(1)),
                    pending_wrap: false,
                });
            }
            output.extend(rows);
            continue;
        }
        if logical_flags.is_none() {
            logical_start_y = absolute_y;
            let mut flags = line.flags;
            flags.remove(GridLineFlags::WRAPPED);
            flags.remove(GridLineFlags::START_PROMPT);
            flags.remove(GridLineFlags::START_OUTPUT);
            logical_flags = Some(flags);
            logical_plain_text = (bg == COLOUR_DEFAULT).then(String::new);
        }

        let end = if wrapped {
            line.wrapped_end()
        } else {
            // How many cells this row contributes is a question about what it
            // physically holds, not about what counts as written content: a row
            // is `X` followed by an erase-painted suffix just as often as it is
            // wholly painted, and either way the surviving columns must keep
            // their background. The logical bound decides transfer and the
            // resize fast paths; it must not decide this extent, or the paint
            // of every column past the last glyph would be rebuilt from the
            // resize background. A row with no paint has the same value for
            // both bounds, so this is exactly the pre-split behaviour.
            line.painted_end()
        };
        if let (Some(logical_text), Some(text)) = (logical_plain_text.as_mut(), line.plain_text()) {
            logical_text.extend(
                text.bytes()
                    .chain(std::iter::repeat(b' '))
                    .take(end)
                    .map(char::from),
            );
        } else {
            if let Some(text) = logical_plain_text.take() {
                extend_plain_ascii_cells(&mut logical_cells, text.bytes());
            }
            if let Some(text) = line.plain_text() {
                extend_plain_ascii_cells(
                    &mut logical_cells,
                    text.bytes().chain(std::iter::repeat(b' ')).take(end),
                );
            } else {
                logical_cells.extend(
                    line.cells
                        .iter()
                        .take(end)
                        .filter(|cell| !cell.is_padding() && !cell.is_reflow_gap())
                        .cloned(),
                );
            }
        }

        if !wrapped {
            let flags = logical_flags.take().unwrap_or_default();
            let cursor_offset =
                (logical_start_y == cursor.logical_start_y).then_some(cursor.offset);
            let (mut reflowed, relative_cursor) = if let Some(text) = logical_plain_text.take() {
                reflow_plain_ascii_line_remapping_cursor(&text, flags, width, bg, cursor_offset)
            } else {
                shell_marks.account_for_clamped_cells(&logical_cells, width);
                reflow_logical_line_remapping_cursor(
                    &logical_cells,
                    flags,
                    width,
                    bg,
                    cursor_offset,
                )
            };
            if let Some(mut physical) = relative_cursor {
                physical.absolute_y = physical.absolute_y.saturating_add(output.len());
                mapped_cursor = Some(physical);
            }
            shell_marks.apply(&mut reflowed);
            output.extend(reflowed);
            logical_cells.clear();
        }
    }

    if logical_flags.is_some() || !logical_cells.is_empty() || logical_plain_text.is_some() {
        let flags = logical_flags.unwrap_or_default();
        let cursor_offset = (logical_start_y == cursor.logical_start_y).then_some(cursor.offset);
        let (mut reflowed, relative_cursor) = if let Some(text) = logical_plain_text {
            reflow_plain_ascii_line_remapping_cursor(&text, flags, width, bg, cursor_offset)
        } else {
            shell_marks.account_for_clamped_cells(&logical_cells, width);
            reflow_logical_line_remapping_cursor(&logical_cells, flags, width, bg, cursor_offset)
        };
        if let Some(mut physical) = relative_cursor {
            physical.absolute_y = physical.absolute_y.saturating_add(output.len());
            mapped_cursor = Some(physical);
        }
        shell_marks.apply(&mut reflowed);
        output.extend(reflowed);
    }

    (output, mapped_cursor)
}

fn extend_plain_ascii_cells(cells: &mut Vec<GridCell>, bytes: impl IntoIterator<Item = u8>) {
    cells.extend(bytes.into_iter().map(GridCell::from_plain_ascii));
}

fn reflow_plain_ascii_line_remapping_cursor(
    text: &str,
    first_flags: GridLineFlags,
    width: u32,
    bg: Colour,
    cursor_offset: Option<usize>,
) -> (Vec<GridLine>, Option<GridPhysicalCursor>) {
    let width = width.max(1);
    if text.is_empty() || bg != COLOUR_DEFAULT {
        let mut line = GridLine::blank_with_bg(width, bg);
        line.flags = first_flags;
        let cursor = cursor_offset.map(|offset| GridPhysicalCursor {
            absolute_y: 0,
            x: u32::try_from(offset)
                .unwrap_or(u32::MAX)
                .min(width.saturating_sub(1)),
            pending_wrap: false,
        });
        return (vec![line], cursor);
    }

    let width_usize = width as usize;
    let mut output = Vec::with_capacity(text.len().div_ceil(width_usize));
    let mut start = 0;
    let mut flags = first_flags;
    while start < text.len() {
        let end = (start + width_usize).min(text.len());
        let mut line = GridLine::from_plain_ascii_text(width, flags, text[start..end].to_owned());
        if end < text.len() {
            line.set_wrapped(true);
        }
        output.push(line);
        flags = GridLineFlags::default();
        start = end;
    }

    let cursor = cursor_offset.map(|offset| {
        let content_len = text.len();
        if offset == content_len && content_len > 0 && content_len.is_multiple_of(width_usize) {
            return GridPhysicalCursor {
                absolute_y: content_len.div_ceil(width_usize).saturating_sub(1),
                x: width.saturating_sub(1),
                pending_wrap: true,
            };
        }
        if offset <= content_len {
            return GridPhysicalCursor {
                absolute_y: offset / width_usize,
                x: u32::try_from(offset % width_usize).unwrap_or(u32::MAX),
                pending_wrap: false,
            };
        }
        GridPhysicalCursor {
            absolute_y: output.len().saturating_sub(1),
            x: u32::try_from(offset)
                .unwrap_or(u32::MAX)
                .min(width.saturating_sub(1)),
            pending_wrap: false,
        }
    });
    (output, cursor)
}

fn reflow_logical_line_remapping_cursor(
    cells: &[GridCell],
    first_flags: GridLineFlags,
    width: u32,
    bg: Colour,
    cursor_offset: Option<usize>,
) -> (Vec<GridLine>, Option<GridPhysicalCursor>) {
    let width = width.max(1);
    if cells.is_empty() {
        let mut line = GridLine::blank_with_bg(width, bg);
        line.flags = first_flags;
        let cursor = cursor_offset.map(|offset| GridPhysicalCursor {
            absolute_y: 0,
            x: u32::try_from(offset)
                .unwrap_or(u32::MAX)
                .min(width.saturating_sub(1)),
            pending_wrap: false,
        });
        return (vec![line], cursor);
    }

    let mut output = Vec::new();
    let mut current = GridLine::blank_with_bg(width, bg);
    current.flags = first_flags;
    let mut x: u32 = 0;
    let mut logical_offset = 0_usize;
    let mut mapped_cursor = None;

    for cell in cells {
        let mut cell = cell.clone();
        let source_cell_width = u32::from(cell.width().max(1));
        let mut cell_width = source_cell_width;
        if cell_width > width {
            cell_width = 1;
            cell.set_width(1);
        }
        if x > 0 && x.saturating_add(cell_width) > width {
            current.mark_reflow_gap(x);
            current.set_wrapped(true);
            output.push(current);
            current = GridLine::blank_with_bg(width, bg);
            x = 0;
        }

        if mapped_cursor.is_none() {
            if let Some(cursor_offset) = cursor_offset {
                let cell_end = logical_offset.saturating_add(source_cell_width as usize);
                if cursor_offset >= logical_offset && cursor_offset < cell_end {
                    let relative = cursor_offset.saturating_sub(logical_offset);
                    let physical_offset =
                        u32::try_from(relative).unwrap_or(u32::MAX).min(cell_width);
                    if physical_offset == cell_width && x.saturating_add(cell_width) == width {
                        mapped_cursor = Some(GridPhysicalCursor {
                            absolute_y: output.len(),
                            x: width.saturating_sub(1),
                            pending_wrap: true,
                        });
                    } else {
                        mapped_cursor = Some(GridPhysicalCursor {
                            absolute_y: output.len(),
                            x: x.saturating_add(physical_offset)
                                .min(width.saturating_sub(1)),
                            pending_wrap: false,
                        });
                    }
                }
            }
        }

        if let Some(target) = current.cell_mut(x) {
            *target = cell.clone();
        }
        for offset in 1..cell_width {
            if let Some(padding_cell) = current.cell_mut(x + offset) {
                let mut padding = cell.clone();
                padding.set_text(" ".to_owned());
                padding.set_width(0);
                padding.set_flags(GridCellFlags::PADDING);
                *padding_cell = padding;
            }
        }
        current.touch();
        x += cell_width;
        logical_offset = logical_offset.saturating_add(source_cell_width as usize);
    }

    if mapped_cursor.is_none() {
        if let Some(cursor_offset) = cursor_offset {
            if cursor_offset == logical_offset {
                mapped_cursor = Some(if x == width {
                    GridPhysicalCursor {
                        absolute_y: output.len(),
                        x: width.saturating_sub(1),
                        pending_wrap: true,
                    }
                } else {
                    GridPhysicalCursor {
                        absolute_y: output.len(),
                        x,
                        pending_wrap: false,
                    }
                });
            } else if cursor_offset > logical_offset {
                mapped_cursor = Some(GridPhysicalCursor {
                    absolute_y: output.len(),
                    x: u32::try_from(cursor_offset)
                        .unwrap_or(u32::MAX)
                        .min(width.saturating_sub(1)),
                    pending_wrap: false,
                });
            }
        }
    }
    output.push(current);
    (output, mapped_cursor)
}
