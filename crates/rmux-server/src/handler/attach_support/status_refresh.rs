use std::sync::atomic::Ordering;

use rmux_proto::{RmuxError, SessionId, SessionName};

use crate::handler::client_runtime_support::ListClientSnapshot;
use crate::handler::{attach_support, prompt_support, RequestHandler};
use crate::handler_support::attached_client_required;
use crate::key_table::effective_client_key_table_name;
use crate::outer_terminal::{ClientTitleUpdate, OuterTerminal, RenderedClientTitle};
use crate::pane_io::AttachControl;
use crate::pane_terminals::session_not_found;

impl RequestHandler {
    pub(in crate::handler) async fn send_attached_status_if_unobscured(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        expected_session_name: &SessionName,
        expected_session_id: SessionId,
        frame: (
            Vec<u8>,
            Option<RenderedClientTitle>,
            Option<crate::renderer::OverlayRect>,
        ),
    ) -> Result<(), rmux_proto::RmuxError> {
        let (bytes, client_title, status_region) = frame;
        let mut state = self.state.lock().await;
        let mut active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
            return Err(attached_client_required("refresh-client"));
        };
        if active.id != expected_attach_id
            || &active.session_name != expected_session_name
            || active.session_id != expected_session_id
            || active.closing.load(Ordering::SeqCst)
        {
            return Err(attached_client_required("refresh-client"));
        }
        // Check the current overlay under the delivery lock: it may have
        // opened or moved after rendering. Repainting it after the status
        // would still expose the covered cells for a frame.
        if active.transient_message.is_some()
            || status_region.is_some_and(|region| {
                active
                    .overlay
                    .as_ref()
                    .is_some_and(|overlay| overlay.intersects(region))
            })
        {
            return Ok(());
        }
        let outer_terminal =
            OuterTerminal::resolve(&state.options, active.terminal_context.clone());
        let bytes = outer_terminal.wrap_render_frame(&bytes);
        // Remember before the send: a failed send takes the client down, and a
        // partially written frame must not leave a title claimed as delivered
        // that a later reconnect would then skip. The next client registers
        // fresh with no remembered title of its own.
        active.remember_client_title(client_title.as_ref());
        if let Err(error) = active.control_tx.send(AttachControl::Write(bytes)) {
            if error.is_full() {
                active.closing.store(true, Ordering::SeqCst);
            }
            active_attach.remove_attached_client(attach_pid);
            self.bump_active_attach_epoch();
            let focus_effects = crate::handler::focus_support::prepare_server_focus_effects(
                &mut state,
                &mut active_attach,
            );
            let result = if error.is_full() {
                Err(rmux_proto::RmuxError::Server(
                    "attached client is not draining updates".to_owned(),
                ))
            } else {
                Err(attached_client_required("refresh-client"))
            };
            drop(active_attach);
            drop(state);
            let _ = focus_effects.execute(self).await;
            return result;
        }
        Ok(())
    }

    pub(crate) async fn refresh_attached_client_status(
        &self,
        attach_pid: u32,
        session_name: &rmux_proto::SessionName,
    ) -> Result<(), RmuxError> {
        self.refresh_attached_client_status_with_expected_identity(attach_pid, None, session_name)
            .await
    }

    pub(crate) async fn refresh_attached_client_status_for_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        session_name: &rmux_proto::SessionName,
    ) -> Result<(), RmuxError> {
        self.refresh_attached_client_status_with_expected_identity(
            attach_pid,
            Some(expected_attach_id),
            session_name,
        )
        .await
    }

    async fn refresh_attached_client_status_with_expected_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: Option<u64>,
        session_name: &rmux_proto::SessionName,
    ) -> Result<(), RmuxError> {
        let attached_count = self.attached_count(session_name).await;
        let client_loop_snapshots = self.client_loop_snapshots().await;
        let (
            prompt,
            terminal_context,
            client_size,
            key_table,
            previous_title,
            client,
            current_attach_id,
            current_session_id,
        ) = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| {
                    expected_attach_id.is_none_or(|expected| active.id == expected)
                        && &active.session_name == session_name
                })
                .ok_or_else(|| attached_client_required("refresh-client"))?;
            if active.transient_message.is_some() {
                return Ok(());
            }
            (
                active
                    .prompt
                    .as_ref()
                    .map(prompt_support::ClientPromptState::rendered_prompt),
                active.terminal_context.clone(),
                active.client_size,
                active.key_table_name.clone(),
                active.client_title.clone(),
                ListClientSnapshot::from_attached_client(attach_pid, active),
                active.id,
                active.session_id,
            )
        };
        let socket_path = self.socket_path();
        let status_frame = {
            let state = self.state.lock().await;
            let session = state
                .sessions
                .session(session_name)
                .ok_or_else(|| session_not_found(session_name))?;
            if session.id() != current_session_id {
                return Err(attached_client_required("refresh-client"));
            }
            let key_table = effective_client_key_table_name(&state, session, key_table.as_deref());
            let mut session = attach_support::sized_session(session, Some(client_size));
            let mut pan = client.pan;
            let viewport = pan.resolve(&state, session.view(), client_size);
            session.set_viewport_offset(viewport.bigger.then_some((viewport.x, viewport.y)));
            let outer_terminal = OuterTerminal::resolve(&state.options, terminal_context);
            // tmux re-expands `set-titles-string` on every server loop, not only
            // when the client redraws, so a title carrying a `#(shell-command)`
            // job, a strftime token or `#{session_alerts}` keeps up. This status
            // tick is rmux's equivalent periodic pass; the expansion is skipped
            // outright while `set-titles` is off (issue #182).
            let client_bindings = client
                .resolved_for_render(outer_terminal.features_string(), &key_table)
                .format_bindings();
            let resolved_title = crate::renderer::expand_client_title(
                session.view(),
                &state.options,
                &crate::renderer::ClientTitleContext {
                    state: Some(&state),
                    attached_count,
                    key_table: Some(&key_table),
                    socket_path: Some(&socket_path),
                    client_loop_snapshots: Some(&client_loop_snapshots),
                    client: Some(&client_bindings),
                },
            );
            let title_update = ClientTitleUpdate {
                resolved: resolved_title.as_deref(),
                // The OSC 7 path follows the active pane's own report, which
                // only the prelude reads; the tick draws no prelude, so this
                // client keeps whatever it was already told.
                path: crate::outer_terminal::ClientPathUpdate::Unread,
                previous: Some(&previous_title),
            };
            let frame = crate::renderer::render_status_only_with_attached_count_and_prompt(
                session.view(),
                &state.options,
                attached_count,
                crate::renderer::StatusRenderContext {
                    prompt: prompt.as_ref(),
                    client: Some(&client_bindings),
                    state: Some(&state),
                    key_table: Some(&key_table),
                    socket_path: Some(&socket_path),
                    client_loop_snapshots: Some(&client_loop_snapshots),
                    ..crate::renderer::StatusRenderContext::default()
                },
            );
            // Delivery checks the current overlay before sending the status frame.
            let mut bytes = outer_terminal.render_client_title(title_update);
            bytes.extend_from_slice(&frame);
            (
                bytes,
                outer_terminal.rendered_client_title(title_update),
                crate::renderer::StatusGeometry::for_session(session.view(), &state.options)
                    .status_rect(),
            )
        };
        self.send_attached_status_if_unobscured(
            attach_pid,
            current_attach_id,
            session_name,
            current_session_id,
            status_frame,
        )
        .await
    }
}
