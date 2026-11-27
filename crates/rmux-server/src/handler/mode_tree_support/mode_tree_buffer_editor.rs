use super::super::attach_support::ActiveAttachIdentity;
use super::super::RequestHandler;
use super::mode_tree_model::{ModeTreeAction, ModeTreeActionIdentity, ModeTreeClientState};
use rmux_proto::{OptionName, PaneTarget, RmuxError, Target};

pub(in crate::handler) struct BufferEditRequest {
    pub(in crate::handler) action: ModeTreeActionIdentity,
    pub(in crate::handler) client: ActiveAttachIdentity,
    pub(in crate::handler) target: Target,
    pub(in crate::handler) editor: String,
    pub(in crate::handler) name: String,
    pub(in crate::handler) order: u64,
    pub(in crate::handler) content: Vec<u8>,
    mode: ModeTreeClientState,
}

impl RequestHandler {
    pub(super) async fn perform_buffer_edit_for_identity(
        &self,
        action: ModeTreeActionIdentity,
    ) -> Result<(), RmuxError> {
        let (client, mut mode) = {
            let clients = self.active_attach.lock().await;
            let Some(active) = clients.by_pid.get(&action.attach_pid()).filter(|active| {
                active.id == action.attach_id() && active.mode_tree_state_id == action.state_id()
            }) else {
                return Ok(());
            };
            let Some(mode) = active.mode_tree.clone() else {
                return Ok(());
            };
            (active.identity(action.attach_pid()), mode)
        };
        let _access = self.require_requester_origin_write(&mode.origin).await?;
        // Rebuilding may reconcile a vanished selection to a different buffer.
        // The edit belongs only to the identity captured before that rebuild.
        let Some(selected_id) = mode.selected_id.clone() else {
            return Ok(());
        };
        let build = self.build_mode_tree(&mut mode, action.attach_pid()).await?;
        let Some(ModeTreeAction::Buffer { name, order }) =
            build.items.get(&selected_id).map(|item| &item.action)
        else {
            return Ok(());
        };
        let (editor, content, target) = {
            let state = self.state.lock().await;
            let editor = state
                .options
                .resolve(None, OptionName::Editor)
                .unwrap_or("")
                .to_owned();
            if editor.is_empty() {
                return Ok(());
            }
            let (_, content, current_order) = state.buffers.show_with_order(Some(name))?;
            if *order != current_order {
                return Ok(());
            }
            let session = state
                .sessions
                .session(&mode.session_name)
                .ok_or_else(|| RmuxError::Server("buffer chooser session disappeared".into()))?;
            let pane = mode.host_pane.clone().unwrap_or_else(|| {
                PaneTarget::with_window(
                    mode.session_name.clone(),
                    session.active_window_index(),
                    session.active_pane_index(),
                )
            });
            (editor, content.to_vec(), Target::Pane(pane))
        };
        self.open_buffer_editor(BufferEditRequest {
            action,
            client,
            target,
            editor,
            name: name.clone(),
            order: *order,
            content,
            mode,
        })
        .await
    }

    pub(in crate::handler) async fn finish_buffer_editor(
        &self,
        request: BufferEditRequest,
        popup_id: u64,
        mut edited: Option<Vec<u8>>,
    ) {
        let Ok(_access) = self
            .require_requester_origin_write(&request.mode.origin)
            .await
        else {
            return;
        };
        if !request.content.ends_with(b"\n") {
            if let Some(content) = edited.as_mut().filter(|content| content.ends_with(b"\n")) {
                content.pop();
            }
        }
        let event = {
            let mut state = self.state.lock().await;
            let mut clients = self.active_attach.lock().await;
            let Some(_mutation) =
                super::super::lifecycle_producer_tasks::begin_current_lifecycle_mutation()
            else {
                return;
            };
            let Some(active) =
                clients
                    .by_pid
                    .get_mut(&request.action.attach_pid())
                    .filter(|active| {
                        request.client.matches_active(active)
                            && active.session_id == request.client.session_id()
                            && !active.suspended
                            && active.can_write
                            && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                            && active.mode_tree_state_id == request.action.state_id()
                            && active.mode_tree.is_some()
                            && active.overlay_state_id == popup_id
                            && active.overlay.is_none()
                    })
            else {
                return;
            };
            if state
                .sessions
                .session(&active.session_name)
                .is_none_or(|session| session.id() != request.client.session_id())
                || request
                    .mode
                    .host_identity
                    .as_ref()
                    .is_some_and(|host| !host.matches(&state))
            {
                return;
            }
            active.mode_tree_frame = None;
            let changed = edited.is_some_and(|content| {
                state
                    .buffers
                    .replace_if_order_matches(&request.name, request.order, content)
            });
            changed.then(|| {
                super::super::prepare_lifecycle_event(
                    &mut state,
                    &rmux_core::LifecycleEvent::PasteBufferChanged {
                        buffer_name: request.name,
                    },
                )
            })
        };
        if let Some(event) = event {
            self.emit_prepared(event).await;
        }
        let _ = self
            .refresh_mode_tree_overlay_for_action_identity(request.action)
            .await;
    }
}
