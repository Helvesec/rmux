use std::num::IntErrorKind;

use rmux_core::{BoxLines, PaneGeometry, Style};
use rmux_proto::{OptionName, RmuxError, Target, TerminalSize};

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::mouse::{AttachedMouseEvent, StatusLineLayout, StatusRangeType};
use crate::pane_terminals::HandlerState;
use crate::renderer::{
    status_line_layout, OverlayMousePosition, OverlayPositionContext, OverlayRect,
};
use crate::status_lines::status_line_count;

use super::parse::{ParsedDisplayMenuCommand, ParsedDisplayPopupCommand};
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

/// Resolves a display-popup `-w`/`-h` value as tmux 3.7b does: half the
/// client by default, otherwise an absolute size or a percentage of the
/// client that must fall within 1..=total. Anything else fails the command
/// with "<name> <cause>" instead of being clamped.
pub(super) fn resolve_popup_size(
    name: &str,
    value: Option<&str>,
    total: u16,
) -> Result<u16, RmuxError> {
    let Some(value) = value else {
        return Ok(total / 2);
    };
    if value.is_empty() {
        return Err(popup_size_error(name, "empty"));
    }
    let limit = i64::from(total);
    let size = match value.strip_suffix('%') {
        Some(percent) => {
            let percent =
                bounded_number(percent, 0, 100).map_err(|cause| popup_size_error(name, cause))?;
            limit * percent / 100
        }
        None => bounded_number(value, 1, limit).map_err(|cause| popup_size_error(name, cause))?,
    };
    if size < 1 {
        return Err(popup_size_error(name, "too small"));
    }
    if size > limit {
        return Err(popup_size_error(name, "too large"));
    }
    u16::try_from(size).map_err(|_| popup_size_error(name, "too large"))
}

/// tmux's `strtonum`: a base-10 integer within `min..=max`.
fn bounded_number(text: &str, min: i64, max: i64) -> Result<i64, &'static str> {
    let value = text.parse::<i64>().map_err(|error| match error.kind() {
        IntErrorKind::PosOverflow => "too large",
        IntErrorKind::NegOverflow => "too small",
        _ => "invalid",
    })?;
    if value < min {
        Err("too small")
    } else if value > max {
        Err("too large")
    } else {
        Ok(value)
    }
}

fn popup_size_error(name: &str, cause: &str) -> RmuxError {
    RmuxError::Server(format!("{name} {cause}"))
}

/// tmux opens no popup, without an error, when it cannot hold one content
/// cell inside its border.
pub(super) fn popup_holds_content(width: u16, height: u16, border_lines: BoxLines) -> bool {
    let border = if border_lines.visible() { 2 } else { 0 };
    width > border && height > border
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
    let status_layout = status_line_layout(session, &state.options, 0, None, Some(state));
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
    session: rmux_core::ManagedSessionView<'_>,
    options: &rmux_core::OptionStore,
) -> Option<u16> {
    let status_lines = overlay_status_lines(session, options);
    if status_lines == 0 {
        return None;
    }

    match options.resolve(Some(session.name()), OptionName::StatusPosition) {
        Some("top") => Some(0),
        _ => Some(session.terminal_size().rows.saturating_sub(status_lines)),
    }
}

fn overlay_status_lines(
    session: rmux_core::ManagedSessionView<'_>,
    options: &rmux_core::OptionStore,
) -> u16 {
    status_line_count(
        options.resolve(Some(session.name()), OptionName::Status),
        session.terminal_size().rows,
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

fn pane_geometry_for_target(
    session: rmux_core::ManagedSessionView<'_>,
    target: &Target,
) -> Option<PaneGeometry> {
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

    fn session() -> crate::session_fixture::SessionFixture {
        crate::session_fixture::SessionFixture::new(
            session_name("overlay"),
            TerminalSize { cols: 80, rows: 10 },
        )
    }

    fn set_session_option(
        options: &mut rmux_core::OptionStore,
        session: rmux_core::ManagedSessionView<'_>,
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

        assert_eq!(overlay_status_lines(session.view(), &options), 1);
        assert_eq!(overlay_status_at(session.view(), &options), Some(9));

        set_session_option(&mut options, session.view(), "status", "3");
        assert_eq!(overlay_status_lines(session.view(), &options), 3);
        assert_eq!(overlay_status_at(session.view(), &options), Some(7));

        set_session_option(&mut options, session.view(), "status-position", "top");
        assert_eq!(overlay_status_lines(session.view(), &options), 3);
        assert_eq!(overlay_status_at(session.view(), &options), Some(0));

        set_session_option(&mut options, session.view(), "status", "off");
        assert_eq!(overlay_status_lines(session.view(), &options), 0);
        assert_eq!(overlay_status_at(session.view(), &options), None);
    }

    /// Measured on tmux 3.7b with an attached 100x30 client: sizes outside
    /// 1..=client are refused with "<name> <cause>", never clamped.
    #[test]
    fn popup_sizes_are_refused_outside_the_client_like_tmux() {
        let size = |name: &str, value: Option<&str>, total: u16| {
            resolve_popup_size(name, value, total).map_err(|error| match error {
                RmuxError::Server(message) => message,
                other => format!("unexpected error {other:?}"),
            })
        };
        let refused = |cause: &str| Err(cause.to_owned());

        assert_eq!(size("width", None, 100), Ok(50));
        assert_eq!(size("width", Some("100"), 100), Ok(100));
        assert_eq!(size("width", Some("99"), 100), Ok(99));
        assert_eq!(size("width", Some("1"), 100), Ok(1));
        assert_eq!(size("width", Some("100%"), 100), Ok(100));
        assert_eq!(size("width", Some("200"), 100), refused("width too large"));
        assert_eq!(size("width", Some("101"), 100), refused("width too large"));
        assert_eq!(size("width", Some("101%"), 100), refused("width too large"));
        assert_eq!(size("width", Some("0"), 100), refused("width too small"));
        assert_eq!(size("width", Some("0%"), 100), refused("width too small"));
        assert_eq!(size("width", Some("abc"), 100), refused("width invalid"));
        assert_eq!(size("height", None, 30), Ok(15));
        assert_eq!(size("height", Some("30"), 30), Ok(30));
        assert_eq!(size("height", Some("29"), 30), Ok(29));
        assert_eq!(size("height", Some("31"), 30), refused("height too large"));
    }

    #[test]
    fn a_bordered_popup_needs_one_content_cell_like_tmux() {
        let single = BoxLines::parse(Some("single"));
        assert!(single.visible());
        assert!(!popup_holds_content(2, 10, single));
        assert!(popup_holds_content(3, 10, single));
        assert!(popup_holds_content(1, 10, BoxLines::None));
    }
}
