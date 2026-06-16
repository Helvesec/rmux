use rmux_core::style::{Style, StyleCell};
use rmux_core::{
    text_width as tmux_text_width, GridRenderOptions, OptionStore, Pane, Screen,
    ScreenCaptureRange, Session, Utf8Config,
};
use rmux_proto::OptionName;

use super::{cursor_position_bytes, visible_pane_geometry, StatusGeometry};

pub(crate) fn render_pane_screen(
    session: &Session,
    options: &OptionStore,
    pane: &Pane,
    screen: &Screen,
) -> Vec<u8> {
    let geometry = StatusGeometry::for_session(session, options);
    let Some(pane_geometry) = visible_pane_geometry(session, options, pane, geometry.content_rows)
    else {
        return Vec::new();
    };
    if pane_geometry.cols() == 0 || pane_geometry.rows() == 0 {
        return Vec::new();
    }

    let sparse_full_width_clear = pane_geometry.x() == 0
        && pane_geometry.cols() == session.terminal_size().cols
        && pane_default_style(session, options, pane).is_none();
    let styled_screen = styled_pane_screen(session, options, pane, screen);
    let rendered = styled_screen.capture_transcript(
        ScreenCaptureRange::default(),
        GridRenderOptions {
            with_sequences: true,
            include_empty_cells: !sparse_full_width_clear,
            trim_spaces: false,
            ..GridRenderOptions::default()
        },
    );
    let utf8 = Utf8Config::from_options(options);
    let rendered_lines = rendered.split(|byte| *byte == b'\n').collect::<Vec<_>>();
    let mut frame = Vec::with_capacity(
        rendered
            .len()
            .saturating_add(usize::from(pane_geometry.rows()).saturating_mul(20))
            .saturating_add(32),
    );
    frame.extend_from_slice(b"\x1b[s\x1b[0m");
    for row in 0..usize::from(pane_geometry.rows()) {
        let line = rendered_lines.get(row).copied().unwrap_or_default();
        let line = truncate_rendered_pane_line(line, usize::from(pane_geometry.cols()), &utf8);
        frame.extend_from_slice(
            cursor_position_bytes(
                pane_geometry
                    .y()
                    .saturating_add(geometry.content_y_offset)
                    .saturating_add(row as u16),
                pane_geometry.x(),
            )
            .as_slice(),
        );
        frame.extend_from_slice(&line);
        if sparse_full_width_clear {
            frame.extend_from_slice(b"\x1b[0m\x1b[K");
        }
    }
    frame.extend_from_slice(b"\x1b[0m\x1b[u");
    frame
}

pub(crate) fn styled_pane_screen(
    session: &Session,
    options: &OptionStore,
    pane: &Pane,
    screen: &Screen,
) -> Screen {
    let mut styled_screen = screen.clone();
    if let Some(style) = pane_default_style(session, options, pane) {
        styled_screen.overlay_default_style(&style);
    }
    if let Some(style) = options.resolve_for_pane(
        session.name(),
        session.active_window_index(),
        pane.index(),
        OptionName::CopyModeSelectionStyle,
    ) {
        styled_screen.overlay_style_on_selected(style);
    }
    styled_screen
}

pub(crate) fn pane_default_style(
    session: &Session,
    options: &OptionStore,
    pane: &Pane,
) -> Option<Style> {
    let mut style = Style::default();
    let base = StyleCell::default();
    let mut applied = false;
    for option in [OptionName::WindowStyle, OptionName::WindowActiveStyle] {
        if option == OptionName::WindowActiveStyle && pane.index() != session.active_pane_index() {
            continue;
        }
        let Some(value) = options.resolve_for_pane(
            session.name(),
            session.active_window_index(),
            pane.index(),
            option,
        ) else {
            continue;
        };
        if value.is_empty() || value == "default" {
            continue;
        }
        if style.parse_in_place(&base, value).is_ok() {
            applied = true;
        }
    }
    applied.then_some(style)
}

pub(crate) fn truncate_rendered_pane_line(line: &[u8], width: usize, utf8: &Utf8Config) -> Vec<u8> {
    if width == 0 {
        return Vec::new();
    }

    let mut output = Vec::with_capacity(line.len().min(width.saturating_mul(4)));
    let mut used = 0_usize;
    let mut index = 0_usize;
    while index < line.len() {
        if line[index] == 0x1b {
            let end = ansi_sequence_end(line, index);
            output.extend_from_slice(&line[index..end]);
            index = end;
            continue;
        }

        let Ok(rest) = std::str::from_utf8(&line[index..]) else {
            break;
        };
        let Some(ch) = rest.chars().next() else {
            break;
        };
        let ch_len = ch.len_utf8();
        let mut buf = [0_u8; 4];
        let text = ch.encode_utf8(&mut buf);
        let cell_width = tmux_text_width(text, utf8);
        if cell_width != 0 && used.saturating_add(cell_width) > width {
            break;
        }
        output.extend_from_slice(&line[index..index + ch_len]);
        used = used.saturating_add(cell_width);
        index += ch_len;
    }
    output
}

fn ansi_sequence_end(line: &[u8], start: usize) -> usize {
    let Some(&kind) = line.get(start.saturating_add(1)) else {
        return line.len();
    };
    match kind {
        b'[' => line[start + 2..]
            .iter()
            .position(|byte| (0x40..=0x7e).contains(byte))
            .map_or(line.len(), |offset| start + 3 + offset),
        b']' => osc_sequence_end(line, start),
        _ => start.saturating_add(2).min(line.len()),
    }
}

fn osc_sequence_end(line: &[u8], start: usize) -> usize {
    let mut index = start.saturating_add(2);
    while index < line.len() {
        match line[index] {
            0x07 => return index + 1,
            0x1b if line.get(index + 1) == Some(&b'\\') => return index + 2,
            _ => index += 1,
        }
    }
    line.len()
}
