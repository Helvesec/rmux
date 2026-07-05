use rmux_core::{BoxLines, PaneGeometry, Style};
use rmux_proto::{OptionName, Target, TerminalSize};

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::mouse::{AttachedMouseEvent, StatusLineLayout, StatusRangeType};
use crate::pane_terminals::HandlerState;
use crate::renderer::{
    status_line_layout, OverlayMousePosition, OverlayPositionContext, OverlayRect,
};
use crate::status_lines::status_line_count;

use super::parse::{ParsedDisplayMenuCommand, ParsedDisplayPopupCommand, PopupSizeSpec};
use super::MenuOverlayItem;

#[derive(Debug)]
pub(super) struct ResolvedMenuStyles {
    pub(super) style: Style,
    pub(super) selected_style: Style,
    pub(super) border_style: Style,
    pub(super) border_lines: BoxLines,
}

#[derive(Debug)]
pub(super) struct ResolvedPopupStyles {
    pub(super) style: Style,
    pub(super) border_style: Style,
    pub(super) border_lines: BoxLines,
}

pub(super) fn menu_styles_for_target(
    state: &HandlerState,
    target: &Target,
    command: &ParsedDisplayMenuCommand,
    runtime: &RuntimeFormatContext<'_>,
) -> ResolvedMenuStyles {
    let (session_name, window_index) = window_scope_for_target(state, target);
    let base = menu_option_styles(state, session_name, window_index, command.border_lines);
    ResolvedMenuStyles {
        style: parse_style_with_runtime(
            command.style.as_deref().or_else(|| {
                state
                    .options
                    .resolve_for_window(session_name, window_index, OptionName::MenuStyle)
            }),
            runtime,
        ),
        selected_style: parse_style_with_runtime(
            command.selected_style.as_deref().or_else(|| {
                state.options.resolve_for_window(
                    session_name,
                    window_index,
                    OptionName::MenuSelectedStyle,
                )
            }),
            runtime,
        ),
        border_style: parse_style_with_runtime(
            command.border_style.as_deref().or_else(|| {
                state.options.resolve_for_window(
                    session_name,
                    window_index,
                    OptionName::MenuBorderStyle,
                )
            }),
            runtime,
        ),
        border_lines: command.border_lines.unwrap_or(base.border_lines),
    }
}

pub(super) fn popup_styles_for_target(
    state: &HandlerState,
    target: &Target,
    command: &ParsedDisplayPopupCommand,
    runtime: &RuntimeFormatContext<'_>,
) -> ResolvedPopupStyles {
    let (session_name, window_index) = window_scope_for_target(state, target);
    ResolvedPopupStyles {
        style: parse_style_with_runtime(
            command.style.as_deref().or_else(|| {
                state
                    .options
                    .resolve_for_window(session_name, window_index, OptionName::PopupStyle)
            }),
            runtime,
        ),
        border_style: parse_style_with_runtime(
            command.border_style.as_deref().or_else(|| {
                state.options.resolve_for_window(
                    session_name,
                    window_index,
                    OptionName::PopupBorderStyle,
                )
            }),
            runtime,
        ),
        border_lines: command.border_lines.unwrap_or_else(|| {
            BoxLines::parse(state.options.resolve_for_window(
                session_name,
                window_index,
                OptionName::PopupBorderLines,
            ))
        }),
    }
}

pub(super) fn menu_option_styles(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    window_index: u32,
    override_lines: Option<BoxLines>,
) -> ResolvedMenuStyles {
    ResolvedMenuStyles {
        style: parse_style_or_default(state.options.resolve_for_window(
            session_name,
            window_index,
            OptionName::MenuStyle,
        )),
        selected_style: parse_style_or_default(state.options.resolve_for_window(
            session_name,
            window_index,
            OptionName::MenuSelectedStyle,
        )),
        border_style: parse_style_or_default(state.options.resolve_for_window(
            session_name,
            window_index,
            OptionName::MenuBorderStyle,
        )),
        border_lines: override_lines.unwrap_or_else(|| {
            BoxLines::parse(state.options.resolve_for_window(
                session_name,
                window_index,
                OptionName::MenuBorderLines,
            ))
        }),
    }
}

pub(super) fn resolve_popup_size(spec: Option<PopupSizeSpec>, default: u16, total: u16) -> u16 {
    match spec {
        Some(PopupSizeSpec::Absolute(value)) => value.clamp(1, total.max(1)),
        Some(PopupSizeSpec::Percent(percent)) => {
            let value = ((u32::from(total.max(1)) * u32::from(percent)) / 100)
                .clamp(1, u32::from(total.max(1)));
            u16::try_from(value).unwrap_or(total.max(1))
        }
        None => default.clamp(1, total.max(1)),
    }
}

pub(super) fn popup_content_size(rect: OverlayRect, border_lines: BoxLines) -> TerminalSize {
    if border_lines.visible() {
        TerminalSize {
            cols: rect.width.saturating_sub(2),
            rows: rect.height.saturating_sub(2),
        }
    } else {
        TerminalSize {
            cols: rect.width,
            rows: rect.height,
        }
    }
}

pub(super) fn overlay_position_context(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    target: &Target,
    client_size: TerminalSize,
    mouse: Option<&AttachedMouseEvent>,
) -> OverlayPositionContext {
    let session = state
        .sessions
        .session(session_name)
        .expect("overlay session must exist");
    let window_index = target_window_index(target).unwrap_or_else(|| session.active_window_index());
    let window = session
        .window_at(window_index)
        .unwrap_or_else(|| session.window());
    let pane = pane_geometry_for_target(session, target)
        .or_else(|| window.active_pane().map(|pane| pane.geometry()));
    let status_layout = status_line_layout(session, &state.options, 0, None);
    OverlayPositionContext {
        client_size,
        pane,
        mouse: mouse.map(|event| OverlayMousePosition {
            x: event.raw.x,
            y: event.raw.y,
        }),
        status_at: overlay_status_at(session, &state.options),
        status_lines: overlay_status_lines(session, &state.options),
        window_status_x: mouse
            .and_then(|event| window_status_range_start(status_layout.as_ref(), event)),
    }
}

pub(super) fn target_window_index(target: &Target) -> Option<u32> {
    match target {
        Target::Window(target) => Some(target.window_index()),
        Target::Pane(target) => Some(target.window_index()),
        Target::Session(_) => None,
    }
}

pub(super) fn menu_width(title: &str, items: &[MenuOverlayItem]) -> u16 {
    let utf8 = rmux_core::Utf8Config::default();
    let title_width = rmux_core::text_width(title, &utf8);
    let item_width = items
        .iter()
        .filter(|item| !item.separator)
        .map(|item| {
            rmux_core::text_width(&item.label, &utf8)
                + item
                    .shortcut_label
                    .as_ref()
                    .map(|shortcut| rmux_core::text_width(shortcut, &utf8) + 1)
                    .unwrap_or_default()
        })
        .max()
        .unwrap_or_default();
    u16::try_from(title_width.max(item_width)).unwrap_or(u16::MAX)
}

fn parse_style_or_default(value: Option<&str>) -> Style {
    value
        .filter(|value| !value.is_empty())
        .and_then(|value| Style::parse(value).ok())
        .unwrap_or_default()
}

fn parse_style_with_runtime(value: Option<&str>, runtime: &RuntimeFormatContext<'_>) -> Style {
    value
        .filter(|value| !value.is_empty())
        .map(|value| render_runtime_template(value, runtime, true))
        .and_then(|value| Style::parse(&value).ok())
        .unwrap_or_default()
}

fn overlay_status_at(
    session: &rmux_core::Session,
    options: &rmux_core::OptionStore,
) -> Option<u16> {
    let status_lines = overlay_status_lines(session, options);
    if status_lines == 0 {
        return None;
    }

    match options.resolve(Some(session.name()), OptionName::StatusPosition) {
        Some("top") => Some(0),
        _ => Some(session.window().size().rows.saturating_sub(status_lines)),
    }
}

fn overlay_status_lines(session: &rmux_core::Session, options: &rmux_core::OptionStore) -> u16 {
    status_line_count(
        options.resolve(Some(session.name()), OptionName::Status),
        session.window().size().rows,
    )
}

fn window_status_range_start(
    layout: Option<&StatusLineLayout>,
    event: &AttachedMouseEvent,
) -> Option<u16> {
    let window_id = event.window_id?;
    layout?
        .ranges
        .iter()
        .find(|range| matches!(range.kind, StatusRangeType::Window(id) if id == window_id))
        .map(|range| *range.x.start())
}

fn pane_geometry_for_target(session: &rmux_core::Session, target: &Target) -> Option<PaneGeometry> {
    match target {
        Target::Pane(target) => session
            .window_at(target.window_index())
            .and_then(|window| window.pane(target.pane_index()))
            .map(rmux_core::Pane::geometry),
        Target::Window(target) => session
            .window_at(target.window_index())
            .and_then(rmux_core::Window::active_pane)
            .map(rmux_core::Pane::geometry),
        Target::Session(_) => session
            .window()
            .active_pane()
            .map(rmux_core::Pane::geometry),
    }
}

fn window_scope_for_target<'a>(
    state: &'a HandlerState,
    target: &'a Target,
) -> (&'a rmux_proto::SessionName, u32) {
    match target {
        Target::Window(target) => (target.session_name(), target.window_index()),
        Target::Pane(target) => (target.session_name(), target.window_index()),
        Target::Session(session_name) => {
            let session = state
                .sessions
                .session(session_name)
                .expect("overlay session must exist");
            (session_name, session.active_window_index())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_name(value: &str) -> rmux_proto::SessionName {
        rmux_proto::SessionName::new(value).expect("valid session name")
    }

    fn session() -> rmux_core::Session {
        rmux_core::Session::new(session_name("overlay"), TerminalSize { cols: 80, rows: 10 })
    }

    fn set_session_option(
        options: &mut rmux_core::OptionStore,
        session: &rmux_core::Session,
        name: &str,
        value: &str,
    ) {
        options
            .set_by_name(
                rmux_proto::types::OptionScopeSelector::Session(session.name().clone()),
                name,
                Some(value.to_owned()),
                rmux_proto::SetOptionMode::Replace,
                false,
                false,
                false,
            )
            .expect("session option set succeeds");
    }

    #[test]
    fn overlay_status_geometry_respects_multi_line_status() {
        let session = session();
        let mut options = rmux_core::OptionStore::new();

        assert_eq!(overlay_status_lines(&session, &options), 1);
        assert_eq!(overlay_status_at(&session, &options), Some(9));

        set_session_option(&mut options, &session, "status", "3");
        assert_eq!(overlay_status_lines(&session, &options), 3);
        assert_eq!(overlay_status_at(&session, &options), Some(7));

        set_session_option(&mut options, &session, "status-position", "top");
        assert_eq!(overlay_status_lines(&session, &options), 3);
        assert_eq!(overlay_status_at(&session, &options), Some(0));

        set_session_option(&mut options, &session, "status", "off");
        assert_eq!(overlay_status_lines(&session, &options), 0);
        assert_eq!(overlay_status_at(&session, &options), None);
    }
}
