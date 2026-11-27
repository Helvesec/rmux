pub(crate) fn status_line_count(status: Option<&str>, rows: u16) -> u16 {
    if rows == 0 || matches!(status, Some("off")) {
        return 0;
    }
    let requested = match status {
        Some("on") | None => 1,
        Some(value) => value.parse::<u16>().unwrap_or(1),
    };
    requested.clamp(1, rows)
}

pub(crate) fn content_rows_for_status(status: Option<&str>, rows: u16) -> u16 {
    rows.saturating_sub(status_line_count(status, rows))
}

/// Reserve the status (or the prompt row when status is off) and two borders.
pub(crate) fn prompt_completion_menu_height(
    size: rmux_proto::TerminalSize,
    status_lines: u16,
) -> usize {
    // Two borders, padding, a shortcut and at least one label cell must fit.
    if size.cols < 9 {
        return 0;
    }
    usize::from(
        size.rows
            .saturating_sub(status_lines.max(1))
            .saturating_sub(2)
            .min(10),
    )
}
