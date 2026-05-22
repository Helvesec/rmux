use rmux_core::{
    style::Style, text_width as tmux_text_width, truncate_to_width as tmux_truncate_to_width,
    Utf8Config,
};

use super::super::{cursor_position_bytes, style_sgr_bytes};

pub(in crate::renderer::status) fn render_status_runs(row: u16, runs: &[StatusRun]) -> Vec<u8> {
    if runs.is_empty() {
        return Vec::new();
    }

    let mut frame = Vec::new();
    let mut last_style: Option<StatusStyle> = None;
    frame.extend_from_slice(b"\x1b[s");
    frame.extend_from_slice(b"\x1b[0m");
    frame.extend_from_slice(cursor_position_bytes(row, 0).as_slice());

    for run in runs {
        if run.text.is_empty() {
            continue;
        }
        if last_style.as_ref() != Some(&run.style) {
            if last_style.is_some() {
                frame.extend_from_slice(b"\x1b[0m");
            }
            frame.extend_from_slice(style_sgr_bytes(&run.style, true).as_slice());
            last_style = Some(run.style.clone());
        }
        frame.extend_from_slice(run.text.as_bytes());
    }

    frame.extend_from_slice(b"\x1b[0m\x1b[u");
    frame
}

#[cfg_attr(not(test), allow(dead_code))]
pub(in crate::renderer::status) fn truncate_status_runs(
    runs: &[StatusRun],
    width: usize,
    utf8_config: &Utf8Config,
) -> Vec<StatusRun> {
    let mut truncated = Vec::new();
    let mut remaining = width;

    for run in runs {
        if remaining == 0 {
            break;
        }
        let text = tmux_truncate_to_width(&run.text, remaining, utf8_config);
        remaining = remaining.saturating_sub(tmux_text_width(&text, utf8_config));
        push_status_run(&mut truncated, text, run.style.clone());
    }

    truncated
}

pub(in crate::renderer::status) fn push_spaces(
    runs: &mut Vec<StatusRun>,
    count: usize,
    style: StatusStyle,
) {
    if count > 0 {
        push_status_run(runs, " ".repeat(count), style);
    }
}

pub(in crate::renderer::status) fn push_status_run(
    runs: &mut Vec<StatusRun>,
    text: String,
    style: StatusStyle,
) {
    let text = sanitize_status_text(text);
    if text.is_empty() {
        return;
    }
    if let Some(last) = runs.last_mut()
        && last.style == style {
            last.text.push_str(&text);
            return;
        }
    runs.push(StatusRun { text, style });
}

pub(in crate::renderer) fn sanitize_status_text(text: String) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

pub(in crate::renderer) fn status_runs_width(
    runs: &[StatusRun],
    utf8_config: &Utf8Config,
) -> usize {
    runs.iter()
        .map(|run| tmux_text_width(&run.text, utf8_config))
        .sum()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::renderer) struct StatusRun {
    pub(in crate::renderer) text: String,
    pub(in crate::renderer) style: StatusStyle,
}

pub(in crate::renderer) type StatusStyle = Style;
