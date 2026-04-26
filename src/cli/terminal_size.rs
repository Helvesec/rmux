use rmux_proto::TerminalSize;

const DEFAULT_SESSION_COLS: u16 = 80;
const DEFAULT_SESSION_ROWS: u16 = 24;

pub(super) fn build_terminal_size(cols: Option<u16>, rows: Option<u16>) -> Option<TerminalSize> {
    match (cols, rows) {
        (None, None) => None,
        (cols, rows) => Some(TerminalSize {
            cols: cols.unwrap_or(DEFAULT_SESSION_COLS),
            rows: rows.unwrap_or(DEFAULT_SESSION_ROWS),
        }),
    }
}

pub(super) fn current_terminal_size() -> Option<TerminalSize> {
    rmux_os::terminal::current_size()
}
