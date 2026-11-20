use rmux_core::OptionStore;
use rmux_proto::{OptionName, PaneTarget};

use crate::pane_terminals::HandlerState;

use super::projection::{TerminalPaneSpan, TerminalSceneProjection};

const DISPLAY_PANE_GLYPHS: [[[u8; 5]; 5]; 10] = [
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
];

pub(crate) fn render_display_panes_overlay(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
) -> Vec<u8> {
    let specs = display_pane_specs(session, options);
    if specs.is_empty() {
        return Vec::new();
    }

    let mut frame = Vec::new();
    frame.extend_from_slice(b"\x1b[s\x1b[?25l");
    for spec in &specs {
        for run in &spec.overlay_runs {
            for clipped in clipped_text_runs(run, &spec.spans) {
                frame.extend_from_slice(
                    super::cursor_position_bytes(clipped.y, clipped.x).as_slice(),
                );
                frame.extend_from_slice(b"\x1b[0m");
                frame.extend_from_slice(run.sgr.as_slice());
                frame.extend_from_slice(clipped.text.as_bytes());
            }
        }
    }
    frame.extend_from_slice(b"\x1b[0m\x1b[u");
    frame
}

pub(crate) fn render_display_panes_clear(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    state: &HandlerState,
) -> Vec<u8> {
    let base = super::render_with_attached_count_prompt_and_pane_title(
        session,
        options,
        0,
        super::StatusRenderContext {
            state: Some(state),
            ..super::StatusRenderContext::default()
        },
    );
    render_display_panes_clear_with_base(session, options, &base)
}

pub(crate) fn render_display_panes_clear_with_base(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    base_frame: &[u8],
) -> Vec<u8> {
    let specs = display_pane_specs(session, options);
    if specs.is_empty() {
        return Vec::new();
    }

    let mut frame = Vec::new();
    frame.extend_from_slice(b"\x1b[s\x1b[0m");
    for spec in &specs {
        for run in &spec.clear_runs {
            for clipped in clipped_clear_runs(run, &spec.spans) {
                frame.extend_from_slice(
                    super::cursor_position_bytes(clipped.y, clipped.x).as_slice(),
                );
                frame.resize(frame.len() + clipped.width, b' ');
            }
        }
    }
    frame.extend_from_slice(b"\x1b[0m\x1b[?25h\x1b[u");
    frame.extend_from_slice(base_frame);
    frame
}

pub(crate) fn display_panes_label_count(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
) -> u32 {
    u32::try_from(display_pane_specs(session, options).len()).unwrap_or(u32::MAX)
}

pub(crate) fn display_pane_targets(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
) -> Vec<DisplayPaneTarget> {
    let mut targets = Vec::new();
    for spec in display_pane_specs(session, options) {
        targets.push(DisplayPaneTarget {
            label: spec.label.clone(),
            target: spec.target.clone(),
            target_string: spec.target_string.clone(),
        });
        if let Some(alias) = spec.alias {
            targets.push(DisplayPaneTarget {
                label: alias,
                target: spec.target,
                target_string: spec.target_string,
            });
        }
    }
    targets
}

fn display_pane_specs(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
) -> Vec<DisplayPaneSpec> {
    let window = session.window();
    let active_pane_index = window.active_pane_index();
    let window_index = session.active_window_index();
    let projection = TerminalSceneProjection::resolve(session, options);

    window
        .panes()
        .iter()
        .filter_map(|pane| {
            let pane_projection = projection.raw_pane_projection(pane)?;
            if pane_projection.spans().is_empty() {
                return None;
            }
            let geometry = pane_projection.source_geometry();
            let visible_index = crate::pane_indices::visible_pane_index(
                session,
                options,
                window_index,
                pane.index(),
            );
            let label = visible_index.to_string();
            let label_width = u16::try_from(label.len()).ok()?;
            if geometry.cols() < label_width || geometry.rows() == 0 {
                return None;
            }

            let alias = pane_alias(visible_index);
            let is_active = pane.index() == active_pane_index;
            let compact = geometry.cols() < label_width.saturating_mul(6) || geometry.rows() < 5;
            let colour = display_panes_colour(
                options.resolve(
                    Some(session.name()),
                    if is_active {
                        OptionName::DisplayPanesActiveColour
                    } else {
                        OptionName::DisplayPanesColour
                    },
                ),
                compact,
            );
            let mut overlay_runs = Vec::new();
            let mut clear_runs = Vec::new();

            if compact {
                let text = compact_label_text(&label, alias.as_deref(), geometry.cols());
                let width = u16::try_from(text.len()).ok()?;
                let x = geometry
                    .x()
                    .saturating_add(geometry.cols() / 2)
                    .saturating_sub(width / 2);
                let y = geometry.y().saturating_add(geometry.rows() / 2);
                overlay_runs.push(DisplayPaneRun {
                    x,
                    y,
                    text: text.clone(),
                    sgr: colour.to_vec(),
                });
                clear_runs.push(DisplayPaneClearRun {
                    x,
                    y,
                    width: text.len(),
                });
            } else {
                let mut digit_x = geometry
                    .x()
                    .saturating_add(geometry.cols() / 2)
                    .saturating_sub(label_width.saturating_mul(3));
                let digit_y = geometry
                    .y()
                    .saturating_add(geometry.rows() / 2)
                    .saturating_sub(2);
                for ch in label.chars() {
                    let Some(index) = ch.to_digit(10).map(|value| value as usize) else {
                        digit_x = digit_x.saturating_add(6);
                        continue;
                    };
                    push_large_digit_runs(
                        &mut overlay_runs,
                        &mut clear_runs,
                        digit_x,
                        digit_y,
                        &DISPLAY_PANE_GLYPHS[index],
                        &colour,
                    );
                    digit_x = digit_x.saturating_add(6);
                }

                if geometry.rows() > 6 {
                    let size_text = format!("{}x{}", geometry.cols(), geometry.rows());
                    if usize::from(geometry.cols()) >= size_text.len() {
                        let x = geometry
                            .x()
                            .saturating_add(geometry.cols().saturating_sub(size_text.len() as u16));
                        let y = geometry.y();
                        overlay_runs.push(DisplayPaneRun {
                            x,
                            y,
                            text: size_text.clone(),
                            sgr: colour.to_vec(),
                        });
                        clear_runs.push(DisplayPaneClearRun {
                            x,
                            y,
                            width: size_text.len(),
                        });
                    }

                    if let Some(alias) = alias.as_ref() {
                        let x = geometry
                            .x()
                            .saturating_add(geometry.cols() / 2)
                            .saturating_add(label_width.saturating_mul(3))
                            .saturating_sub(2);
                        let y = digit_y.saturating_add(5);
                        overlay_runs.push(DisplayPaneRun {
                            x,
                            y,
                            text: alias.clone(),
                            sgr: colour.to_vec(),
                        });
                        clear_runs.push(DisplayPaneClearRun { x, y, width: 1 });
                    }
                }
            }

            Some(DisplayPaneSpec {
                label,
                alias,
                target: PaneTarget::with_window(session.name().clone(), window_index, pane.index()),
                target_string: format!(
                    "={}:{}.%{}",
                    session.name(),
                    window_index,
                    pane.id().as_u32()
                ),
                spans: pane_projection.spans().to_vec(),
                overlay_runs,
                clear_runs,
            })
        })
        .collect()
}

fn compact_label_text(label: &str, alias: Option<&str>, pane_width: u16) -> String {
    let Some(alias) = alias else {
        return label.to_owned();
    };
    let candidate = format!("{label} {alias}");
    if usize::from(pane_width) >= candidate.len() {
        candidate
    } else {
        label.to_owned()
    }
}

fn push_large_digit_runs(
    overlay_runs: &mut Vec<DisplayPaneRun>,
    clear_runs: &mut Vec<DisplayPaneClearRun>,
    start_x: u16,
    start_y: u16,
    glyph: &[[u8; 5]; 5],
    colour: &[u8],
) {
    for (row, cells) in glyph.iter().enumerate() {
        let Ok(row) = u16::try_from(row) else {
            continue;
        };
        let mut segment_start = None::<usize>;
        for col in 0..=cells.len() {
            let filled = cells.get(col).copied().unwrap_or(0) != 0;
            match (segment_start, filled) {
                (None, true) => segment_start = Some(col),
                (Some(start), false) => {
                    let width = col.saturating_sub(start);
                    let Ok(offset) = u16::try_from(start) else {
                        segment_start = None;
                        continue;
                    };
                    overlay_runs.push(DisplayPaneRun {
                        x: start_x.saturating_add(offset),
                        y: start_y.saturating_add(row),
                        text: " ".repeat(width),
                        sgr: colour.to_vec(),
                    });
                    clear_runs.push(DisplayPaneClearRun {
                        x: start_x.saturating_add(offset),
                        y: start_y.saturating_add(row),
                        width,
                    });
                    segment_start = None;
                }
                _ => {}
            }
        }
    }
}

fn pane_alias(index: u32) -> Option<String> {
    (10..35).contains(&index).then(|| {
        char::from_u32(u32::from(b'a') + index - 10)
            .unwrap_or('a')
            .to_string()
    })
}

fn display_panes_colour(value: Option<&str>, compact: bool) -> Vec<u8> {
    let Some(colour) = super::parse_option_colour(value) else {
        return Vec::new();
    };
    let parameter = if compact {
        super::foreground_sgr_parameter(colour)
    } else {
        super::background_sgr_parameter(colour)
    };
    let Some(parameter) = parameter else {
        return Vec::new();
    };
    format!("\x1b[{parameter}m").into_bytes()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DisplayPaneSpec {
    label: String,
    alias: Option<String>,
    target: PaneTarget,
    target_string: String,
    spans: Vec<TerminalPaneSpan>,
    overlay_runs: Vec<DisplayPaneRun>,
    clear_runs: Vec<DisplayPaneClearRun>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
// Runs are composed in source-local coordinates; only clipped runs are terminal-local.
struct DisplayPaneRun {
    x: u16,
    y: u16,
    text: String,
    sgr: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DisplayPaneClearRun {
    x: u16,
    y: u16,
    width: usize,
}

struct ClippedTextRun<'a> {
    x: u16,
    y: u16,
    text: &'a str,
}

struct ClippedClearRun {
    x: u16,
    y: u16,
    width: usize,
}

fn clipped_text_runs<'a>(
    run: &'a DisplayPaneRun,
    spans: &[TerminalPaneSpan],
) -> Vec<ClippedTextRun<'a>> {
    let run_start = usize::from(run.x);
    let run_end = run_start.saturating_add(run.text.len());
    spans
        .iter()
        .filter(|span| span.source_row == usize::from(run.y))
        .filter_map(|span| {
            let start = run_start.max(span.source_col);
            let end = run_end.min(span.source_col.saturating_add(usize::from(span.cols)));
            if start >= end {
                return None;
            }
            let text_start = start.saturating_sub(run_start);
            let text_end = end.saturating_sub(run_start).min(run.text.len());
            Some(ClippedTextRun {
                x: span.x + u16::try_from(start - span.source_col).ok()?,
                y: span.y,
                text: &run.text[text_start..text_end],
            })
        })
        .collect()
}

fn clipped_clear_runs(
    run: &DisplayPaneClearRun,
    spans: &[TerminalPaneSpan],
) -> Vec<ClippedClearRun> {
    let run_start = usize::from(run.x);
    let run_end = run_start.saturating_add(run.width);
    spans
        .iter()
        .filter(|span| span.source_row == usize::from(run.y))
        .filter_map(|span| {
            let start = run_start.max(span.source_col);
            let end = run_end.min(span.source_col.saturating_add(usize::from(span.cols)));
            if start >= end {
                return None;
            }
            Some(ClippedClearRun {
                x: span.x + u16::try_from(start - span.source_col).ok()?,
                y: span.y,
                width: end.saturating_sub(start),
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DisplayPaneTarget {
    pub(crate) label: String,
    pub(crate) target: PaneTarget,
    pub(crate) target_string: String,
}

#[cfg(test)]
mod tests {
    #[test]
    fn compact_display_panes_colour_is_foreground_while_large_glyphs_are_background() {
        let mut options = OptionStore::default();
        options
            .set(
                rmux_proto::ScopeSelector::Global,
                OptionName::DisplayPanesActiveColour,
                "red".into(),
                rmux_proto::SetOptionMode::Replace,
            )
            .unwrap();
        let small = crate::session_fixture::SessionFixture::new(
            session_name("small"),
            rmux_proto::TerminalSize { cols: 20, rows: 4 },
        );
        let frame = render_display_panes_overlay(small.view(), &options);
        assert!(frame
            .windows(b"\x1b[31m0".len())
            .any(|part| part == b"\x1b[31m0"));
        let large = crate::session_fixture::SessionFixture::new(
            session_name("large"),
            rmux_proto::TerminalSize { cols: 80, rows: 24 },
        );
        let frame = render_display_panes_overlay(large.view(), &options);
        assert!(frame
            .windows(b"\x1b[41m ".len())
            .any(|part| part == b"\x1b[41m "));
        assert!(!frame
            .windows(b"\x1b[31m".len())
            .any(|part| part == b"\x1b[31m"));
    }

    use super::*;
    use rmux_core::{FloatingPaneRect, OptionStore};
    use rmux_proto::{
        ResizePaneAdjustment, ScopeSelector, SessionName, SetOptionMode, TerminalSize, WindowTarget,
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
    fn source_modes_project_negative_origin_below_top_status() {
        let mut session = crate::session_fixture::SessionFixture::new(
            session_name("source"),
            TerminalSize { cols: 12, rows: 10 },
        );
        session.split_active_pane().expect("split");
        float_pane(
            &mut session,
            1,
            FloatingPaneRect::new(-3, -2, 8, 4).expect("rect"),
        );
        let mut options = OptionStore::new();
        options
            .set(
                ScopeSelector::Session(session.name().clone()),
                OptionName::StatusPosition,
                "top".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("top status");
        let frame = String::from_utf8(render_display_panes_overlay(session.view(), &options))
            .expect("frame");
        assert!(frame.contains("\x1b[2;2H\x1b[0m\x1b[31m1"), "{frame:?}");
        use chrono::TimeZone;
        let now = chrono::Local
            .with_ymd_and_hms(2026, 4, 15, 13, 2, 3)
            .single()
            .expect("time");
        let frame = String::from_utf8(super::super::clock_mode::render_clock_overlay(
            session.view(),
            &options,
            &[super::super::clock_mode::ClockPaneRenderData {
                pane_index: 1,
                history_size: 0,
                alternate_on: false,
            }],
            now,
        ))
        .expect("frame");
        assert!(frame.contains("\x1b[2;1H\x1b[34m3:02"), "{frame:?}");
    }

    #[test]
    fn display_panes_composes_in_source_before_clipping() {
        for (x, y, cols, rows) in [(-3, 0, 8, 4), (-3, 0, 8, 8), (8, 6, 8, 8), (-3, -2, 8, 8)] {
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
            let options = OptionStore::new();
            let specs = display_pane_specs(session.view(), &options);
            let spec = specs
                .iter()
                .find(|spec| spec.label == "1")
                .expect("float spec");
            if rows == 4 {
                let run = &spec.overlay_runs[0];
                let clipped = clipped_text_runs(run, &spec.spans);
                assert_eq!((clipped[0].x, clipped[0].y, clipped[0].text), (1, 2, "1"));
                let clear = clipped_clear_runs(&spec.clear_runs[0], &spec.spans);
                assert_eq!((clear[0].x, clear[0].y, clear[0].width), (1, 2, 1));
            } else {
                let size = spec
                    .overlay_runs
                    .iter()
                    .find(|run| run.text == "8x8")
                    .expect("source dimensions");
                assert_eq!((size.x, size.y), (5, 0));
                let sizes = clipped_text_runs(size, &spec.spans);
                if x < 0 && y == 0 {
                    assert_eq!((sizes[0].x, sizes[0].text), (2, "8x8"));
                } else {
                    assert!(sizes.is_empty(), "source size text is outside clip");
                }
                assert_eq!((spec.overlay_runs[0].x, spec.overlay_runs[0].y), (5, 2));
            }
            for (run, clear) in spec.overlay_runs.iter().zip(&spec.clear_runs) {
                let text = clipped_text_runs(run, &spec.spans);
                let erased = clipped_clear_runs(clear, &spec.spans);
                assert_eq!(
                    text.iter()
                        .map(|r| (r.x, r.y, r.text.len()))
                        .collect::<Vec<_>>(),
                    erased
                        .iter()
                        .map(|r| (r.x, r.y, r.width))
                        .collect::<Vec<_>>()
                );
            }
        }
    }

    #[test]
    fn display_panes_overlay_places_labels_at_pane_centers() {
        let mut session = crate::session_fixture::SessionFixture::new(
            session_name("alpha"),
            TerminalSize { cols: 8, rows: 4 },
        );
        // Match the canonical content layout after reserving the bottom status row.
        session.resize_active_window_geometry(
            TerminalSize { cols: 8, rows: 4 },
            TerminalSize { cols: 8, rows: 3 },
        );
        session.split_active_pane().expect("split succeeds");

        let options = OptionStore::new();
        let frame = render_display_panes_overlay(session.view(), &options);
        let frame = String::from_utf8(frame).expect("overlay is utf-8");

        assert!(frame.contains("\u{1b}[2;3H\u{1b}[0m\u{1b}[34m0"));
        assert!(frame.contains("\u{1b}[2;7H\u{1b}[0m\u{1b}[31m1"));
    }

    #[test]
    fn display_panes_overlay_uses_tmux_big_digits_for_large_panes() {
        let mut session = crate::session_fixture::SessionFixture::new(
            session_name("alpha"),
            TerminalSize { cols: 32, rows: 8 },
        );
        // Match the canonical content layout after reserving the bottom status row.
        session.resize_active_window_geometry(
            TerminalSize { cols: 32, rows: 8 },
            TerminalSize { cols: 32, rows: 7 },
        );
        session.split_active_pane().expect("split succeeds");

        let options = OptionStore::new();
        let frame = String::from_utf8(render_display_panes_overlay(session.view(), &options))
            .expect("overlay is utf-8");

        assert!(frame.contains("\u{1b}[44m     "));
        assert!(frame.contains("\u{1b}[41m "));
        assert!(frame.contains("15x7") || frame.contains("16x7"));
        assert!(!frame.contains("\u{1b}[7m"));
    }

    #[test]
    fn display_panes_clear_uses_the_daemon_status_job_runtime() {
        let session = crate::session_fixture::SessionFixture::new(
            session_name("alpha"),
            TerminalSize { cols: 32, rows: 8 },
        );
        let mut state = HandlerState::default();
        let marker = "dp-owned";
        state
            .options
            .set(
                ScopeSelector::Session(session.name().clone()),
                OptionName::StatusLeft,
                format!("#(echo {marker})"),
                SetOptionMode::Replace,
            )
            .expect("status-left set succeeds");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let frame = render_display_panes_clear(session.view(), &state.options, &state);
            if String::from_utf8_lossy(&frame).contains(marker) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "display-panes clear never rendered its daemon-owned status job"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn zoomed_display_panes_overlay_labels_only_the_active_pane() {
        let mut session = crate::session_fixture::SessionFixture::new(
            session_name("alpha"),
            TerminalSize { cols: 8, rows: 4 },
        );
        session.split_active_pane().expect("split succeeds");
        session
            .resize_pane(1, ResizePaneAdjustment::Zoom)
            .expect("zoom succeeds");

        let options = OptionStore::new();
        let frame = render_display_panes_overlay(session.view(), &options);
        let frame = String::from_utf8(frame).expect("overlay is utf-8");

        assert!(frame.contains("\u{1b}[2;5H\u{1b}[0m\u{1b}[31m1"));
        assert!(!frame.contains("\u{1b}[2;3H\u{1b}[0m\u{1b}[34m0"));
    }

    #[test]
    fn display_panes_overlay_clips_labels_occluded_by_front_floating_pane() {
        let mut session = crate::session_fixture::SessionFixture::new(
            session_name("alpha"),
            TerminalSize { cols: 10, rows: 4 },
        );
        session.split_active_pane().expect("split succeeds");
        float_pane(
            &mut session,
            1,
            FloatingPaneRect::new(0, 0, 5, 2).expect("valid floating rect"),
        );

        let frame = String::from_utf8(render_display_panes_overlay(
            session.view(),
            &OptionStore::new(),
        ))
        .expect("overlay is utf-8");

        assert!(
            !frame.contains("\u{1b}[2;6H\u{1b}[0m\u{1b}[34m0"),
            "rear label must be clipped by the front floating frame: {frame:?}"
        );
        assert!(
            frame.contains("\u{1b}[2;3H\u{1b}[0m\u{1b}[31m1"),
            "{frame:?}"
        );
    }

    #[test]
    fn zoomed_windows_do_not_render_borders() {
        let mut session = crate::session_fixture::SessionFixture::new(
            session_name("alpha"),
            TerminalSize { cols: 8, rows: 4 },
        );
        session.split_active_pane().expect("split succeeds");
        session
            .resize_pane(1, ResizePaneAdjustment::Zoom)
            .expect("zoom succeeds");

        assert!(super::super::render(session.view(), &OptionStore::new()).contains(&b'['));
    }

    #[test]
    fn display_panes_label_count_matches_renderable_overlay_labels() {
        let mut session = crate::session_fixture::SessionFixture::new(
            session_name("alpha"),
            TerminalSize { cols: 3, rows: 4 },
        );
        session.split_active_pane().expect("split succeeds");
        session.resize_terminal(TerminalSize { cols: 3, rows: 1 });

        let options = OptionStore::new();
        assert!(render_display_panes_overlay(session.view(), &options).is_empty());
        assert_eq!(display_panes_label_count(session.view(), &options), 0);
    }

    #[test]
    fn display_panes_uses_window_local_pane_base_index_for_labels() {
        let mut session = crate::session_fixture::SessionFixture::new(
            session_name("alpha"),
            TerminalSize { cols: 12, rows: 4 },
        );
        session.split_active_pane().expect("split succeeds");
        let mut options = OptionStore::new();
        options
            .set(
                ScopeSelector::Window(WindowTarget::with_window(session.name().clone(), 0)),
                OptionName::PaneBaseIndex,
                "10".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("pane-base-index set succeeds");

        let targets = display_pane_targets(session.view(), &options);

        assert_eq!(
            targets
                .iter()
                .map(|target| target.label.as_str())
                .collect::<Vec<_>>(),
            vec!["10", "a", "11", "b"]
        );
        assert_eq!(targets[0].target.pane_index(), 0);
        assert_eq!(targets[1].target.pane_index(), 0);
        assert_eq!(targets[2].target.pane_index(), 1);
        assert_eq!(targets[3].target.pane_index(), 1);
    }
}
