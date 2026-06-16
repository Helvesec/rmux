use chrono::{DateTime, Local};
use rmux_core::{OptionStore, Pane, PaneGeometry, Session};
use rmux_proto::OptionName;

use crate::pane_visible_geometry::visible_pane_content_geometry;

use crate::clock_mode::format_clock_time;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClockPaneRestoreData {
    pub(crate) pane_index: u32,
    pub(crate) lines: Vec<String>,
}

const CLOCK_GLYPHS: [[[u8; 5]; 5]; 14] = [
    [
        [1, 1, 1, 1, 1],
        [1, 0, 0, 0, 1],
        [1, 0, 0, 0, 1],
        [1, 0, 0, 0, 1],
        [1, 1, 1, 1, 1],
    ],
    [
        [0, 0, 0, 0, 1],
        [0, 0, 0, 0, 1],
        [0, 0, 0, 0, 1],
        [0, 0, 0, 0, 1],
        [0, 0, 0, 0, 1],
    ],
    [
        [1, 1, 1, 1, 1],
        [0, 0, 0, 0, 1],
        [1, 1, 1, 1, 1],
        [1, 0, 0, 0, 0],
        [1, 1, 1, 1, 1],
    ],
    [
        [1, 1, 1, 1, 1],
        [0, 0, 0, 0, 1],
        [1, 1, 1, 1, 1],
        [0, 0, 0, 0, 1],
        [1, 1, 1, 1, 1],
    ],
    [
        [1, 0, 0, 0, 1],
        [1, 0, 0, 0, 1],
        [1, 1, 1, 1, 1],
        [0, 0, 0, 0, 1],
        [0, 0, 0, 0, 1],
    ],
    [
        [1, 1, 1, 1, 1],
        [1, 0, 0, 0, 0],
        [1, 1, 1, 1, 1],
        [0, 0, 0, 0, 1],
        [1, 1, 1, 1, 1],
    ],
    [
        [1, 1, 1, 1, 1],
        [1, 0, 0, 0, 0],
        [1, 1, 1, 1, 1],
        [1, 0, 0, 0, 1],
        [1, 1, 1, 1, 1],
    ],
    [
        [1, 1, 1, 1, 1],
        [0, 0, 0, 0, 1],
        [0, 0, 0, 0, 1],
        [0, 0, 0, 0, 1],
        [0, 0, 0, 0, 1],
    ],
    [
        [1, 1, 1, 1, 1],
        [1, 0, 0, 0, 1],
        [1, 1, 1, 1, 1],
        [1, 0, 0, 0, 1],
        [1, 1, 1, 1, 1],
    ],
    [
        [1, 1, 1, 1, 1],
        [1, 0, 0, 0, 1],
        [1, 1, 1, 1, 1],
        [0, 0, 0, 0, 1],
        [1, 1, 1, 1, 1],
    ],
    [
        [0, 0, 0, 0, 0],
        [0, 0, 1, 0, 0],
        [0, 0, 0, 0, 0],
        [0, 0, 1, 0, 0],
        [0, 0, 0, 0, 0],
    ],
    [
        [1, 1, 1, 1, 1],
        [1, 0, 0, 0, 1],
        [1, 1, 1, 1, 1],
        [1, 0, 0, 0, 1],
        [1, 0, 0, 0, 1],
    ],
    [
        [1, 1, 1, 1, 1],
        [1, 0, 0, 0, 1],
        [1, 1, 1, 1, 1],
        [1, 0, 0, 0, 0],
        [1, 0, 0, 0, 0],
    ],
    [
        [1, 0, 0, 0, 1],
        [1, 1, 0, 1, 1],
        [1, 0, 1, 0, 1],
        [1, 0, 0, 0, 1],
        [1, 0, 0, 0, 1],
    ],
];

pub(crate) fn render_clock_overlay(
    session: &Session,
    options: &OptionStore,
    pane_indexes: &[u32],
    now: DateTime<Local>,
) -> Vec<u8> {
    if pane_indexes.is_empty() {
        return Vec::new();
    }

    let mut frame = Vec::new();
    frame.extend_from_slice(b"\x1b[s\x1b[?25l");
    for pane_index in pane_indexes {
        let Some(pane) = session.window().pane(*pane_index) else {
            continue;
        };
        let Some(geometry) = visible_pane_geometry(session, options, pane) else {
            continue;
        };
        if geometry.cols() == 0 || geometry.rows() == 0 {
            continue;
        }

        render_pane_clear(&mut frame, geometry);
        render_clock_for_pane(
            &mut frame,
            geometry,
            options.resolve_for_pane(
                session.name(),
                session.active_window_index(),
                pane.index(),
                OptionName::ClockModeColour,
            ),
            options.resolve_for_pane(
                session.name(),
                session.active_window_index(),
                pane.index(),
                OptionName::ClockModeStyle,
            ),
            now,
        );
    }
    frame.extend_from_slice(b"\x1b[0m\x1b[u");
    frame
}

pub(crate) fn render_clock_restore_frame(
    session: &Session,
    options: &OptionStore,
    panes: &[ClockPaneRestoreData],
    cursor_visible: bool,
) -> Vec<u8> {
    if panes.is_empty() {
        return Vec::new();
    }

    let mut frame = Vec::new();
    frame.extend_from_slice(b"\x1b[s\x1b[0m");
    for pane_data in panes {
        let Some(pane) = session.window().pane(pane_data.pane_index) else {
            continue;
        };
        let Some(geometry) = visible_pane_geometry(session, options, pane) else {
            continue;
        };
        for (row, line) in pane_data
            .lines
            .iter()
            .take(usize::from(geometry.rows()))
            .enumerate()
        {
            let Ok(row) = u16::try_from(row) else {
                continue;
            };
            frame.extend_from_slice(
                super::cursor_position_bytes(geometry.y().saturating_add(row), geometry.x())
                    .as_slice(),
            );
            frame.extend_from_slice(b"\x1b[0m");
            frame.extend_from_slice(line.as_bytes());
        }
    }
    frame.extend_from_slice(b"\x1b[0m");
    if cursor_visible {
        frame.extend_from_slice(b"\x1b[?25h");
    } else {
        frame.extend_from_slice(b"\x1b[?25l");
    }
    frame.extend_from_slice(b"\x1b[u");
    frame
}

fn render_pane_clear(frame: &mut Vec<u8>, geometry: PaneGeometry) {
    frame.extend_from_slice(b"\x1b[49m");
    let blank_row = vec![b' '; usize::from(geometry.cols())];
    for row in 0..geometry.rows() {
        frame.extend_from_slice(
            super::cursor_position_bytes(geometry.y().saturating_add(row), geometry.x()).as_slice(),
        );
        frame.extend_from_slice(blank_row.as_slice());
    }
}

fn render_clock_for_pane(
    frame: &mut Vec<u8>,
    geometry: PaneGeometry,
    colour: Option<&str>,
    style: Option<&str>,
    now: DateTime<Local>,
) {
    let time = format_clock_time(now, style);
    if geometry.cols() < 6 * u16::try_from(time.len()).unwrap_or(u16::MAX) || geometry.rows() < 6 {
        render_small_clock(frame, geometry, colour, &time);
        return;
    }

    let start_x = geometry
        .x()
        .saturating_add(geometry.cols() / 2)
        .saturating_sub(3 * u16::try_from(time.len()).unwrap_or(u16::MAX));
    let start_y = geometry
        .y()
        .saturating_add(geometry.rows() / 2)
        .saturating_sub(3);
    let Some(colour) = super::parse_option_colour(colour) else {
        return;
    };
    if let Some(sgr) = super::background_sgr_parameter(colour) {
        frame.extend_from_slice(format!("\x1b[{sgr}m").as_bytes());
    }

    let mut x = start_x;
    for ch in time.chars() {
        let Some(index) = glyph_index(ch) else {
            x = x.saturating_add(6);
            continue;
        };
        for row in 0..5_u16 {
            for col in 0..5_u16 {
                if CLOCK_GLYPHS[index][usize::from(row)][usize::from(col)] == 0 {
                    continue;
                }
                frame.extend_from_slice(
                    super::cursor_position_bytes(
                        start_y.saturating_add(row),
                        x.saturating_add(col),
                    )
                    .as_slice(),
                );
                frame.push(b' ');
            }
        }
        x = x.saturating_add(6);
    }
}

fn render_small_clock(
    frame: &mut Vec<u8>,
    geometry: PaneGeometry,
    colour: Option<&str>,
    time: &str,
) {
    let Ok(width) = u16::try_from(time.len()) else {
        return;
    };
    if geometry.cols() < width || geometry.rows() == 0 {
        return;
    }

    let x = geometry
        .x()
        .saturating_add(geometry.cols() / 2)
        .saturating_sub(width / 2);
    let y = geometry.y().saturating_add(geometry.rows() / 2);
    frame.extend_from_slice(super::cursor_position_bytes(y, x).as_slice());
    match super::parse_option_colour(colour) {
        Some(colour) => {
            if let Some(sgr) = super::foreground_sgr_parameter(colour) {
                frame.extend_from_slice(format!("\x1b[{sgr}m").as_bytes());
            }
        }
        None => {
            frame.extend_from_slice(b"\x1b[0m");
        }
    }
    frame.extend_from_slice(time.as_bytes());
}

fn visible_pane_geometry(
    session: &Session,
    options: &OptionStore,
    pane: &Pane,
) -> Option<PaneGeometry> {
    let geometry = super::StatusGeometry::for_session(session, options);
    if session.window().is_zoomed() {
        if session.window().active_pane_index() != pane.index() {
            return None;
        }
        let size = geometry.content_size();
        let pane = visible_pane_content_geometry(
            options,
            session.name(),
            session.active_window_index(),
            PaneGeometry::new(0, 0, size.cols, size.rows),
            geometry.content_rows,
        );
        return Some(PaneGeometry::new(
            pane.x(),
            pane.y().saturating_add(geometry.content_y_offset),
            pane.cols(),
            pane.rows(),
        ));
    }

    let pane = visible_pane_content_geometry(
        options,
        session.name(),
        session.active_window_index(),
        pane.geometry(),
        geometry.content_rows,
    );
    Some(PaneGeometry::new(
        pane.x(),
        pane.y().saturating_add(geometry.content_y_offset),
        pane.cols(),
        pane.rows(),
    ))
}

fn glyph_index(ch: char) -> Option<usize> {
    match ch {
        '0'..='9' => Some((ch as u8 - b'0') as usize),
        ':' => Some(10),
        'A' => Some(11),
        'P' => Some(12),
        'M' => Some(13),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{render_clock_overlay, render_clock_restore_frame, ClockPaneRestoreData};
    use chrono::{Local, TimeZone};
    use rmux_core::{OptionStore, Session};
    use rmux_proto::{
        OptionName, ResizePaneAdjustment, ScopeSelector, SessionName, SetOptionMode, TerminalSize,
        WindowTarget,
    };

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid session name")
    }

    #[test]
    fn clock_overlay_draws_tmux_big_digits() {
        let session = Session::new(session_name("alpha"), TerminalSize { cols: 48, rows: 7 });
        let options = OptionStore::new();
        let now = Local
            .with_ymd_and_hms(2026, 4, 15, 13, 2, 3)
            .single()
            .expect("valid local time");

        let frame = String::from_utf8(render_clock_overlay(&session, &options, &[0], now))
            .expect("overlay is utf-8");

        assert!(frame.contains("\u{1b}[?25l"));
        assert!(frame.contains("\u{1b}[49m"));
        assert!(!frame.contains("\u{1b}[48;5;8m"));
        assert!(frame.contains("\u{1b}[44m"));
    }

    #[test]
    fn clock_overlay_falls_back_to_plain_text_when_pane_is_small() {
        let session = Session::new(session_name("alpha"), TerminalSize { cols: 8, rows: 5 });
        let options = OptionStore::new();
        let now = Local
            .with_ymd_and_hms(2026, 4, 15, 13, 2, 3)
            .single()
            .expect("valid local time");

        let frame = String::from_utf8(render_clock_overlay(&session, &options, &[0], now))
            .expect("overlay is utf-8");

        assert!(frame.contains("13:02"));
    }

    #[test]
    fn zoomed_windows_only_render_the_active_pane_clock() {
        let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 20, rows: 8 });
        session.split_active_pane().expect("split succeeds");
        session
            .resize_pane(1, ResizePaneAdjustment::Zoom)
            .expect("zoom succeeds");
        let options = OptionStore::new();
        let now = Local
            .with_ymd_and_hms(2026, 4, 15, 13, 2, 3)
            .single()
            .expect("valid local time");

        let frame = String::from_utf8(render_clock_overlay(&session, &options, &[0], now))
            .expect("overlay is utf-8");

        assert!(!frame.contains("\u{1b}[2;1H"));
    }

    #[test]
    fn restore_frame_restores_hidden_cursor_and_lines() {
        let session = Session::new(session_name("alpha"), TerminalSize { cols: 8, rows: 4 });
        let options = OptionStore::new();

        let frame = String::from_utf8(render_clock_restore_frame(
            &session,
            &options,
            &[ClockPaneRestoreData {
                pane_index: 0,
                lines: vec!["line one".to_owned(), "line two".to_owned()],
            }],
            false,
        ))
        .expect("frame is utf-8");

        assert!(frame.contains("\u{1b}[?25l"));
        assert!(!frame.contains("\u{1b}[?25h"));
        assert!(frame.contains("line one"));
        assert!(frame.contains("line two"));
    }

    #[test]
    fn clock_overlay_supports_rgb_clock_colours() {
        let session = Session::new(session_name("alpha"), TerminalSize { cols: 48, rows: 7 });
        let window = WindowTarget::new(session.name().clone());
        let mut options = OptionStore::new();
        options
            .set(
                ScopeSelector::Window(window),
                OptionName::ClockModeColour,
                "#123456".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("clock-mode-colour accepts rgb values");
        let now = Local
            .with_ymd_and_hms(2026, 4, 15, 13, 2, 3)
            .single()
            .expect("valid local time");

        let frame = String::from_utf8(render_clock_overlay(&session, &options, &[0], now))
            .expect("overlay is utf-8");

        assert!(frame.contains("\u{1b}[48;2;18;52;86m"));
    }
}
