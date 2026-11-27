//! Copy-mode transfers: paste-buffer writes, attached clipboard and pipe commands.

use super::pipe_command::run_pipe_command;
use super::{ActiveAttachIdentity, RequestHandler};
use crate::copy_mode::{
    CopyBufferTarget, CopyModeCommandContext, CopyModePipeCommand, CopyModeTransfer,
};
use crate::outer_terminal::OuterTerminal;
use crate::pane_io::AttachControl;
use rmux_core::LifecycleEvent;
use rmux_proto::{OptionName, RmuxError};

impl RequestHandler {
    pub(super) async fn apply_copy_mode_transfer(
        &self,
        requester_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        context: &CopyModeCommandContext,
        transfer: CopyModeTransfer,
    ) -> Result<(), RmuxError> {
        let writes_buffer = transfer.buffer_target.is_some();
        if let Some(buffer_target) = transfer.buffer_target.clone() {
            self.store_copy_mode_buffer(buffer_target, transfer.append, &transfer.data)
                .await?;
        }
        if writes_buffer {
            match identity {
                Some(identity) => {
                    self.copy_mode_bytes_to_attached_clipboard_for_identity(
                        identity,
                        &transfer.data,
                    )
                    .await;
                }
                None => {
                    self.copy_mode_bytes_to_attached_clipboard(requester_pid, &transfer.data)
                        .await;
                }
            }
        }
        if let Some(command) = self
            .resolve_copy_mode_pipe_command(transfer.pipe_command.as_ref())
            .await
        {
            run_pipe_command(
                self,
                &context.default_shell,
                &command,
                context.working_directory.as_ref(),
                &transfer.data,
            )
            .await?;
        }
        Ok(())
    }

    async fn copy_mode_bytes_to_attached_clipboard(&self, requester_pid: u32, bytes: &[u8]) {
        let Some((attach_pid, terminal_context)) = self
            .clipboard_attach_for_requester(requester_pid, "copy-mode")
            .await
        else {
            return;
        };
        let payload = {
            let state = self.state.lock().await;
            OuterTerminal::resolve(&state.options, terminal_context).encode_clipboard_set(bytes)
        };
        let Some(payload) = payload else {
            return;
        };
        let _ = self
            .send_attach_control(attach_pid, AttachControl::Write(payload), "copy-mode")
            .await;
    }

    async fn copy_mode_bytes_to_attached_clipboard_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        bytes: &[u8],
    ) {
        let (terminal_context, session_id) = {
            let active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach
                .by_pid
                .get(&identity.attach_pid())
                .filter(|active| {
                    identity.matches_active(active)
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
            else {
                return;
            };
            (active.terminal_context.clone(), active.session_id)
        };
        let payload = {
            let state = self.state.lock().await;
            OuterTerminal::resolve(&state.options, terminal_context).encode_clipboard_set(bytes)
        };
        let Some(payload) = payload else {
            return;
        };
        let _ = self
            .send_attach_control_for_client_current_session_identity(
                identity.attach_pid(),
                identity.attach_id(),
                session_id,
                AttachControl::Write(payload),
                "copy-mode",
            )
            .await;
    }

    async fn resolve_copy_mode_pipe_command(
        &self,
        pipe_command: Option<&CopyModePipeCommand>,
    ) -> Option<String> {
        match pipe_command {
            Some(CopyModePipeCommand::Explicit(command)) => Some(command.clone()),
            Some(CopyModePipeCommand::CopyCommandOption) => {
                let state = self.state.lock().await;
                state
                    .options
                    .resolve(None, OptionName::CopyCommand)
                    .filter(|command| !command.is_empty())
                    .map(str::to_owned)
            }
            None => None,
        }
    }

    async fn store_copy_mode_buffer(
        &self,
        target: CopyBufferTarget,
        append: bool,
        data: &[u8],
    ) -> Result<(), RmuxError> {
        let (buffer_name, evicted) = {
            let mut state = self.state.lock().await;
            let buffer_limit = state
                .options
                .resolve(None, OptionName::BufferLimit)
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(50);

            let outcome = match target {
                CopyBufferTarget::New(name) => {
                    state
                        .buffers
                        .set(name.as_deref(), data.to_vec(), buffer_limit)?
                }
                CopyBufferTarget::Top if append => {
                    if let Ok((name, existing)) = state
                        .buffers
                        .show(None)
                        .map(|(name, existing)| (name.to_owned(), existing.to_vec()))
                    {
                        let mut combined = Vec::with_capacity(existing.len() + data.len());
                        combined.extend_from_slice(&existing);
                        combined.extend_from_slice(data);
                        state.buffers.set(Some(&name), combined, buffer_limit)?
                    } else {
                        state.buffers.set(None, data.to_vec(), buffer_limit)?
                    }
                }
                CopyBufferTarget::Top => state.buffers.set(None, data.to_vec(), buffer_limit)?,
            };
            (
                outcome.buffer_name().map(str::to_owned),
                outcome.evicted().to_vec(),
            )
        };

        for evicted in evicted {
            self.emit(LifecycleEvent::PasteBufferDeleted {
                buffer_name: evicted,
            })
            .await;
        }
        if let Some(buffer_name) = buffer_name {
            self.emit(LifecycleEvent::PasteBufferChanged { buffer_name })
                .await;
        }

        Ok(())
    }
}
