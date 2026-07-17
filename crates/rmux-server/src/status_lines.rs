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
