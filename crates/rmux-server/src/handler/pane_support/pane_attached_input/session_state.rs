use std::io;

use rmux_core::{key_code_to_bytes, key_string_lookup_string};
use rmux_proto::{OptionName, PaneTarget, RmuxError};

use super::ensure_session_identity;
use crate::client_flags::ClientFlags;
use crate::handler::attach_support::ActiveAttachIdentity;
use crate::handler::pane_support::{io_other, resolve_input_target};
use crate::handler::RequestHandler;
use crate::pane_terminals::HandlerState;

impl RequestHandler {
    pub(in crate::handler::pane_support) async fn with_live_input_session_state<T>(
        &self,
        identity: ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        mutate: impl FnOnce(&mut HandlerState) -> io::Result<T>,
    ) -> io::Result<Option<T>> {
        // Publication of a replacement attach and preparation of pane input
        // are linearized by this lock order.  No lock is retained across the
        // eventual PTY write.
        let mut state = self.state.lock().await;
        let active_attach = self.active_attach.lock().await;
        let Some(_active) = active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active_session(active, session_name, session_id)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    && active.can_write
                    && !active.flags.contains(ClientFlags::READONLY)
            })
        else {
            return Ok(None);
        };
        ensure_session_identity(&state, session_name, session_id).map_err(io_other)?;
        mutate(&mut state).map(Some)
    }

    pub(super) async fn with_live_input_state<T>(
        &self,
        identity: ActiveAttachIdentity,
        target: &PaneTarget,
        session_id: rmux_proto::SessionId,
        mutate: impl FnOnce(&mut HandlerState) -> io::Result<T>,
    ) -> io::Result<Option<T>> {
        self.with_live_input_session_state(identity, target.session_name(), session_id, mutate)
            .await
    }

    pub(super) async fn attached_client_input_is_read_only_for_identity(
        &self,
        identity: ActiveAttachIdentity,
    ) -> io::Result<bool> {
        let active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .ok_or_else(|| io_other(RmuxError::Server("attached client disappeared".to_owned())))?;
        Ok(!active.can_write || active.flags.contains(ClientFlags::READONLY))
    }

    #[cfg(test)]
    pub(in crate::handler) async fn attached_input_target(
        &self,
        attach_pid: u32,
    ) -> Result<PaneTarget, RmuxError> {
        let session_name = self.attached_session_name(attach_pid).await?;
        let state = self.state.lock().await;
        resolve_input_target(&state, None, Some(&session_name))
    }

    pub(in crate::handler) async fn attached_input_target_identity(
        &self,
        identity: ActiveAttachIdentity,
    ) -> Result<(PaneTarget, rmux_proto::SessionId), RmuxError> {
        let (session_name, session_id) = self
            .attached_session_identity_for_identity(identity)
            .await?;
        let state = self.state.lock().await;
        ensure_session_identity(&state, &session_name, session_id)?;
        resolve_input_target(&state, None, Some(&session_name)).map(|target| (target, session_id))
    }

    pub(crate) async fn attached_session_name(
        &self,
        attach_pid: u32,
    ) -> Result<rmux_proto::SessionName, RmuxError> {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .map(|active| active.session_name.clone())
            .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))
    }

    pub(crate) async fn attached_session_name_for_identity(
        &self,
        identity: crate::handler::attach_support::ActiveAttachIdentity,
    ) -> Result<rmux_proto::SessionName, RmuxError> {
        self.attached_session_identity_for_identity(identity)
            .await
            .map(|(session_name, _)| session_name)
    }

    pub(crate) async fn attached_session_identity_for_identity(
        &self,
        identity: crate::handler::attach_support::ActiveAttachIdentity,
    ) -> Result<(rmux_proto::SessionName, rmux_proto::SessionId), RmuxError> {
        self.attached_session_identity_and_client_name_for_identity(identity)
            .await
            .map(|(session_name, session_id, _)| (session_name, session_id))
    }

    pub(crate) async fn attached_session_identity_and_client_name_for_identity(
        &self,
        identity: crate::handler::attach_support::ActiveAttachIdentity,
    ) -> Result<(rmux_proto::SessionName, rmux_proto::SessionId, String), RmuxError> {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .map(|active| {
                (
                    active.session_name.clone(),
                    active.session_id,
                    active.client_name.clone(),
                )
            })
            .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))
    }

    /// Whether the pane that would consume this client's OSC 4 responses has a
    /// query still active, tmux's non-empty `c->input_requests`.
    pub(crate) async fn attached_palette_query_is_active_for_identity(
        &self,
        identity: ActiveAttachIdentity,
    ) -> bool {
        let Ok((target, _)) = self.attached_input_target_identity(identity).await else {
            return false;
        };
        let state = self.state.lock().await;
        state
            .transcript_handle(&target)
            .is_ok_and(|transcript| transcript.lock().has_active_palette_query())
    }

    pub(super) async fn target_pane_mode(&self, target: &PaneTarget) -> Result<u32, RmuxError> {
        let state = self.state.lock().await;
        let transcript = state.transcript_handle(target)?;
        let mode = transcript.lock().mode();
        Ok(mode)
    }

    #[cfg(test)]
    pub(crate) async fn start_attached_input_capture_for_test(&self, target: &PaneTarget) {
        let state = self.state.lock().await;
        state.start_pane_input_capture_for_test(target);
    }

    #[cfg(test)]
    pub(crate) async fn attached_input_capture_for_test(
        &self,
        target: &PaneTarget,
    ) -> Option<Vec<u8>> {
        let state = self.state.lock().await;
        state.pane_input_capture_for_test(target)
    }

    pub(in crate::handler) async fn attached_backspace_byte(&self) -> Option<u8> {
        let state = self.state.lock().await;
        state
            .options
            .resolve(None, OptionName::Backspace)
            .and_then(key_string_lookup_string)
            .and_then(key_code_to_bytes)
            .and_then(|bytes| (bytes.len() == 1).then_some(bytes[0]))
    }
}
