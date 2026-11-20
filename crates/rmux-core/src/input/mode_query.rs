//! Read-only reports for modes the pane actually implements.

use super::{mode, InputParser, ScreenWriter};

pub(super) fn dispatch<W: ScreenWriter + ?Sized>(
    parser: &mut InputParser,
    writer: &W,
    private: bool,
) {
    let number = parser.param_list.get(0, 0, 0);
    if number <= 0 {
        return;
    }
    let status = if private {
        private_status(writer, number)
    } else if number == 4 {
        state(writer.current_mode() & mode::MODE_INSERT != 0)
    } else {
        0
    };
    let prefix = if private { "?" } else { "" };
    parser.reply(&format!("\x1b[{prefix}{number};{status}$y"));
}

fn state(enabled: bool) -> u8 {
    if enabled {
        1
    } else {
        2
    }
}

fn private_status<W: ScreenWriter + ?Sized>(writer: &W, number: i32) -> u8 {
    let bit = match number {
        1 => mode::MODE_KCURSOR,
        // tmux 3.7b reports permanently reset; no column-mode implementation.
        3 => return 4,
        6 => mode::MODE_ORIGIN,
        7 => mode::MODE_WRAP,
        12 => {
            let cursor = writer.resolved_cursor_state();
            return state(cursor.default_blink().unwrap_or(cursor.style() % 2 == 1));
        }
        25 => mode::MODE_CURSOR,
        47 | 1047 | 1049 => return state(writer.alternate_active()),
        1000 => mode::MODE_MOUSE_STANDARD,
        1002 => mode::MODE_MOUSE_BUTTON,
        1003 => mode::MODE_MOUSE_ALL,
        1004 => mode::MODE_FOCUSON,
        1005 => mode::MODE_MOUSE_UTF8,
        1006 => mode::MODE_MOUSE_SGR,
        2004 => mode::MODE_BRACKETPASTE,
        2026 => mode::MODE_SYNC,
        // This reports the stored subscription, not delivery of theme events.
        2031 => mode::MODE_THEME_UPDATES,
        _ => return 0,
    };
    state(writer.current_mode() & bit != 0)
}
