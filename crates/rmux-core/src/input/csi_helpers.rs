//! CSI helper dispatchers for mode and window-operation sequences.

use super::mode;
use super::writer::ScreenWriter;
use super::InputParser;

const WIN32_CONSOLE_INPUT_MODE: i32 = 9001;

pub(super) fn dispatch_sm<W: ScreenWriter + ?Sized>(parser: &mut InputParser, writer: &mut W) {
    for i in 0..parser.param_list.len() {
        match parser.param_list.get(i, 0, -1) {
            -1 => {}
            4 => writer.mode_set(mode::MODE_INSERT),
            34 => writer.mode_clear(mode::MODE_CURSOR_VERY_VISIBLE),
            _ => {}
        }
    }
}

pub(super) fn dispatch_rm<W: ScreenWriter + ?Sized>(parser: &mut InputParser, writer: &mut W) {
    for i in 0..parser.param_list.len() {
        match parser.param_list.get(i, 0, -1) {
            -1 => {}
            4 => writer.mode_clear(mode::MODE_INSERT),
            34 => writer.mode_set(mode::MODE_CURSOR_VERY_VISIBLE),
            _ => {}
        }
    }
}

pub(super) fn dispatch_sm_private<W: ScreenWriter + ?Sized>(
    parser: &mut InputParser,
    writer: &mut W,
) {
    let bg = parser.cell.cell.bg;
    for i in 0..parser.param_list.len() {
        match parser.param_list.get(i, 0, -1) {
            -1 => {}
            1 => writer.mode_set(mode::MODE_KCURSOR), // DECCKM
            3 => {
                // DECCOLM
                writer.cursor_move(0, 0, true);
                writer.clear_screen(bg);
            }
            6 => {
                // DECOM
                writer.mode_set(mode::MODE_ORIGIN);
                writer.cursor_move(0, 0, true);
            }
            7 => writer.mode_set(mode::MODE_WRAP), // DECAWM
            12 => {
                writer.mode_set(mode::MODE_CURSOR_BLINKING);
                writer.mode_set(mode::MODE_CURSOR_BLINKING_SET);
            }
            25 => writer.mode_set(mode::MODE_CURSOR), // TCEM
            1000 => {
                writer.mode_clear(mode::ALL_MOUSE_MODES);
                writer.mode_set(mode::MODE_MOUSE_STANDARD);
            }
            1002 => {
                writer.mode_clear(mode::ALL_MOUSE_MODES);
                writer.mode_set(mode::MODE_MOUSE_BUTTON);
            }
            1003 => {
                writer.mode_clear(mode::ALL_MOUSE_MODES);
                writer.mode_set(mode::MODE_MOUSE_ALL);
            }
            1004 => writer.mode_set(mode::MODE_FOCUSON),
            1005 => writer.mode_set(mode::MODE_MOUSE_UTF8),
            1006 => writer.mode_set(mode::MODE_MOUSE_SGR),
            47 | 1047 => writer.alternate_on(bg, false),
            1049 => writer.alternate_on(bg, true),
            2004 => writer.mode_set(mode::MODE_BRACKETPASTE),
            2026 => writer.start_sync(),
            2031 => writer.mode_set(mode::MODE_THEME_UPDATES),
            WIN32_CONSOLE_INPUT_MODE => {}
            _ => {}
        }
    }
}

pub(super) fn dispatch_rm_private<W: ScreenWriter + ?Sized>(
    parser: &mut InputParser,
    writer: &mut W,
) {
    let bg = parser.cell.cell.bg;
    for i in 0..parser.param_list.len() {
        match parser.param_list.get(i, 0, -1) {
            -1 => {}
            1 => writer.mode_clear(mode::MODE_KCURSOR), // DECCKM
            3 => {
                // DECCOLM
                writer.cursor_move(0, 0, true);
                writer.clear_screen(bg);
            }
            6 => {
                // DECOM
                writer.mode_clear(mode::MODE_ORIGIN);
                writer.cursor_move(0, 0, true);
            }
            7 => writer.mode_clear(mode::MODE_WRAP), // DECAWM
            12 => {
                writer.mode_clear(mode::MODE_CURSOR_BLINKING);
                writer.mode_set(mode::MODE_CURSOR_BLINKING_SET);
            }
            25 => writer.mode_clear(mode::MODE_CURSOR), // TCEM
            1000..=1003 => {
                writer.mode_clear(mode::ALL_MOUSE_MODES);
            }
            1004 => writer.mode_clear(mode::MODE_FOCUSON),
            1005 => writer.mode_clear(mode::MODE_MOUSE_UTF8),
            1006 => writer.mode_clear(mode::MODE_MOUSE_SGR),
            47 | 1047 => writer.alternate_off(bg, false),
            1049 => writer.alternate_off(bg, true),
            2004 => writer.mode_clear(mode::MODE_BRACKETPASTE),
            2026 => writer.stop_sync(),
            2031 => writer.mode_clear(mode::MODE_THEME_UPDATES),
            WIN32_CONSOLE_INPUT_MODE => {}
            _ => {}
        }
    }
}

pub(super) fn dispatch_winops<W: ScreenWriter + ?Sized>(parser: &mut InputParser, writer: &mut W) {
    let sx = writer.screen_size_x();
    let sy = writer.screen_size_y();
    let mut m: u32 = 0;
    loop {
        let n = parser.param_list.get(m, 0, -1);
        if n == -1 {
            break;
        }
        match n {
            1 | 2 | 5 | 6 | 7 | 11 | 13 | 20 | 21 | 24 => {}
            3 | 4 | 8 => {
                m += 1;
                if parser.param_list.get(m, 0, -1) == -1 {
                    return;
                }
                // Fall through to consume one more.
                m += 1;
                if parser.param_list.get(m, 0, -1) == -1 {
                    return;
                }
            }
            9 | 10 => {
                m += 1;
                if parser.param_list.get(m, 0, -1) == -1 {
                    return;
                }
            }
            18 => {
                let reply = format!("\x1b[8;{sy};{sx}t");
                parser.reply(&reply);
            }
            19 => {
                let reply = format!("\x1b[9;{sy};{sx}t");
                parser.reply(&reply);
            }
            22 => {
                m += 1;
                match parser.param_list.get(m, 0, -1) {
                    -1 => return,
                    0 | 2 => writer.push_title(),
                    _ => {}
                }
            }
            23 => {
                m += 1;
                match parser.param_list.get(m, 0, -1) {
                    -1 => return,
                    0 | 2 => {
                        writer.pop_title();
                        writer.notify_pane_title_changed();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        m += 1;
    }
}
