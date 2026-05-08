use rmux_proto::{OptionName, RmuxError, WindowTarget};

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::HandlerState;

use super::parse::ParsedDisplayPopupCommand;
use super::PromptInputEvent;

pub(super) fn popup_shell_command(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    command: &ParsedDisplayPopupCommand,
    runtime: &RuntimeFormatContext<'_>,
) -> Result<Option<String>, RmuxError> {
    if command.no_job {
        return Ok(None);
    }
    if let Some(command_text) = &command.command {
        return Ok(Some(render_runtime_template(command_text, runtime, false)));
    }
    let default = state
        .options
        .resolve(Some(session_name), OptionName::DefaultCommand)
        .unwrap_or_default();
    if default.is_empty() {
        Ok(None)
    } else {
        Ok(Some(render_runtime_template(default, runtime, false)))
    }
}

pub(super) fn find_window_target_by_id(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    window_id: u32,
) -> Option<WindowTarget> {
    let session = state.sessions.session(session_name)?;
    session
        .windows()
        .iter()
        .find_map(|(window_index, window)| {
            (window.id().as_u32() == window_id).then_some(*window_index)
        })
        .map(|window_index| WindowTarget::with_window(session_name.clone(), window_index))
}

pub(super) fn find_session_name_by_id(
    state: &HandlerState,
    session_id: u32,
) -> Option<rmux_proto::SessionName> {
    state.sessions.iter().find_map(|(session_name, session)| {
        (session.id().as_u32() == session_id).then_some(session_name.clone())
    })
}

pub(super) fn decode_prompt_key_guess(bytes: &[u8]) -> Option<PromptInputEvent> {
    super::super::pane_support::decode_prompt_input_event(bytes).map(|(event, _)| event)
}
