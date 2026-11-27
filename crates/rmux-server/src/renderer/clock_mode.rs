use chrono::{DateTime, Local};
use rmux_core::{OptionStore, PaneGeometry, Utf8Config};
use rmux_proto::OptionName;

use crate::clock_mode::format_clock_time;

use super::pane_row::crop_rendered_pane_row;
use super::projection::{
    ProjectedPaneScrollbarContext, TerminalPaneProjection, TerminalPaneSpan,
    TerminalSceneProjection,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClockPaneRenderData {
    pub(crate) pane_index: u32,
    pub(crate) history_size: usize,
    pub(crate) alternate_on: bool,
}

impl ClockPaneRenderData {
    #[cfg(test)]
    const fn new(pane_index: u32) -> Self {
        Self {
            pane_index,
            history_size: 0,
            alternate_on: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClockPaneRestoreData {
    pub(crate) pane_index: u32,
    pub(crate) lines: Vec<String>,
    pub(crate) history_size: usize,
    pub(crate) alternate_on: bool,
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
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    panes: &[ClockPaneRenderData],
    now: DateTime<Local>,
) -> Vec<u8> {
    if panes.is_empty() {
        return Vec::new();
    }

    let mut frame = Vec::new();
    frame.extend_from_slice(b"\x1b[s\x1b[?25l");
    let projection = TerminalSceneProjection::resolve(session, options);
    for pane_data in panes {
        let Some(pane) = session.window().pane(pane_data.pane_index) else {
            continue;
        };
        let Some(pane_projection) = projection.pane_projection(
            session,
            options,
            pane,
            ProjectedPaneScrollbarContext {
                history_size: pane_data.history_size,
                alternate_on: pane_data.alternate_on,
                copy_mode_scroll_position: None,
            },
        ) else {
            continue;
        };
        if pane_projection.spans().is_empty() && pane_projection.scrollbar().is_none() {
            continue;
        }
        let geometry = pane_projection.source_geometry();

        render_pane_clear(&mut frame, &pane_projection);
        render_clock_for_pane(
            &mut frame,
            geometry,
            pane_projection.spans(),
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
        if let Some(scrollbar) = pane_projection.scrollbar() {
            frame.extend_from_slice(scrollbar.frame().as_slice());
        }
    }
    frame.extend_from_slice(b"\x1b[0m\x1b[u");
    frame
}

pub(crate) fn render_clock_restore_frame(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    panes: &[ClockPaneRestoreData],
    cursor_visible: bool,
) -> Vec<u8> {
    if panes.is_empty() {
        return Vec::new();
    }

    let mut frame = Vec::new();
    frame.extend_from_slice(b"\x1b[s\x1b[0m");
    let projection = TerminalSceneProjection::resolve(session, options);
    let utf8 = Utf8Config::from_options(options);
    for pane_data in panes {
        let Some(pane) = session.window().pane(pane_data.pane_index) else {
            continue;
        };
        let Some(pane_projection) = projection.pane_projection(
            session,
            options,
            pane,
            ProjectedPaneScrollbarContext {
                history_size: pane_data.history_size,
                alternate_on: pane_data.alternate_on,
                copy_mode_scroll_position: None,
            },
        ) else {
            continue;
        };
        for span in pane_projection.spans() {
            let Some(line) = pane_data.lines.get(span.source_row) else {
                continue;
            };
            let line = crop_rendered_pane_row(
                line.as_bytes(),
                span.source_col,
                usize::from(span.cols),
                &utf8,
            );
            frame.extend_from_slice(super::cursor_position_bytes(span.y, span.x).as_slice());
            frame.extend_from_slice(b"\x1b[0m");
            frame.extend_from_slice(line.bytes());
        }
        if let Some(scrollbar) = pane_projection.scrollbar() {
            frame.extend_from_slice(scrollbar.frame().as_slice());
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

fn render_pane_clear(frame: &mut Vec<u8>, projection: &TerminalPaneProjection) {
    frame.extend_from_slice(b"\x1b[49m");
    for span in projection.spans() {
        frame.extend_from_slice(super::cursor_position_bytes(span.y, span.x).as_slice());
        frame.extend(std::iter::repeat_n(b' ', usize::from(span.cols)));
    }
}

fn render_clock_for_pane(
    frame: &mut Vec<u8>,
    geometry: PaneGeometry,
    spans: &[TerminalPaneSpan],
    colour: Option<&str>,
    style: Option<&str>,
    now: DateTime<Local>,
) {
    let time = format_clock_time(now, style);
    if geometry.cols() < 6 * u16::try_from(time.len()).unwrap_or(u16::MAX) || geometry.rows() < 6 {
        render_small_clock(frame, geometry, spans, colour, &time);
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
                let Some((terminal_x, terminal_y)) =
                    projected_cell(spans, x.saturating_add(col), start_y.saturating_add(row))
                else {
                    continue;
                };
                frame.extend_from_slice(
                    super::cursor_position_bytes(terminal_y, terminal_x).as_slice(),
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
    spans: &[TerminalPaneSpan],
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
    for (x, y, text) in clipped_clock_text_runs(x, y, time, spans) {
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
        frame.extend_from_slice(text.as_bytes());
    }
}

fn projected_cell(spans: &[TerminalPaneSpan], x: u16, y: u16) -> Option<(u16, u16)> {
    spans.iter().find_map(|span| {
        let offset = usize::from(x).checked_sub(span.source_col)?;
        (span.source_row == usize::from(y) && offset < usize::from(span.cols))
            .then(|| (span.x + offset as u16, span.y))
    })
}

fn clipped_clock_text_runs<'a>(
    x: u16,
    y: u16,
    text: &'a str,
    spans: &[TerminalPaneSpan],
) -> Vec<(u16, u16, &'a str)> {
    let x = usize::from(x);
    let run_end = x.saturating_add(text.len());
    spans
        .iter()
        .filter(|span| span.source_row == usize::from(y))
        .filter_map(|span| {
            let start = x.max(span.source_col);
            let end = run_end.min(span.source_col.saturating_add(usize::from(span.cols)));
            if start >= end {
                return None;
            }
            let text_start = start.saturating_sub(x);
            let text_end = end.saturating_sub(x).min(text.len());
            Some((
                span.x + u16::try_from(start - span.source_col).ok()?,
                span.y,
                &text[text_start..text_end],
            ))
        })
        .collect()
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
    use super::{
        render_clock_overlay, render_clock_restore_frame, ClockPaneRenderData, ClockPaneRestoreData,
    };
    use chrono::{Local, TimeZone};
    use rmux_core::{FloatingPaneRect, OptionStore};
    use rmux_proto::{
        OptionName, ResizePaneAdjustment, ScopeSelector, SessionName, SetOptionMode, TerminalSize,
        WindowTarget,
    };

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid session name")
    }

    fn float_pane(
        session: &mut crate::session_fixture::SessionFixture,
        pane_index: u32,
        rect: FloatingPaneRect,
    ) {
        session
            .store
            .window_layout_context_mut(&WindowTarget::with_window(
                session.name().clone(),
                session.active_window_index(),
            ))
            .expect("window layout context")
            .float_pane(pane_index, rect)
            .expect("pane can float");
    }

    #[test]
    fn clock_overlay_uses_source_threshold_and_center_before_clipping() {
        for (x, y, cols, rows, expected) in [
            (-3, 0, 8, 4, "\x1b[3;1H\x1b[34m3:02"),
            (-3, -2, 8, 4, "\x1b[1;1H\x1b[34m3:02"),
            (8, 6, 8, 4, "\x1b[9;11H\x1b[34m13"),
            (-3, 0, 30, 6, "\x1b[44m"),
        ] {
            let mut session = crate::session_fixture::SessionFixture::new(
                session_name("source"),
                TerminalSize { cols: 12, rows: 10 },
            );
            session.split_active_pane().expect("split");
            float_pane(
                &mut session,
                1,
                FloatingPaneRect::new(x, y, cols, rows).expect("rect"),
            );
            let now = Local
                .with_ymd_and_hms(2026, 4, 15, 13, 2, 3)
                .single()
                .expect("time");
            let frame = String::from_utf8(render_clock_overlay(
                session.view(),
                &OptionStore::new(),
                &[ClockPaneRenderData::new(1)],
                now,
            ))
            .expect("frame");
            assert!(frame.contains(expected), "{frame:?} lacks {expected:?}");
            if cols == 30 {
                assert!(
                    !frame.contains("13:02"),
                    "large source must retain big glyphs"
                );
            }
        }
    }

    #[test]
    fn clock_overlay_draws_tmux_big_digits() {
        let session = crate::session_fixture::SessionFixture::new(
            session_name("alpha"),
            TerminalSize { cols: 48, rows: 7 },
        );
        let options = OptionStore::new();
        let now = Local
            .with_ymd_and_hms(2026, 4, 15, 13, 2, 3)
            .single()
            .expect("valid local time");

        let frame = String::from_utf8(render_clock_overlay(
            session.view(),
            &options,
            &[ClockPaneRenderData::new(0)],
            now,
        ))
        .expect("overlay is utf-8");

        assert!(frame.contains("\u{1b}[?25l"));
        assert!(frame.contains("\u{1b}[49m"));
        assert!(!frame.contains("\u{1b}[48;5;8m"));
        assert!(frame.contains("\u{1b}[44m"));
    }

    #[test]
    fn clock_overlay_falls_back_to_plain_text_when_pane_is_small() {
        let session = crate::session_fixture::SessionFixture::new(
            session_name("alpha"),
            TerminalSize { cols: 8, rows: 5 },
        );
        let options = OptionStore::new();
        let now = Local
            .with_ymd_and_hms(2026, 4, 15, 13, 2, 3)
            .single()
            .expect("valid local time");

        let frame = String::from_utf8(render_clock_overlay(
            session.view(),
            &options,
            &[ClockPaneRenderData::new(0)],
            now,
        ))
        .expect("overlay is utf-8");

        assert!(frame.contains("13:02"));
    }

    #[test]
    fn clock_overlay_obeys_always_modal_and_alternate_scrollbar_rules() {
        let session = crate::session_fixture::SessionFixture::new(
            session_name("alpha"),
            TerminalSize { cols: 20, rows: 8 },
        );
        let target = WindowTarget::with_window(session.name().clone(), 0);
        let now = Local
            .with_ymd_and_hms(2026, 4, 15, 13, 2, 3)
            .single()
            .expect("valid local time");
        let mut options = OptionStore::new();
        options
            .set(
                ScopeSelector::Session(session.name().clone()),
                OptionName::Status,
                "off".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("status disabled for the oracle geometry");
        options
            .set(
                ScopeSelector::Window(target.clone()),
                OptionName::PaneScrollbars,
                "on".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("always-on scrollbars");

        let always = String::from_utf8(render_clock_overlay(
            session.view(),
            &options,
            &[ClockPaneRenderData::new(0)],
            now,
        ))
        .expect("overlay is utf-8");
        assert!(always.contains("\u{1b}[5;8H"));
        assert!(always.contains("13:02"));
        assert!(always.contains("\u{1b}[1;20H\u{1b}[0;30;47m "));

        options
            .set(
                ScopeSelector::Window(target),
                OptionName::PaneScrollbars,
                "modal".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("modal scrollbars");
        let modal = String::from_utf8(render_clock_overlay(
            session.view(),
            &options,
            &[ClockPaneRenderData::new(0)],
            now,
        ))
        .expect("overlay is utf-8");
        assert!(modal.contains("\u{1b}[5;9H"));
        assert!(modal.contains("13:02"));
        assert!(!modal.contains("\u{1b}[1;20H"));

        options
            .set(
                ScopeSelector::Window(WindowTarget::with_window(session.name().clone(), 0)),
                OptionName::PaneScrollbars,
                "on".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("always-on scrollbars");
        let alternate = String::from_utf8(render_clock_overlay(
            session.view(),
            &options,
            &[ClockPaneRenderData {
                pane_index: 0,
                history_size: 0,
                alternate_on: true,
            }],
            now,
        ))
        .expect("overlay is utf-8");
        assert!(alternate.contains("\u{1b}[5;9H"));
        assert!(alternate.contains("13:02"));
        assert!(!alternate.contains("\u{1b}[1;20H"));
    }

    #[test]
    fn zoomed_windows_only_render_the_active_pane_clock() {
        let mut session = crate::session_fixture::SessionFixture::new(
            session_name("alpha"),
            TerminalSize { cols: 20, rows: 8 },
        );
        session.split_active_pane().expect("split succeeds");
        session
            .resize_pane(1, ResizePaneAdjustment::Zoom)
            .expect("zoom succeeds");
        let options = OptionStore::new();
        let now = Local
            .with_ymd_and_hms(2026, 4, 15, 13, 2, 3)
            .single()
            .expect("valid local time");

        let frame = String::from_utf8(render_clock_overlay(
            session.view(),
            &options,
            &[ClockPaneRenderData::new(0)],
            now,
        ))
        .expect("overlay is utf-8");

        assert!(!frame.contains("\u{1b}[2;1H"));
    }

    #[test]
    fn clock_overlay_clips_text_occluded_by_front_floating_pane() {
        let mut session = crate::session_fixture::SessionFixture::new(
            session_name("alpha"),
            TerminalSize { cols: 20, rows: 6 },
        );
        session.split_active_pane().expect("split succeeds");
        float_pane(
            &mut session,
            1,
            FloatingPaneRect::new(0, 0, 20, 3).expect("valid floating rect"),
        );
        let now = Local
            .with_ymd_and_hms(2026, 4, 15, 13, 2, 3)
            .single()
            .expect("valid local time");

        let frame = String::from_utf8(render_clock_overlay(
            session.view(),
            &OptionStore::new(),
            &[ClockPaneRenderData::new(0)],
            now,
        ))
        .expect("overlay is utf-8");

        assert!(
            !frame.contains("13:02"),
            "rear clock text must be clipped by the front floating frame: {frame:?}"
        );
    }

    #[test]
    fn restore_frame_restores_hidden_cursor_and_lines() {
        let session = crate::session_fixture::SessionFixture::new(
            session_name("alpha"),
            TerminalSize { cols: 8, rows: 4 },
        );
        let options = OptionStore::new();

        let frame = String::from_utf8(render_clock_restore_frame(
            session.view(),
            &options,
            &[ClockPaneRestoreData {
                pane_index: 0,
                lines: vec!["line one".to_owned(), "line two".to_owned()],
                history_size: 0,
                alternate_on: false,
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
        let session = crate::session_fixture::SessionFixture::new(
            session_name("alpha"),
            TerminalSize { cols: 48, rows: 7 },
        );
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

        let frame = String::from_utf8(render_clock_overlay(
            session.view(),
            &options,
            &[ClockPaneRenderData::new(0)],
            now,
        ))
        .expect("overlay is utf-8");

        assert!(frame.contains("\u{1b}[48;2;18;52;86m"));
    }
}
