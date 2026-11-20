//! Key, token and mouse encoding for the input mode of a target pane.

use rmux_core::{
    key_code_lookup_bits, key_code_to_bytes, key_string_lookup_key, key_string_lookup_string,
};
use rmux_proto::{OptionName, PaneTarget, RmuxError};

#[cfg(windows)]
use super::windows_console_keys::{
    should_emulate_windows_cmd_select_all, windows_cmd_select_all_sequence,
};
use crate::input_keys::{encode_key_with_backspace, encode_mouse_event, ExtendedKeyFormat};
use crate::keys::parse_key_code;
use crate::pane_terminals::{session_not_found, HandlerState};

pub(in crate::handler::pane_support) fn encode_tokens_for_target(
    state: &HandlerState,
    target: &PaneTarget,
    tokens: &[String],
) -> Result<Vec<u8>, RmuxError> {
    let mut bytes = Vec::new();
    for token in tokens {
        if let Some(key) = parse_key_code(token) {
            let Some(encoded) = encode_key_for_target(state, target, key)? else {
                return Err(RmuxError::Server(format!(
                    "key {} cannot be sent to a pane",
                    key_string_lookup_key(key_code_lookup_bits(key), false)
                )));
            };
            bytes.extend_from_slice(&encoded);
        } else {
            bytes.extend_from_slice(token.as_bytes());
        }
    }
    Ok(bytes)
}

pub(in crate::handler::pane_support) fn encode_key_for_target(
    state: &HandlerState,
    target: &PaneTarget,
    key: rmux_core::KeyCode,
) -> Result<Option<Vec<u8>>, RmuxError> {
    #[cfg(windows)]
    if should_emulate_windows_cmd_select_all(state, target, key) {
        return windows_cmd_select_all_sequence(state, target);
    }

    let pane_mode = pane_input_mode(state, target)?;
    let format =
        ExtendedKeyFormat::parse(state.options.resolve(None, OptionName::ExtendedKeysFormat));
    let backspace = state
        .options
        .resolve(None, OptionName::Backspace)
        .and_then(key_string_lookup_string)
        .and_then(key_code_to_bytes)
        .and_then(|bytes| (bytes.len() == 1).then_some(bytes[0]))
        .unwrap_or(0x7f);
    Ok(encode_key_with_backspace(pane_mode, format, key, backspace))
}

pub(in crate::handler::pane_support) fn pane_input_mode(
    state: &HandlerState,
    target: &PaneTarget,
) -> Result<u32, RmuxError> {
    let pane_id = state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .map(|pane| pane.id())
        .ok_or_else(|| {
            RmuxError::invalid_target(target.to_string(), "pane index does not exist in session")
        })?;
    let pane_mode = state
        .pane_screen_state(target.session_name(), pane_id)
        .map(|screen_state| screen_state.mode)
        .unwrap_or_default();
    Ok(pane_mode)
}

pub(in crate::handler::pane_support) fn encode_mouse_for_target(
    state: &HandlerState,
    target: &PaneTarget,
    event: &crate::mouse::AttachedMouseEvent,
) -> Result<Vec<u8>, RmuxError> {
    let session = state
        .sessions
        .session(target.session_name())
        .ok_or_else(|| session_not_found(target.session_name()))?;
    let window = session.window_at(target.window_index()).ok_or_else(|| {
        RmuxError::invalid_target(target.to_string(), "window index does not exist in session")
    })?;
    let pane = window.pane(target.pane_index()).ok_or_else(|| {
        RmuxError::invalid_target(target.to_string(), "pane index does not exist in session")
    })?;
    if event.ignore || event.pane_id != Some(pane.id()) {
        return Ok(Vec::new());
    }

    let pane_mode = state
        .pane_screen_state(target.session_name(), pane.id())
        .map(|screen_state| screen_state.mode)
        .unwrap_or_default();
    let adjusted_y = match event.status_at {
        Some(0) if event.raw.y >= event.status_lines => event.raw.y - event.status_lines,
        _ => event.raw.y,
    };
    let Some(geometry) = event
        .coordinates
        .or_else(|| crate::mouse::pane_mouse_coordinates_for_target(state, target))
    else {
        return Ok(Vec::new());
    };
    let Some((x, y)) = relative_mouse_position(event.raw.x, adjusted_y, geometry) else {
        return Ok(Vec::new());
    };
    Ok(encode_mouse_event(pane_mode, &event.raw, x, y).unwrap_or_default())
}

fn relative_mouse_position(
    x: u16,
    y: u16,
    geometry: impl Into<crate::mouse::MouseCoordinates>,
) -> Option<(u16, u16)> {
    geometry.into().relative_position(x, y)
}

#[cfg(test)]
mod mouse_geometry_tests {
    use rmux_core::PaneGeometry;
    use rmux_proto::{OptionName, ScopeSelector, SetOptionMode, TerminalSize, WindowTarget};

    use super::*;
    use crate::session_fixture::SessionSetupExt as _;
    use rmux_proto::SessionName;

    #[test]
    fn signed_scene_application_mouse_coordinates_use_source_origin() {
        let mut state = HandlerState::default();
        let name = SessionName::new("signed-application-mouse").unwrap();
        state
            .sessions
            .create_session(name.clone(), TerminalSize { cols: 20, rows: 10 })
            .unwrap();
        state
            .options
            .set(
                ScopeSelector::Session(name.clone()),
                OptionName::Status,
                "off".into(),
                SetOptionMode::Replace,
            )
            .unwrap();
        let window = WindowTarget::with_window(name.clone(), 0);
        let pane = state
            .sessions
            .split_pane(
                &window,
                0,
                rmux_core::ManagedPaneSplit {
                    direction: rmux_proto::SplitDirection::Vertical,
                    before: false,
                    detached: false,
                    full_size: false,
                },
            )
            .unwrap();
        state
            .sessions
            .window_layout_context_mut(&window)
            .unwrap()
            .float_pane(
                pane.pane_index,
                rmux_core::FloatingPaneRect::new(-3, 2, 8, 4).unwrap(),
            )
            .unwrap();
        let target = PaneTarget::with_window(name, 0, pane.pane_index);
        let coordinates = crate::mouse::pane_mouse_coordinates_for_target(&state, &target).unwrap();
        assert_eq!(relative_mouse_position(0, 2, coordinates), Some((3, 0)));
        assert_eq!(relative_mouse_position(4, 5, coordinates), Some((7, 3)));
        assert_eq!(relative_mouse_position(5, 2, coordinates), None);
        assert_eq!(relative_mouse_position(0, 1, coordinates), None);
    }

    #[test]
    fn left_scrollbar_application_mouse_coordinates_start_at_content_zero() {
        let mut state = HandlerState::default();
        let session_name = SessionName::new("mouse-left-scrollbar").expect("valid session");
        state
            .sessions
            .create_session(session_name.clone(), TerminalSize { cols: 20, rows: 8 })
            .expect("session creation");
        state
            .sessions
            .session_setup(&session_name)
            .expect("created session")
            .resize_active_window_geometry(
                TerminalSize { cols: 20, rows: 8 },
                TerminalSize { cols: 20, rows: 7 },
            );
        let window = WindowTarget::with_window(session_name.clone(), 0);
        for (option, value) in [
            (OptionName::PaneScrollbars, "on"),
            (OptionName::PaneScrollbarsPosition, "left"),
            (OptionName::PaneScrollbarsStyle, "width=2,pad=1"),
        ] {
            state
                .options
                .set(
                    ScopeSelector::Window(window.clone()),
                    option,
                    value.to_owned(),
                    SetOptionMode::Replace,
                )
                .expect("scrollbar option");
        }
        let target = PaneTarget::with_window(session_name, 0, 0);

        let geometry = crate::mouse::pane_content_geometry_for_target(&state, &target)
            .expect("pane content geometry");

        assert_eq!(geometry, PaneGeometry::new(3, 0, 17, 7));
        assert_eq!(relative_mouse_position(3, 0, geometry), Some((0, 0)));
        assert_eq!(relative_mouse_position(19, 0, geometry), Some((16, 0)));
        assert_eq!(relative_mouse_position(2, 0, geometry), None);
    }
}
