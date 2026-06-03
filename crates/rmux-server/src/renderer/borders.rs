use std::collections::{btree_map::Entry, BTreeMap};

use rmux_core::formats::FormatContext;
use rmux_core::style::Style;
use rmux_core::{OptionStore, Pane, Session, Window};
use rmux_proto::OptionName;

use crate::format_runtime::RuntimeFormatContext;

use super::{apply_runtime_style_overlay, cursor_position_bytes, style_sgr_bytes, StatusGeometry};

#[path = "borders/layout.rs"]
mod layout;
#[path = "borders/status.rs"]
mod status;

use self::layout::border_layout_cells_with_geometry;
pub(super) use self::layout::content_pane_geometry;
pub(super) use self::status::render_pane_border_status_lines;

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn border_cells(
    window: &Window,
    active_pane_index: u32,
    inactive_style: BorderStyle,
    active_style: BorderStyle,
) -> Vec<BorderCell> {
    border_cells_with_geometry(
        window,
        active_pane_index,
        inactive_style,
        active_style,
        StatusGeometry::without_status(window.size()),
    )
}

fn border_cells_with_geometry(
    window: &Window,
    active_pane_index: u32,
    inactive_style: BorderStyle,
    active_style: BorderStyle,
    geometry: StatusGeometry,
) -> Vec<BorderCell> {
    border_layout_cells_with_geometry(window, active_pane_index, geometry, true)
        .into_iter()
        .map(|cell| BorderCell {
            x: cell.x,
            y: cell.y,
            glyph: cell.glyph,
            style: if cell.active {
                active_style.clone()
            } else {
                inactive_style.clone()
            },
        })
        .collect()
}

pub(super) fn runtime_border_cells(
    session: &Session,
    options: &OptionStore,
    geometry: StatusGeometry,
) -> Vec<BorderCell> {
    let window = session.window();
    let window_index = session.active_window_index();
    let indicators_colour = pane_border_indicators_colour_enabled(options.resolve_for_window(
        session.name(),
        window_index,
        OptionName::PaneBorderIndicators,
    ));
    let layout_cells = border_layout_cells_with_geometry(
        window,
        session.active_pane_index(),
        geometry,
        indicators_colour,
    );
    let line_style = PaneBorderLineStyle::from_option(options.resolve_for_window(
        session.name(),
        window_index,
        OptionName::PaneBorderLines,
    ));
    let mut style_cache = BTreeMap::<(u32, bool), BorderStyle>::new();

    layout_cells
        .into_iter()
        .map(|cell| {
            let style = cell
                .owner_pane_index
                .and_then(
                    |pane_index| match style_cache.entry((pane_index, cell.active)) {
                        Entry::Occupied(entry) => Some(entry.get().clone()),
                        Entry::Vacant(entry) => {
                            let pane = window.pane(pane_index)?;
                            Some(
                                entry
                                    .insert(resolve_border_style_for_pane(
                                        session,
                                        options,
                                        window_index,
                                        pane,
                                        cell.active,
                                    ))
                                    .clone(),
                            )
                        }
                    },
                )
                .unwrap_or_default();
            BorderCell {
                x: cell.x,
                y: cell.y,
                glyph: line_style.map_glyph(cell.glyph),
                style,
            }
        })
        .collect()
}

fn pane_border_indicators_colour_enabled(value: Option<&str>) -> bool {
    matches!(value, Some("colour" | "both"))
}

pub(super) fn render_cells(cells: &[BorderCell]) -> Vec<u8> {
    if cells.is_empty() {
        return Vec::new();
    }

    let mut frame = Vec::new();
    let mut last_style: Option<BorderStyle> = None;
    frame.extend_from_slice(b"\x1b[s");
    frame.extend_from_slice(b"\x1b[0m");

    for cell in cells {
        frame.extend_from_slice(cursor_position_bytes(cell.y, cell.x).as_slice());
        if last_style.as_ref() != Some(&cell.style) {
            if last_style.is_some() {
                frame.extend_from_slice(b"\x1b[0m");
            }
            frame.extend_from_slice(style_sgr_bytes(&cell.style, false).as_slice());
            last_style = Some(cell.style.clone());
        }
        let mut utf8 = [0_u8; 4];
        frame.extend_from_slice(cell.glyph.encode_utf8(&mut utf8).as_bytes());
    }

    frame.extend_from_slice(b"\x1b[0m\x1b[u");
    frame
}

pub(super) type BorderStyle = Style;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PaneBorderLineStyle {
    Single,
    Double,
    Heavy,
    Simple,
    Spaces,
}

impl PaneBorderLineStyle {
    pub(super) fn from_option(value: Option<&str>) -> Self {
        match value {
            Some("double") => Self::Double,
            Some("heavy") => Self::Heavy,
            Some("simple") => Self::Simple,
            Some("spaces") => Self::Spaces,
            _ => Self::Single,
        }
    }

    pub(super) fn map_glyph(self, glyph: char) -> char {
        match self {
            Self::Single => glyph,
            Self::Double => map_border_glyph(glyph, DOUBLE_BORDER_GLYPHS),
            Self::Heavy => map_border_glyph(glyph, HEAVY_BORDER_GLYPHS),
            Self::Simple => map_border_glyph(glyph, SIMPLE_BORDER_GLYPHS),
            Self::Spaces => ' ',
        }
    }
}

fn map_border_glyph(glyph: char, glyphs: &[(char, char)]) -> char {
    glyphs
        .iter()
        .find_map(|(from, to)| (*from == glyph).then_some(*to))
        .unwrap_or(glyph)
}

const DOUBLE_BORDER_GLYPHS: &[(char, char)] = &[
    ('│', '║'),
    ('─', '═'),
    ('┼', '╬'),
    ('┬', '╦'),
    ('┴', '╩'),
    ('┤', '╣'),
    ('├', '╠'),
    ('┘', '╝'),
    ('┐', '╗'),
    ('└', '╚'),
    ('┌', '╔'),
];

const HEAVY_BORDER_GLYPHS: &[(char, char)] = &[
    ('│', '┃'),
    ('─', '━'),
    ('┼', '╋'),
    ('┬', '┳'),
    ('┴', '┻'),
    ('┤', '┫'),
    ('├', '┣'),
    ('┘', '┛'),
    ('┐', '┓'),
    ('└', '┗'),
    ('┌', '┏'),
];

const SIMPLE_BORDER_GLYPHS: &[(char, char)] = &[
    ('│', '|'),
    ('─', '-'),
    ('┼', '+'),
    ('┬', '+'),
    ('┴', '+'),
    ('┤', '+'),
    ('├', '+'),
    ('┘', '+'),
    ('┐', '+'),
    ('└', '+'),
    ('┌', '+'),
];

pub(super) fn resolve_border_style_for_pane(
    session: &Session,
    options: &OptionStore,
    window_index: u32,
    pane: &Pane,
    active: bool,
) -> Style {
    let context = FormatContext::from_session(session)
        .with_window(window_index, session.window(), true, false)
        .with_window_pane(session.window(), pane);
    let runtime = RuntimeFormatContext::new(context)
        .with_options(options)
        .with_session(session)
        .with_window(window_index, session.window())
        .with_pane(pane);
    let option = if active {
        OptionName::PaneActiveBorderStyle
    } else {
        OptionName::PaneBorderStyle
    };
    apply_runtime_style_overlay(
        &Style::default(),
        options.resolve_for_pane(session.name(), window_index, pane.index(), option),
        &runtime,
    )
}
pub(super) struct BorderCell {
    pub(super) x: u16,
    pub(super) y: u16,
    pub(super) glyph: char,
    pub(super) style: BorderStyle,
}
