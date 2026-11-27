//! Windows console key records for control keys sent to a target pane.

use rmux_core::{key_code_lookup_bits, key_string_lookup_string};
use rmux_proto::{PaneTarget, RmuxError};
use rmux_pty::WindowsConsoleKeyEvent;

use super::key_encoding::encode_key_for_target;
use super::windows_target_shell::{
    target_routes_windows_ctrl_d_as_posix_eot, target_uses_windows_cmd_console_ctrl_d,
    target_uses_windows_cmd_shell, target_uses_wsl_host_process,
};
use super::WindowsConsoleInputAction;
use crate::keys::parse_key_code;
use crate::pane_terminals::HandlerState;

pub(in crate::handler::pane_support) fn windows_console_input_for_attached_key(
    state: &HandlerState,
    target: &PaneTarget,
    decoded_key: rmux_core::KeyCode,
    console_key: WindowsConsoleKeyEvent,
) -> WindowsConsoleInputAction {
    let console_key = if key_matches_name(decoded_key, "C-d")
        && !target_uses_windows_cmd_console_ctrl_d(state, target)
    {
        windows_ctrl_d_console_key(false).with_repeat_count(console_key.repeat_count())
    } else {
        console_key
    };
    windows_console_input_for_attached_key_event(console_key)
}

fn windows_console_input_for_attached_key_event(
    console_key: WindowsConsoleKeyEvent,
) -> WindowsConsoleInputAction {
    if console_key.virtual_key_code() == b'C' as u16 && console_key.unicode_char() == 0x03 {
        WindowsConsoleInputAction::KeyThenInterrupt(console_key)
    } else {
        WindowsConsoleInputAction::Key(console_key)
    }
}

pub(in crate::handler::pane_support) fn windows_console_input_for_tokens(
    tokens: &[String],
    repeat_count: usize,
) -> Option<(WindowsConsoleInputAction, Vec<u8>)> {
    windows_console_input_for_tokens_with_ctrl_d(
        tokens,
        repeat_count,
        WindowsConsoleKeyEvent::ctrl_d(),
    )
}

pub(in crate::handler::pane_support) fn windows_console_input_for_target_tokens(
    state: &HandlerState,
    target: &PaneTarget,
    tokens: &[String],
    repeat_count: usize,
) -> Option<(WindowsConsoleInputAction, Vec<u8>)> {
    let target_uses_cmd =
        tokens_are_windows_ctrl_d(tokens) && target_uses_windows_cmd_console_ctrl_d(state, target);
    let ctrl_d = windows_ctrl_d_console_key(target_uses_cmd);
    windows_console_input_for_tokens_with_ctrl_d(tokens, repeat_count, ctrl_d)
}

/// `cmd.exe` needs the physical key record to interrupt commands such as
/// `timeout.exe`. Every other target receives a typed, scan-code-free EOT: the
/// writer suppresses it in processed mode but preserves it for raw/TUI input.
const fn windows_ctrl_d_console_key(target_uses_cmd: bool) -> WindowsConsoleKeyEvent {
    if target_uses_cmd {
        WindowsConsoleKeyEvent::ctrl_d()
    } else {
        WindowsConsoleKeyEvent::ctrl_d_eot()
    }
}

fn windows_console_input_for_tokens_with_ctrl_d(
    tokens: &[String],
    repeat_count: usize,
    ctrl_d: WindowsConsoleKeyEvent,
) -> Option<(WindowsConsoleInputAction, Vec<u8>)> {
    let [token] = tokens else {
        return None;
    };
    windows_console_input_for_token_with_ctrl_d(token, repeat_count, ctrl_d)
}

pub(in crate::handler::pane_support) fn windows_console_input_for_token(
    token: &str,
    repeat_count: usize,
) -> Option<(WindowsConsoleInputAction, Vec<u8>)> {
    windows_console_input_for_token_with_ctrl_d(
        token,
        repeat_count,
        WindowsConsoleKeyEvent::ctrl_d(),
    )
}

fn windows_console_input_for_token_with_ctrl_d(
    token: &str,
    repeat_count: usize,
    ctrl_d: WindowsConsoleKeyEvent,
) -> Option<(WindowsConsoleInputAction, Vec<u8>)> {
    let key = key_code_lookup_bits(parse_key_code(token)?);
    let repeat_count = repeat_count.min(usize::from(u16::MAX)).max(1);
    let repeat_count_u16 = repeat_count as u16;
    let (key_event, byte) = windows_console_ctrl_letter_for_key(key, ctrl_d)?;
    let key_event = key_event.with_repeat_count(repeat_count_u16);
    let action = if key_matches_name(key, "C-c") {
        WindowsConsoleInputAction::KeyThenInterrupt(key_event)
    } else {
        WindowsConsoleInputAction::Key(key_event)
    };
    Some((action, vec![byte; repeat_count]))
}

fn tokens_are_windows_ctrl_d(tokens: &[String]) -> bool {
    let [token] = tokens else {
        return false;
    };
    parse_key_code(token).is_some_and(|key| key_matches_name(key_code_lookup_bits(key), "C-d"))
}

pub(in crate::handler::pane_support) fn tokens_contain_windows_console_interrupt(
    tokens: &[String],
) -> bool {
    tokens.iter().any(|token| {
        windows_console_input_for_token(token, 1).is_some_and(|(action, _)| {
            matches!(action, WindowsConsoleInputAction::KeyThenInterrupt(_))
        })
    })
}

fn windows_console_ctrl_letter_for_key(
    key: rmux_core::KeyCode,
    ctrl_d: WindowsConsoleKeyEvent,
) -> Option<(WindowsConsoleKeyEvent, u8)> {
    for letter in b'A'..=b'Z' {
        let name = format!("C-{}", char::from(letter.to_ascii_lowercase()));
        if Some(key) == key_string_lookup_string(&name).map(key_code_lookup_bits) {
            let event = match letter {
                b'C' => WindowsConsoleKeyEvent::ctrl_c(),
                b'D' => ctrl_d,
                b'Z' => WindowsConsoleKeyEvent::ctrl_z(),
                _ => WindowsConsoleKeyEvent::ctrl_letter(letter)?,
            };
            return Some((event, letter - b'A' + 1));
        }
    }
    None
}

pub(in crate::handler::pane_support) fn should_emulate_windows_cmd_select_all(
    state: &HandlerState,
    target: &PaneTarget,
    key: rmux_core::KeyCode,
) -> bool {
    key_matches_name(key, "C-a") && target_uses_windows_cmd_shell(state, target)
}

pub(in crate::handler::pane_support) fn tokens_emulate_windows_cmd_select_all(
    state: &HandlerState,
    target: &PaneTarget,
    tokens: &[String],
) -> bool {
    let [token] = tokens else {
        return false;
    };
    parse_key_code(token)
        .is_some_and(|key| should_emulate_windows_cmd_select_all(state, target, key))
}

pub(in crate::handler::pane_support) fn should_route_windows_control_as_pty_bytes(
    state: &HandlerState,
    target: &PaneTarget,
    key: rmux_core::KeyCode,
) -> bool {
    if key_matches_name(key, "C-d") {
        return target_routes_windows_ctrl_d_as_posix_eot(state, target);
    }
    key_matches_name(key, "C-c") && target_uses_wsl_host_process(state, target)
}

pub(in crate::handler::pane_support) fn tokens_route_windows_control_as_pty_bytes(
    state: &HandlerState,
    target: &PaneTarget,
    tokens: &[String],
) -> bool {
    let [token] = tokens else {
        return false;
    };
    parse_key_code(token)
        .is_some_and(|key| should_route_windows_control_as_pty_bytes(state, target, key))
}

pub(super) fn windows_cmd_select_all_sequence(
    state: &HandlerState,
    target: &PaneTarget,
) -> Result<Option<Vec<u8>>, RmuxError> {
    let mut bytes = Vec::new();
    for key_name in ["C-Home", "S-End"] {
        let Some(key) = key_string_lookup_string(key_name) else {
            return Ok(None);
        };
        let Some(encoded) = encode_key_for_target(state, target, key)? else {
            return Ok(None);
        };
        bytes.extend_from_slice(&encoded);
    }
    Ok(Some(bytes))
}

fn key_matches_name(key: rmux_core::KeyCode, name: &str) -> bool {
    key_string_lookup_string(name)
        .is_some_and(|candidate| key_code_lookup_bits(candidate) == key_code_lookup_bits(key))
}

#[cfg(test)]
mod windows_tests {
    use rmux_pty::WindowsConsoleKeyEvent;

    #[test]
    fn windows_console_key_mapping_covers_control_signals() {
        assert_eq!(
            super::windows_console_input_for_tokens(&["C-a".to_owned()], 1),
            Some((
                super::WindowsConsoleInputAction::Key(
                    WindowsConsoleKeyEvent::ctrl_letter(b'A').unwrap()
                ),
                vec![0x01]
            ))
        );
        assert_eq!(
            super::windows_console_input_for_tokens(&["C-c".to_owned()], 1),
            Some((
                super::WindowsConsoleInputAction::KeyThenInterrupt(WindowsConsoleKeyEvent::ctrl_c()),
                vec![0x03]
            ))
        );
        assert_eq!(
            super::windows_console_input_for_attached_key_event(WindowsConsoleKeyEvent::ctrl_c()),
            super::WindowsConsoleInputAction::KeyThenInterrupt(WindowsConsoleKeyEvent::ctrl_c())
        );
        assert_eq!(
            super::windows_console_input_for_tokens(&["C-d".to_owned()], 1),
            Some((
                super::WindowsConsoleInputAction::Key(WindowsConsoleKeyEvent::ctrl_d()),
                vec![0x04]
            ))
        );
        assert_eq!(
            super::windows_console_input_for_tokens(&["C-z".to_owned()], 2),
            Some((
                super::WindowsConsoleInputAction::Key(
                    WindowsConsoleKeyEvent::ctrl_z().with_repeat_count(2)
                ),
                vec![0x1a, 0x1a]
            ))
        );
        assert!(super::tokens_contain_windows_console_interrupt(&[
            "Enter".to_owned(),
            "C-c".to_owned()
        ]));
        assert!(super::tokens_contain_windows_console_interrupt(&[
            "C-c".to_owned(),
            "Enter".to_owned()
        ]));
        assert!(!super::tokens_contain_windows_console_interrupt(&[
            "C-d".to_owned(),
            "Enter".to_owned()
        ]));
    }

    #[test]
    fn windows_ctrl_d_uses_a_physical_key_only_for_cmd() {
        assert_eq!(
            super::windows_ctrl_d_console_key(false),
            WindowsConsoleKeyEvent::ctrl_d_eot()
        );
        assert_eq!(
            super::windows_ctrl_d_console_key(true),
            WindowsConsoleKeyEvent::ctrl_d()
        );
    }
}
