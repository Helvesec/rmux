use super::OverlayFrame;

type PopupRow<'a> = (&'a [u8], &'a [u8]);

struct PopupFrame<'a> {
    rows: Vec<PopupRow<'a>>,
    cursor: &'a [u8],
}

/// Diff only self-contained, ordered popup rows, against the last frame that
/// was actually emitted. Never cache a delta: switches and refresh-client must
/// still be able to restore the entire popup, including its cursor state.
pub(in crate::pane_io) fn popup_frame_delta(
    cache: Option<&[u8]>,
    visible: bool,
    overlay: &OverlayFrame,
) -> Option<Vec<u8>> {
    if !overlay.row_diff || !overlay.persistent || !visible {
        return None;
    }
    let before = parse_popup_frame(cache?)?;
    let after = parse_popup_frame(&overlay.frame)?;
    if before.rows.len() != after.rows.len()
        || before.rows.iter().zip(&after.rows).any(|(a, b)| a.0 != b.0)
    {
        return None;
    }
    let mut delta: Vec<u8> = before
        .rows
        .iter()
        .zip(&after.rows)
        .filter(|(a, b)| a.1 != b.1)
        .flat_map(|(_, row)| row.1.iter().copied())
        .collect();
    // PopupOverlayState appends the cursor after all rows. It is independent
    // of their contents: a cursor-only update must also reach the terminal.
    if !delta.is_empty() || before.cursor != after.cursor {
        delta.extend_from_slice(after.cursor);
    }
    Some(delta)
}

/// The popup renderer saves/restores the cursor and resets SGR for each row.
/// Require one strictly increasing row at a fixed column. Borders, nested
/// menus, transient messages, and other command layouts conservatively fall
/// back to a full frame rather than skipping overlapping drawing operations.
fn parse_popup_frame(frame: &[u8]) -> Option<PopupFrame<'_>> {
    let mut rest = frame;
    let mut rows = Vec::new();
    let mut last_y = 0;
    let mut column = None;
    while let Some(position) = rest.strip_prefix(b"\x1b7\x1b[0m\x1b[") {
        let h = position.iter().position(|byte| *byte == b'H')?;
        let coordinates = std::str::from_utf8(&position[..h]).ok()?;
        let (y, x) = coordinates.split_once(';')?;
        let y: u16 = y.parse().ok()?;
        let x: u16 = x.parse().ok()?;
        if y <= last_y || column.is_some_and(|col| col != x) {
            return None;
        }
        last_y = y;
        column = Some(x);
        let end = rest.windows(2).position(|pair| pair == b"\x1b8")? + 2;
        let row = &rest[..end];
        if !row.ends_with(b"\x1b[0m\x1b8") {
            return None;
        }
        rows.push((&position[..=h], row));
        rest = &rest[end..];
    }
    if rows.is_empty() || !is_popup_cursor(rest) {
        return None;
    }
    Some(PopupFrame { rows, cursor: rest })
}

/// Accept only render_popup_cursor's output, or a row-only renderer frame.
/// Unknown trailing commands must retain the conservative full-frame path.
fn is_popup_cursor(bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes == b"\x1b[?25l" {
        return true;
    }
    let Some(position) = bytes.strip_prefix(b"\x1b[") else {
        return false;
    };
    let Some(h) = position.iter().position(|byte| *byte == b'H') else {
        return false;
    };
    let Ok(coordinates) = std::str::from_utf8(&position[..h]) else {
        return false;
    };
    let Some((y, x)) = coordinates.split_once(';') else {
        return false;
    };
    let valid_coordinate = |value: &str| {
        value.as_bytes().iter().all(u8::is_ascii_digit) && value.parse::<u16>().is_ok_and(|n| n > 0)
    };
    valid_coordinate(y)
        && valid_coordinate(x)
        && matches!(&position[h + 1..], b"\x1b[?25h" | b"\x1b[?25l")
}

#[cfg(test)]
pub(super) fn popup_rows(frame: &[u8]) -> Option<Vec<PopupRow<'_>>> {
    Some(parse_popup_frame(frame)?.rows)
}
