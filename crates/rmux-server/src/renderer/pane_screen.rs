use std::borrow::Cow;

use rmux_core::input::mode;
use rmux_core::style::{Style, StyleCell};
use rmux_core::{
    formats::FormatContext, text_width as tmux_text_width,
    truncate_to_width as tmux_truncate_to_width, GridRenderOptions, OptionStore, Pane, Screen,
    ScreenCaptureRange, Session, Utf8Config,
};
use rmux_proto::OptionName;

use crate::copy_mode::{CopyModeOverlayRange, CopyModeRenderOverlays, CopyModeRenderSnapshot};
use crate::format_runtime::RuntimeFormatContext;

use super::copy_mode_line_numbers::CopyModeLineNumberRenderer;
use super::pane_scrollbar::{resolve_pane_scrollbar, PaneScrollbarRenderContext};
use super::{cursor_position_bytes, visible_pane_geometry, StatusGeometry};

const OSC8_CLOSE: &[u8] = b"\x1b]8;;\x1b\\";

pub(crate) fn render_pane_screen(
    session: &Session,
    options: &OptionStore,
    pane: &Pane,
    screen: &Screen,
) -> Vec<u8> {
    render_pane_screen_with_cursor_restore(
        session,
        options,
        pane,
        screen,
        PaneScreenCursorRestore::Pane,
        None,
    )
}

pub(crate) fn render_pane_screen_preserving_prompt_cursor(
    session: &Session,
    options: &OptionStore,
    pane: &Pane,
    screen: &Screen,
) -> Vec<u8> {
    render_pane_screen_with_cursor_restore(
        session,
        options,
        pane,
        screen,
        PaneScreenCursorRestore::Prompt,
        None,
    )
}

pub(crate) fn render_copy_mode_pane_screen(
    session: &Session,
    options: &OptionStore,
    pane: &Pane,
    snapshot: &CopyModeRenderSnapshot,
) -> Vec<u8> {
    render_pane_screen_with_cursor_restore(
        session,
        options,
        pane,
        &snapshot.screen,
        PaneScreenCursorRestore::Pane,
        Some(snapshot),
    )
}

pub(crate) fn render_copy_mode_pane_screen_preserving_prompt_cursor(
    session: &Session,
    options: &OptionStore,
    pane: &Pane,
    snapshot: &CopyModeRenderSnapshot,
) -> Vec<u8> {
    render_pane_screen_with_cursor_restore(
        session,
        options,
        pane,
        &snapshot.screen,
        PaneScreenCursorRestore::Prompt,
        Some(snapshot),
    )
}

#[derive(Clone, Copy)]
enum PaneScreenCursorRestore {
    Pane,
    Prompt,
}

fn render_pane_screen_with_cursor_restore(
    session: &Session,
    options: &OptionStore,
    pane: &Pane,
    screen: &Screen,
    cursor_restore: PaneScreenCursorRestore,
    copy_mode_snapshot: Option<&CopyModeRenderSnapshot>,
) -> Vec<u8> {
    let _render_span = crate::perf_instrument::span("render_compose")
        .with_str("site", "pane_screen")
        .with_u64("pane_id", u64::from(pane.id().as_u32()))
        .with_usize("history_size", screen.history_size());
    let geometry = StatusGeometry::for_session(session, options);
    let Some(raw_pane_geometry) =
        visible_pane_geometry(session, options, pane, geometry.content_rows)
    else {
        return Vec::new();
    };
    let copy_mode_overlays = copy_mode_snapshot.map(|snapshot| &snapshot.overlays);
    let (history_size, alternate_on, copy_mode_scroll_position) = copy_mode_snapshot.map_or(
        (screen.history_size(), screen.is_alternate(), None),
        |snapshot| {
            (
                snapshot.history_size,
                snapshot.alternate_on,
                Some(snapshot.scroll_position),
            )
        },
    );
    let (pane_geometry, scrollbar) = resolve_pane_scrollbar(
        session,
        options,
        pane,
        PaneScrollbarRenderContext {
            geometry: raw_pane_geometry,
            history_size,
            alternate_on,
            copy_mode_scroll_position,
            content_y_offset: geometry.content_y_offset,
        },
    );
    if pane_geometry.cols() == 0 || pane_geometry.rows() == 0 {
        return Vec::new();
    }
    let line_numbers = copy_mode_snapshot
        .and_then(|snapshot| CopyModeLineNumberRenderer::resolve(session, options, pane, snapshot));
    let line_number_layout = line_numbers
        .as_ref()
        .map(CopyModeLineNumberRenderer::layout);
    let physical_line_number_width =
        line_number_layout.map_or(0, |layout| layout.physical_width(pane_geometry.cols()));
    let rendered_content_width = pane_geometry
        .cols()
        .saturating_sub(physical_line_number_width);

    let sparse_full_width_clear = pane_geometry.x() == 0
        && pane_geometry.cols() == session.terminal_size().cols
        && pane_default_style(session, options, pane).is_none()
        && copy_mode_overlays.is_none();
    let styled_screen = copy_mode_overlays.map_or_else(
        || styled_pane_screen(session, options, pane, screen),
        |overlays| {
            Cow::Owned(styled_copy_mode_pane_screen(
                session, options, pane, screen, overlays,
            ))
        },
    );
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
    frame.extend_from_slice(b"\x1b[s\x1b[?25l\x1b[0m");
    for row in 0..usize::from(pane_geometry.rows()) {
        let line = rendered_lines.get(row).copied().unwrap_or_default();
        let line = truncate_rendered_pane_line(line, usize::from(rendered_content_width), &utf8);
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
        frame.extend_from_slice(b"\x1b[0m");
        if let Some(line_numbers) = line_numbers.as_ref() {
            line_numbers.append_prefix(&mut frame, row, pane_geometry.cols());
        }
        frame.extend_from_slice(&line);
        if line_number_layout.is_some_and(|layout| {
            row == usize::try_from(screen.cursor_position().1).unwrap_or(usize::MAX)
                && screen.cursor_position().0
                    >= u32::from(layout.content_width(pane_geometry.cols()))
        }) {
            frame.extend_from_slice(
                cursor_position_bytes(
                    pane_geometry
                        .y()
                        .saturating_add(geometry.content_y_offset)
                        .saturating_add(row as u16),
                    pane_geometry
                        .x()
                        .saturating_add(pane_geometry.cols().saturating_sub(1)),
                )
                .as_slice(),
            );
            frame.extend_from_slice(b"\x1b[0m$\x1b[0m");
        }
        if sparse_full_width_clear {
            if !line.is_empty() {
                frame.extend_from_slice(b"\x1b[0m");
            }
            frame.extend_from_slice(b"\x1b[K");
        }
    }
    if let Some(scrollbar) = scrollbar {
        frame.extend_from_slice(scrollbar.frame().as_slice());
    }
    frame.extend_from_slice(b"\x1b[0m\x1b[u");
    match cursor_restore {
        PaneScreenCursorRestore::Pane => frame.extend_from_slice(
            final_pane_cursor_state(
                screen,
                pane_geometry,
                geometry.content_y_offset,
                line_number_layout,
            )
            .as_slice(),
        ),
        PaneScreenCursorRestore::Prompt => frame.extend_from_slice(b"\x1b[?25h"),
    }
    frame
}

fn final_pane_cursor_state(
    screen: &Screen,
    pane_geometry: rmux_core::PaneGeometry,
    content_y_offset: u16,
    line_numbers: Option<crate::copy_mode::CopyModeLineNumberLayout>,
) -> Vec<u8> {
    let (cursor_x, cursor_y) = screen.cursor_position();
    let cursor_x = line_numbers.map_or_else(
        || cursor_x.min(u32::from(pane_geometry.cols().saturating_sub(1))) as u16,
        |layout| layout.cursor_x(pane_geometry.cols(), cursor_x),
    );
    let x = pane_geometry.x().saturating_add(cursor_x);
    let y = pane_geometry
        .y()
        .saturating_add(content_y_offset)
        .saturating_add(cursor_y.min(u32::from(pane_geometry.rows().saturating_sub(1))) as u16);
    let mut frame = cursor_position_bytes(y, x);
    if screen.mode() & mode::MODE_CURSOR == 0 {
        frame.extend_from_slice(b"\x1b[?25l");
    } else {
        frame.extend_from_slice(b"\x1b[?25h");
    }
    frame
}

/// Expanded `copy-mode-selection-style` for the pane, resolved through the
/// option default (`#{E:mode-style}`) and the runtime format context — the
/// same machinery as the copy-mode position indicator. Passing the raw
/// option value into the cell style parser silently dropped the default
/// template, leaving selections visually unhighlighted (issue #90).
pub(crate) fn pane_selection_overlay_style(
    session: &Session,
    options: &OptionStore,
    pane: &Pane,
) -> Option<String> {
    pane_cell_overlay_style(session, options, pane, OptionName::CopyModeSelectionStyle)
        .map(|style| rmux_core::style_tostring(&style))
}

pub(super) fn pane_cell_overlay_style(
    session: &Session,
    options: &OptionStore,
    pane: &Pane,
    option: OptionName,
) -> Option<Style> {
    let value = options.resolve_for_pane(
        session.name(),
        session.active_window_index(),
        pane.index(),
        option,
    )?;
    let context = FormatContext::from_session(session)
        .with_window(session.active_window_index(), session.window(), true, false)
        .with_window_pane(session.window(), pane);
    let runtime = RuntimeFormatContext::new(context)
        .with_options(options)
        .with_session(session)
        .with_window(session.active_window_index(), session.window())
        .with_pane(pane);
    let style = super::apply_runtime_style_overlay(&Style::default(), Some(value), &runtime);
    let rendered = rmux_core::style_tostring(&style);
    (!rendered.is_empty() && rendered != "default").then_some(style)
}

pub(crate) fn styled_copy_mode_pane_screen(
    session: &Session,
    options: &OptionStore,
    pane: &Pane,
    screen: &Screen,
    overlays: &CopyModeRenderOverlays,
) -> Screen {
    let mut styled_screen = screen.clone();
    if let Some(style) = pane_default_style(session, options, pane) {
        styled_screen.overlay_default_style(&style);
    }
    if let (Some(range), Some(style)) = (
        overlays.mark,
        pane_cell_overlay_style(session, options, pane, OptionName::CopyModeMarkStyle),
    ) {
        overlay_copy_mode_range(&mut styled_screen, range, &style);
    }
    if let Some(style) =
        pane_cell_overlay_style(session, options, pane, OptionName::CopyModeMatchStyle)
    {
        for range in &overlays.matches {
            overlay_copy_mode_range(&mut styled_screen, *range, &style);
        }
    }
    if let (Some(range), Some(style)) = (
        overlays.current_match,
        pane_cell_overlay_style(
            session,
            options,
            pane,
            OptionName::CopyModeCurrentMatchStyle,
        ),
    ) {
        overlay_copy_mode_range(&mut styled_screen, range, &style);
    }
    if styled_screen.has_selected_cells() {
        if let Some(style) = pane_selection_overlay_style(session, options, pane) {
            styled_screen.overlay_style_on_selected(&style);
        }
    }
    styled_screen
}

fn overlay_copy_mode_range(screen: &mut Screen, range: CopyModeOverlayRange, style: &Style) {
    screen.overlay_style_on_row_range(range.row, range.start_x, range.end_x, style);
}

pub(crate) fn styled_pane_screen<'a>(
    session: &Session,
    options: &OptionStore,
    pane: &Pane,
    screen: &'a Screen,
) -> Cow<'a, Screen> {
    let default_style = pane_default_style(session, options, pane);
    let selection_style = screen
        .has_selected_cells()
        .then(|| pane_selection_overlay_style(session, options, pane))
        .flatten();
    if default_style.is_none() && selection_style.is_none() {
        return Cow::Borrowed(screen);
    }

    let mut styled_screen = screen.clone();
    if let Some(style) = default_style {
        styled_screen.overlay_default_style(&style);
    }
    if let Some(style) = selection_style {
        styled_screen.overlay_style_on_selected(&style);
    }
    Cow::Owned(styled_screen)
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
    if line.is_ascii() {
        return truncate_rendered_ascii_pane_line(line, width, utf8);
    }

    let Ok(line_text) = std::str::from_utf8(line) else {
        return Vec::new();
    };
    let mut visible_text = String::with_capacity(line.len());
    let mut index = 0_usize;
    while index < line.len() {
        if line[index] == 0x1b {
            index = ansi_sequence_end(line, index);
            continue;
        }

        let Some(ch) = line_text[index..].chars().next() else {
            break;
        };
        visible_text.push(ch);
        index += ch.len_utf8();
    }
    let visible_prefix_len = tmux_truncate_to_width(&visible_text, width, utf8).len();

    let mut output = Vec::with_capacity(line.len().min(width.saturating_mul(4)));
    let mut visible_copied = 0_usize;
    let mut hyperlink_active = false;
    index = 0;
    while index < line.len() {
        if line[index] == 0x1b {
            let end = ansi_sequence_end(line, index);
            let sequence = &line[index..end];
            update_osc8_state(sequence, &mut hyperlink_active);
            output.extend_from_slice(sequence);
            index = end;
            continue;
        }

        let Some(ch) = line_text[index..].chars().next() else {
            break;
        };
        let ch_len = ch.len_utf8();
        if visible_copied.saturating_add(ch_len) > visible_prefix_len {
            break;
        }
        output.extend_from_slice(&line[index..index + ch_len]);
        visible_copied += ch_len;
        index += ch_len;
    }
    if hyperlink_active {
        output.extend_from_slice(OSC8_CLOSE);
    }
    output
}

fn truncate_rendered_ascii_pane_line(line: &[u8], width: usize, utf8: &Utf8Config) -> Vec<u8> {
    let mut output = Vec::with_capacity(line.len().min(width.saturating_mul(4)));
    let mut used = 0_usize;
    let mut index = 0_usize;
    let mut hyperlink_active = false;
    while index < line.len() {
        if line[index] == 0x1b {
            let end = ansi_sequence_end(line, index);
            let sequence = &line[index..end];
            update_osc8_state(sequence, &mut hyperlink_active);
            output.extend_from_slice(sequence);
            index = end;
            continue;
        }

        let mut buf = [0_u8; 4];
        let text = char::from(line[index]).encode_utf8(&mut buf);
        let cell_width = tmux_text_width(text, utf8);
        if cell_width != 0 && used.saturating_add(cell_width) > width {
            break;
        }
        output.push(line[index]);
        used = used.saturating_add(cell_width);
        index += 1;
    }
    if hyperlink_active {
        output.extend_from_slice(OSC8_CLOSE);
    }
    output
}

fn update_osc8_state(sequence: &[u8], active: &mut bool) {
    if !sequence.starts_with(b"\x1b]8;") {
        return;
    }
    let body_end = if sequence.ends_with(b"\x07") {
        sequence.len().saturating_sub(1)
    } else if sequence.ends_with(b"\x1b\\") {
        sequence.len().saturating_sub(2)
    } else {
        return;
    };
    let Some(separator) = sequence[4..body_end]
        .iter()
        .position(|byte| *byte == b';')
        .map(|offset| offset + 4)
    else {
        return;
    };
    *active = separator.saturating_add(1) < body_end;
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
