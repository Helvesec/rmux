use super::terminal_response::{
    decode_attached_terminal_control_after_append, TerminalControlEvent, TerminalResponseDecode,
};
use crate::handler::{attach_support::ActiveAttachIdentity, RequestHandler};
use crate::theme::Theme;
use std::sync::atomic::Ordering;

/// Only the THEME CSI family bypasses modal key decoding here. Other CSI,
/// including focus, keeps its existing modal admission.
pub(super) fn decode_modal_theme(input: &[u8]) -> TerminalResponseDecode {
    const PREFIX: &[u8] = b"\x1b[?997";
    if input.len() < PREFIX.len() && PREFIX.starts_with(input) {
        return TerminalResponseDecode::Partial;
    }
    if input.starts_with(PREFIX) {
        return decode_attached_terminal_control_after_append(input, false, 0);
    }
    TerminalResponseDecode::NotResponse
}

impl RequestHandler {
    pub(super) async fn record_client_theme(
        &self,
        identity: ActiveAttachIdentity,
        event: TerminalControlEvent,
    ) -> bool {
        let mut attached = self.active_attach.lock().await;
        let Some(client) = attached
            .by_pid
            .get_mut(&identity.attach_pid())
            .filter(|client| {
                identity.matches_active(client) && !client.closing.load(Ordering::SeqCst)
            })
        else {
            return false;
        };
        match event {
            TerminalControlEvent::ClientDarkTheme => client.theme.report = Theme::Dark,
            TerminalControlEvent::ClientLightTheme => client.theme.report = Theme::Light,
            TerminalControlEvent::ClientBackground(rgb) => client.theme.background = Some(rgb),
            _ => return false,
        }
        true
    }
}
