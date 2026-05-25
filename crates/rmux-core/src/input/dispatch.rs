//! Command dispatch for terminal input sequences.

pub use super::commands::{CsiCommand, DcsPayload, EscCommand, InputAction, OscCommand};
pub use super::writer::ScreenWriter;

use super::csi_helpers::{
    dispatch_rm, dispatch_rm_private, dispatch_sm, dispatch_sm_private, dispatch_winops,
};
use super::mode;
use super::sgr;
use super::tables;
use super::{InputParser, INPUT_LAST};

// ─── C0 dispatch ───────────────────────────────────────────────────

pub(crate) fn dispatch_c0(parser: &mut InputParser, writer: &mut dyn ScreenWriter) {
    match parser.ch {
        0x00 => {} // NUL
        0x07 => {
            // BEL
            writer.bell();
        }
        0x08 => {
            // BS
            writer.backspace();
        }
        0x09 => {
            // HT
            writer.tab();
        }
        0x0a..=0x0c => {
            // LF/VT/FF
            let bg = parser.cell.cell.bg;
            writer.linefeed(false, bg);
            if writer.current_mode() & mode::MODE_CRLF != 0 {
                writer.carriage_return();
            }
        }
        0x0d => {
            // CR
            writer.carriage_return();
        }
        0x0e => {
            // SO — shift out (activate G1)
            parser.cell.set = 1;
        }
        0x0f => {
            // SI — shift in (activate G0)
            parser.cell.set = 0;
        }
        _ => {}
    }
}

// ─── ESC dispatch ──────────────────────────────────────────────────

pub(crate) fn dispatch_esc(parser: &mut InputParser, writer: &mut dyn ScreenWriter) {
    let cmd = match tables::lookup_esc(parser.ch, parser.interm_str()) {
        Some(cmd) => cmd,
        None => return,
    };

    match cmd {
        EscCommand::Ris => {
            parser.cell.reset();
            writer.full_reset();
        }
        EscCommand::Ind => {
            let bg = parser.cell.cell.bg;
            writer.linefeed(false, bg);
        }
        EscCommand::Nel => {
            let bg = parser.cell.cell.bg;
            writer.carriage_return();
            writer.linefeed(false, bg);
        }
        EscCommand::Hts => {
            writer.set_tab_stop();
        }
        EscCommand::Ri => {
            let bg = parser.cell.cell.bg;
            writer.reverse_index(bg);
        }
        EscCommand::Deckpam => {
            writer.mode_set(mode::MODE_KKEYPAD);
        }
        EscCommand::Deckpnm => {
            writer.mode_clear(mode::MODE_KKEYPAD);
        }
        EscCommand::Decsc => {
            // Save cell state, cursor position, and origin mode.
            parser.saved.cell = parser.cell.clone();
            parser.saved.cx = writer.cursor_x();
            parser.saved.cy = writer.cursor_y();
            parser.saved.mode_origin = (writer.current_mode() & mode::MODE_ORIGIN) != 0;
        }
        EscCommand::Decrc => {
            // Restore cell state, cursor position, and origin mode.
            parser.cell = parser.saved.cell.clone();
            if parser.saved.mode_origin {
                writer.mode_set(mode::MODE_ORIGIN);
            } else {
                writer.mode_clear(mode::MODE_ORIGIN);
            }
            writer.cursor_move(parser.saved.cx as i32, parser.saved.cy as i32, false);
        }
        EscCommand::Decaln => {
            writer.alignment_test();
        }
        EscCommand::Scsg0On => {
            parser.cell.g0set = 1;
        }
        EscCommand::Scsg0Off => {
            parser.cell.g0set = 0;
        }
        EscCommand::Scsg1On => {
            parser.cell.g1set = 1;
        }
        EscCommand::Scsg1Off => {
            parser.cell.g1set = 0;
        }
        EscCommand::St => {
            // ST just terminates — state transition already handled it.
        }
    }
}

// ─── CSI dispatch ──────────────────────────────────────────────────

pub(crate) fn dispatch_csi(parser: &mut InputParser, writer: &mut dyn ScreenWriter) {
    if !parser.param_list.split(&parser.param_buf, parser.param_len) {
        return;
    }

    let cmd = match tables::lookup_csi(parser.ch, parser.interm_str()) {
        Some(cmd) => cmd,
        None => return,
    };

    let bg = parser.cell.cell.bg;

    match cmd {
        CsiCommand::Ich => {
            let n = parser.param_list.get(0, 1, 1);
            if n != -1 {
                writer.insert_character(n as u32, bg);
            }
        }
        CsiCommand::Cuu => {
            let n = parser.param_list.get(0, 1, 1);
            if n != -1 {
                writer.cursor_up(n as u32);
            }
        }
        CsiCommand::Cud => {
            let n = parser.param_list.get(0, 1, 1);
            if n != -1 {
                writer.cursor_down(n as u32);
            }
        }
        CsiCommand::Cuf => {
            let n = parser.param_list.get(0, 1, 1);
            if n != -1 {
                writer.cursor_right(n as u32);
            }
        }
        CsiCommand::Cub => {
            let n = parser.param_list.get(0, 1, 1);
            if n != -1 {
                writer.cursor_left(n as u32);
            }
        }
        CsiCommand::Cnl => {
            let n = parser.param_list.get(0, 1, 1);
            if n != -1 {
                writer.carriage_return();
                writer.cursor_down(n as u32);
            }
        }
        CsiCommand::Cpl => {
            let n = parser.param_list.get(0, 1, 1);
            if n != -1 {
                writer.carriage_return();
                writer.cursor_up(n as u32);
            }
        }
        CsiCommand::Cup => {
            let n = parser.param_list.get(0, 1, 1);
            let m = parser.param_list.get(1, 1, 1);
            if n != -1 && m != -1 {
                writer.cursor_move(m - 1, n - 1, true);
            }
        }
        CsiCommand::Hpa => {
            let n = parser.param_list.get(0, 1, 1);
            if n != -1 {
                writer.cursor_move(n - 1, -1, true);
            }
        }
        CsiCommand::Vpa => {
            let n = parser.param_list.get(0, 1, 1);
            if n != -1 {
                writer.cursor_move(-1, n - 1, true);
            }
        }
        CsiCommand::Ed => match parser.param_list.get(0, 0, 0) {
            -1 => {}
            0 => writer.clear_end_of_screen(bg),
            1 => writer.clear_start_of_screen(bg),
            2 => writer.clear_screen(bg),
            3 if parser.param_list.get(1, 0, 0) == 0 => {
                writer.clear_history();
            }
            _ => {}
        },
        CsiCommand::El => match parser.param_list.get(0, 0, 0) {
            -1 => {}
            0 => writer.clear_end_of_line(bg),
            1 => writer.clear_start_of_line(bg),
            2 => writer.clear_line(bg),
            _ => {}
        },
        CsiCommand::Il => {
            let n = parser.param_list.get(0, 1, 1);
            if n != -1 {
                writer.insert_line(n as u32, bg);
            }
        }
        CsiCommand::Dl => {
            let n = parser.param_list.get(0, 1, 1);
            if n != -1 {
                writer.delete_line(n as u32, bg);
            }
        }
        CsiCommand::Dch => {
            let n = parser.param_list.get(0, 1, 1);
            if n != -1 {
                writer.delete_character(n as u32, bg);
            }
        }
        CsiCommand::Ech => {
            let n = parser.param_list.get(0, 1, 1);
            if n != -1 {
                writer.clear_character(n as u32, bg);
            }
        }
        CsiCommand::Su => {
            let n = parser.param_list.get(0, 1, 1);
            if n != -1 {
                writer.scroll_up(n as u32, bg);
            }
        }
        CsiCommand::Sd => {
            let n = parser.param_list.get(0, 1, 1);
            if n != -1 {
                writer.scroll_down(n as u32, bg);
            }
        }
        CsiCommand::Cbt => {
            let n = parser.param_list.get(0, 1, 1);
            if n != -1 {
                writer.cursor_backward_tab(n as u32);
            }
        }
        CsiCommand::Rep => {
            let n = parser.param_list.get(0, 1, 1);
            if n == -1 || parser.flags & INPUT_LAST == 0 {
                return; // No prior character to repeat, or invalid param.
            }
            if let Some(ch) = parser.last_char {
                let set = if parser.cell.set == 0 {
                    parser.cell.g0set
                } else {
                    parser.cell.g1set
                };
                let remaining = writer.screen_size_x().saturating_sub(writer.cursor_x());
                let count = (n as u32).min(remaining);
                for _ in 0..count {
                    writer.collect_add_with_charset(ch, &parser.cell, set != 0);
                }
            }
        }
        CsiCommand::Da => {
            match parser.param_list.get(0, 0, 0) {
                0 => {
                    // DA primary: VT100 with AVO.
                    parser.reply("\x1b[?1;2c");
                }
                -1 => {}
                _ => {}
            }
        }
        CsiCommand::DaTwo => {
            match parser.param_list.get(0, 0, 0) {
                0 => {
                    // DA secondary: tmux terminal type 84.
                    parser.reply("\x1b[>84;0;0c");
                }
                -1 => {}
                _ => {}
            }
        }
        CsiCommand::Dsr => {
            match parser.param_list.get(0, 0, 0) {
                -1 => {}
                5 => {
                    // Status report: OK.
                    parser.reply("\x1b[0n");
                }
                6 => {
                    // Cursor position report.
                    let cy = writer.cursor_y() + 1;
                    let cx = writer.cursor_x() + 1;
                    let reply = format!("\x1b[{cy};{cx}R");
                    parser.reply(&reply);
                }
                _ => {}
            }
        }
        CsiCommand::DsrPrivate => {
            // Private DSR: theme query etc. — server-level concerns.
            // For now, no-op.
        }
        CsiCommand::QueryPrivate => {
            // DECRPM query — server-level concern, requires option access.
            // Surface the query for the server to handle.
            let param = parser.param_list.get(0, 0, 0);
            let mode_val = writer.current_mode();
            let reply = match param {
                1004 => {
                    let n = if mode_val & mode::MODE_FOCUSON != 0 {
                        1
                    } else {
                        2
                    };
                    format!("\x1b[?1004;{n}$y")
                }
                1006 => {
                    let n = if mode_val & mode::MODE_MOUSE_SGR != 0 {
                        1
                    } else {
                        2
                    };
                    format!("\x1b[?1006;{n}$y")
                }
                2004 => {
                    let n = if mode_val & mode::MODE_BRACKETPASTE != 0 {
                        1
                    } else {
                        2
                    };
                    format!("\x1b[?2004;{n}$y")
                }
                2026 => {
                    let n = if mode_val & mode::MODE_SYNC != 0 {
                        1
                    } else {
                        2
                    };
                    format!("\x1b[?2026;{n}$y")
                }
                2031 => "\x1b[?2031;2$y".to_owned(),
                _ => return,
            };
            parser.reply(&reply);
        }
        CsiCommand::Decstbm => {
            let n = parser.param_list.get(0, 1, 1);
            let sy = writer.screen_size_y();
            let m = parser.param_list.get(1, 1, sy as i32);
            if n != -1 && m != -1 {
                writer.set_scroll_region((n - 1) as u32, (m - 1) as u32);
            }
        }
        CsiCommand::Tbc => match parser.param_list.get(0, 0, 0) {
            -1 => {}
            0 => writer.clear_tab_stop(),
            3 => writer.clear_all_tab_stops(),
            _ => {}
        },
        CsiCommand::Sm => dispatch_sm(parser, writer),
        CsiCommand::SmPrivate => dispatch_sm_private(parser, writer),
        CsiCommand::Rm => dispatch_rm(parser, writer),
        CsiCommand::RmPrivate => dispatch_rm_private(parser, writer),
        CsiCommand::Sgr => sgr::dispatch_sgr(parser),
        CsiCommand::SmGraphics => {
            // Graphics mode query — typically needs sixel support.
            // Deferred to Milestone 30.
        }
        CsiCommand::Modset => {
            let n = parser.param_list.get(0, 0, 0);
            if n != 4 {
                return;
            }
            let m = parser.param_list.get(1, 0, 0);
            writer.mode_clear(mode::EXTENDED_KEY_MODES);
            match m {
                2 => writer.mode_set(mode::MODE_KEYS_EXTENDED_2),
                1 => writer.mode_set(mode::MODE_KEYS_EXTENDED),
                _ => {}
            }
        }
        CsiCommand::Modoff => {
            let n = parser.param_list.get(0, 0, 0);
            if n != 4 {
                return;
            }
            writer.mode_clear(mode::EXTENDED_KEY_MODES);
        }
        CsiCommand::Scp => {
            // Save cursor position.
            parser.saved.cell = parser.cell.clone();
            parser.saved.cx = writer.cursor_x();
            parser.saved.cy = writer.cursor_y();
            parser.saved.mode_origin = (writer.current_mode() & mode::MODE_ORIGIN) != 0;
        }
        CsiCommand::Rcp => {
            // Restore cursor position.
            parser.cell = parser.saved.cell.clone();
            if parser.saved.mode_origin {
                writer.mode_set(mode::MODE_ORIGIN);
            } else {
                writer.mode_clear(mode::MODE_ORIGIN);
            }
            writer.cursor_move(parser.saved.cx as i32, parser.saved.cy as i32, false);
        }
        CsiCommand::Decscusr => {
            let n = parser.param_list.get(0, 0, 0);
            if n != -1 {
                writer.set_cursor_style(n as u32);
                if n == 0 {
                    // Go back to default blinking state.
                    writer.mode_clear(mode::MODE_CURSOR_BLINKING_SET);
                }
            }
        }
        CsiCommand::Xda => {
            let n = parser.param_list.get(0, 0, 0);
            if n == 0 {
                let version = env!("CARGO_PKG_VERSION");
                let reply = format!("\x1bP>|tmux {version}\x1b\\");
                parser.reply(&reply);
            }
        }
        CsiCommand::Winops => dispatch_winops(parser, writer),
    }
}

// ─── OSC dispatch ──────────────────────────────────────────────────

pub(crate) fn dispatch_osc(parser: &mut InputParser, writer: &mut dyn ScreenWriter) {
    let buf = &parser.input_buf;
    if buf.is_empty() {
        return;
    }

    // Parse OSC number.
    let mut i = 0;
    if buf[i] < b'0' || buf[i] > b'9' {
        return;
    }
    let mut option: u32 = 0;
    while i < buf.len() && buf[i] >= b'0' && buf[i] <= b'9' {
        option = option
            .saturating_mul(10)
            .saturating_add(u32::from(buf[i] - b'0'));
        i += 1;
    }
    if i < buf.len() && buf[i] != b';' && buf[i] != 0 {
        return;
    }
    if i < buf.len() && buf[i] == b';' {
        i += 1;
    }

    let data = String::from_utf8_lossy(&buf[i..]);
    let end = parser.input_end;

    match option {
        0 | 2 => writer.set_title(&data),
        4 => writer.osc_palette(&data, end),
        7 => writer.set_path(&data),
        8 => {
            writer.osc_hyperlink(&data);
            parser.cell.cell.link = writer.current_hyperlink_id();
        }
        9 => writer.osc_notification(&data),
        10 => writer.osc_fg_colour(&data, end),
        11 => writer.osc_bg_colour(&data, end),
        12 => writer.osc_cursor_colour(&data, end),
        52 => writer.osc_clipboard(&data, end),
        104 => writer.osc_reset_palette(&data),
        110 => writer.osc_reset_fg(),
        111 => writer.osc_reset_bg(),
        112 => writer.osc_reset_cursor(),
        133 => writer.osc_shell_integration(&data),
        _ => {}
    }
}

// ─── DCS dispatch ──────────────────────────────────────────────────

pub(crate) fn dispatch_dcs(parser: &mut InputParser, writer: &mut dyn ScreenWriter) {
    let buf = &parser.input_buf;

    // DECRQSS: intermediate '$', first byte 'q'.
    if parser.interm_len == 1 && parser.interm_buf[0] == b'$' && !buf.is_empty() && buf[0] == b'q' {
        parser.reply("\x1bP0$r\x1b\\"); // Not recognized.
        return;
    }

    // tmux passthrough: DCS with `tmux;` prefix.
    if buf.starts_with(b"tmux;") {
        writer.dcs_passthrough(&buf[5..]);
        return;
    }

    // Sixel: final byte 'q' with no intermediates. Preserve DCS parameters
    // before the final byte so the outer terminal receives the original image
    // command, minus the ESC P / ST framing.
    if parser.interm_len == 0 && buf.first() == Some(&b'q') {
        let mut payload = Vec::with_capacity(parser.param_len + buf.len());
        payload.extend_from_slice(&parser.param_buf[..parser.param_len]);
        payload.extend_from_slice(buf);
        writer.sixel_passthrough(&payload);
    }
}
