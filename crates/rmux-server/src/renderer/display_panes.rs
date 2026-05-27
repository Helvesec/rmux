use rmux_core::{OptionStore, Pane, PaneGeometry, Session, Window};
use rmux_proto::{OptionName, PaneTarget};

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

pub(crate) fn render_display_panes_overlay(session: &Session, options: &OptionStore) -> Vec<u8> {
    let specs = display_pane_specs(session, options);
    if specs.is_empty() {
        return Vec::new();
    }

    let mut frame = Vec::new();
    frame.extend_from_slice(b"\x1b[s\x1b[?25l");
    for spec in &specs {
        for run in &spec.overlay_runs {
            frame.extend_from_slice(super::cursor_position_bytes(run.y, run.x).as_slice());
            frame.extend_from_slice(b"\x1b[0m");
            frame.extend_from_slice(run.sgr.as_slice());
            frame.extend_from_slice(run.text.as_bytes());
        }
    }
    frame.extend_from_slice(b"\x1b[0m\x1b[u");
    frame
}

pub(crate) fn render_display_panes_clear(session: &Session, options: &OptionStore) -> Vec<u8> {
    render_display_panes_clear_with_base(session, options, &super::render(session, options))
}

pub(crate) fn render_display_panes_clear_with_base(
    session: &Session,
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
            frame.extend_from_slice(super::cursor_position_bytes(run.y, run.x).as_slice());
            frame.resize(frame.len() + run.width, b' ');
        }
    }
    frame.extend_from_slice(b"\x1b[0m\x1b[?25h\x1b[u");
    frame.extend_from_slice(base_frame);
    frame
}

pub(crate) fn display_panes_label_count(session: &Session, options: &OptionStore) -> u32 {
    u32::try_from(display_pane_specs(session, options).len()).unwrap_or(u32::MAX)
}

pub(crate) fn display_pane_targets(
    session: &Session,
    options: &OptionStore,
) -> Vec<DisplayPaneTarget> {
    display_pane_specs(session, options)
        .into_iter()
        .map(|spec| DisplayPaneTarget {
            label: spec.label,
            target: spec.target,
            target_string: spec.target_string,
        })
        .collect()
}

fn display_pane_specs(session: &Session, options: &OptionStore) -> Vec<DisplayPaneSpec> {
    let window = session.window();
    let active_pane_index = window.active_pane_index();
    let inactive_colour =
        display_panes_colour(options.resolve(Some(session.name()), OptionName::DisplayPanesColour));
    let active_colour = display_panes_colour(
        options.resolve(Some(session.name()), OptionName::DisplayPanesActiveColour),
    );
    let base_index = options
        .resolve(Some(session.name()), 
            OptionName::PaneBaseIndex)
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);

    display_panes(window)
        .into_iter()
        .filter_map(|pane| {
            let geometry = visible_pane_geometry(session, options, pane)?;
            let label = pane.index().saturating_add(base_index).to_string();
            let label_width = u16::try_from(label.len()).ok()?;
            if geometry.cols() < label_width || geometry.rows() == 0 {
                return None;
            }

            let alias = pane_alias(pane.index());
            let is_active = pane.index() == active_pane_index;
            let colour = if is_active {
                active_colour.as_slice()
            } else {
                inactive_colour.as_slice()
            };
            let mut overlay_runs = Vec::new();
            let mut clear_runs = Vec::new();

            if geometry.cols() < label_width.saturating_mul(6) || geometry.rows() < 5 {
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
                        colour,
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

                    if let Some(alias) = alias {
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
                target: PaneTarget::with_window(
                    session.name().clone(),
                    session.active_window_index(),
                    pane.index(),
                ),
                target_string: format!(
                    "={}:{}.%{}",
                    session.name(),
                    session.active_window_index(),
                    pane.id().as_u32()
                ),
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
        return Some(PaneGeometry::new(
            0,
            geometry.content_y_offset,
            size.cols,
            size.rows,
        ));
    }

    let pane = super::content_pane_geometry(pane, geometry.content_rows);
    Some(PaneGeometry::new(
        pane.x(),
        pane.y().saturating_add(geometry.content_y_offset),
        pane.cols(),
        pane.rows(),
    ))
}

fn pane_alias(index: u32) -> Option<String> {
    (10..35).contains(&index).then(|| {
        char::from_u32(u32::from(b'a') + index - 10)
            .unwrap_or('a')
            .to_string()
    })
}

fn display_panes_colour(value: Option<&str>) -> Vec<u8> {
    let Some(colour) = super::parse_option_colour(value) else {
        return Vec::new();
    };
    let Some(parameter) = super::background_sgr_parameter(colour) else {
        return Vec::new();
    };
    format!("\x1b[{parameter}m").into_bytes()
}

fn display_panes(window: &Window) -> Vec<&Pane> {
    if window.is_zoomed() {
        window.active_pane().into_iter().collect()
    } else {
        window.panes().iter().collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DisplayPaneSpec {
    label: String,
    target: PaneTarget,
    target_string: String,
    overlay_runs: Vec<DisplayPaneRun>,
    clear_runs: Vec<DisplayPaneClearRun>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DisplayPaneTarget {
    pub(crate) label: String,
    pub(crate) target: PaneTarget,
    pub(crate) target_string: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_core::{OptionStore, Session};
    use rmux_proto::{ResizePaneAdjustment, SessionName, TerminalSize, ScopeSelector, SetOptionMode};

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid session name")
    }

    #[test]
    fn display_panes_overlay_places_labels_at_pane_centers() {
        let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 8, rows: 4 });
        session.split_active_pane().expect("split succeeds");

        let options = OptionStore::new();
        let frame = render_display_panes_overlay(&session, &options);
        let frame = String::from_utf8(frame).expect("overlay is utf-8");

        assert!(frame.contains("\u{1b}[2;3H\u{1b}[0m\u{1b}[44m0"));
        assert!(frame.contains("\u{1b}[2;7H\u{1b}[0m\u{1b}[41m1"));
    }

    #[test]
    fn display_panes_overlay_uses_tmux_big_digits_for_large_panes() {
        let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 32, rows: 8 });
        session.split_active_pane().expect("split succeeds");

        let options = OptionStore::new();
        let frame = String::from_utf8(render_display_panes_overlay(&session, &options))
            .expect("overlay is utf-8");

        assert!(frame.contains("\u{1b}[44m     "));
        assert!(frame.contains("\u{1b}[41m "));
        assert!(frame.contains("15x7") || frame.contains("16x7"));
        assert!(!frame.contains("\u{1b}[7m"));
    }

    #[test]
    fn zoomed_display_panes_overlay_labels_only_the_active_pane() {
        let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 8, rows: 4 });
        session.split_active_pane().expect("split succeeds");
        session
            .resize_pane(1, ResizePaneAdjustment::Zoom)
            .expect("zoom succeeds");

        let options = OptionStore::new();
        let frame = render_display_panes_overlay(&session, &options);
        let frame = String::from_utf8(frame).expect("overlay is utf-8");

        assert!(frame.contains("\u{1b}[2;5H\u{1b}[0m\u{1b}[41m1"));
        assert!(!frame.contains("\u{1b}[2;3H\u{1b}[0m\u{1b}[44m0"));
    }

    #[test]
    fn zoomed_windows_do_not_render_borders() {
        let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 8, rows: 4 });
        session.split_active_pane().expect("split succeeds");
        session
            .resize_pane(1, ResizePaneAdjustment::Zoom)
            .expect("zoom succeeds");

        assert!(super::super::render(&session, &OptionStore::new()).contains(&b'['));
    }

    #[test]
    fn display_panes_label_count_matches_renderable_overlay_labels() {
        let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 3, rows: 4 });
        session.split_active_pane().expect("split succeeds");
        session.resize_terminal(TerminalSize { cols: 3, rows: 1 });

        let options = OptionStore::new();
        assert!(render_display_panes_overlay(&session, &options).is_empty());
        assert_eq!(display_panes_label_count(&session, &options), 0);
    }

    #[test]
    fn display_panes_overlay_respects_pane_base_index() {
        let mut session = Session::new(session_name("alpha"), TerminalSize { cols: 8, rows: 4 }); 
        session.split_active_pane().expect("split succeeds");
        let mut options = OptionStore::new();
        options.set(
                ScopeSelector::Global,
                OptionName::PaneBaseIndex,
                "1".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("set succeeds");

        let frame = String::from_utf8(render_display_panes_overlay(&session, &options)).expect("overlay is utf-8");

        assert!(frame.contains("\u{1b}[2;3H\u{1b}[0m\u{1b}[44m1"));
        assert!(frame.contains("\u{1b}[2;7H\u{1b}[0m\u{1b}[41m2"));
    }
}
