use rmux_proto::{OptionName, WindowTarget};

use crate::pane_terminals::HandlerState;

use super::parse::ParsedDisplayPopupCommand;

/// The command a popup job's shell runs: the given shell-command, else the target session's
/// default-command. As tmux 3.7b, either is handed to the shell as written, formats unexpanded.
pub(super) fn popup_shell_command(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    command: &ParsedDisplayPopupCommand,
) -> Option<String> {
    if command.no_job {
        return None;
    }
    if let Some(command_text) = &command.command {
        return Some(command_text.clone());
    }
    let default = state
        .options
        .resolve(Some(session_name), OptionName::DefaultCommand)
        .unwrap_or_default();
    (!default.is_empty()).then(|| default.to_owned())
}

pub(super) fn find_window_target_by_id(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    window_id: u32,
) -> Option<WindowTarget> {
    let session = state.sessions.session(session_name)?;
    session
        .windows()
        .find_map(|(window_index, window)| {
            (window.id().as_u32() == window_id).then_some(window_index)
        })
        .map(|window_index| WindowTarget::with_window(session_name.clone(), window_index))
}

pub(super) fn find_session_name_by_id(
    state: &HandlerState,
    session_id: u32,
) -> Option<rmux_proto::SessionName> {
    state
        .sessions
        .iter()
        .map(|session| (session.name(), session))
        .find_map(|(session_name, session)| {
            (session.id().as_u32() == session_id).then_some(session_name.clone())
        })
}

#[cfg(all(test, unix))]
#[path = "../../handler_overlay/support_tests.rs"]
mod tests;
