use super::*;

impl RequestHandler {
    pub(super) async fn try_forward_plain_attached_bytes_fast(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &[u8],
        bytes: &[u8],
        active_emit_cache: &mut ActiveClientEmitCache,
    ) -> io::Result<Option<bool>> {
        if !pending_input.is_empty() || !is_plain_attached_fast_path_input(bytes) {
            return Ok(None);
        }

        if self.attached_paste_limit(identity).await != 0 {
            return Ok(None);
        }

        let Some((session_name, session_id)) = self.fast_path_attached_session(identity).await?
        else {
            return Ok(None);
        };
        let prepared = self
            .with_live_input_session_state(identity, &session_name, session_id, |state| {
                let target =
                    resolve_input_target(state, None, Some(&session_name)).map_err(io_other)?;
                let transcript = state.transcript_handle(&target).map_err(io_other)?;
                {
                    let transcript = transcript.lock();
                    if transcript.copy_mode_state().is_some()
                        || transcript.clock_mode_generation().is_some()
                        || transcript.mode() & mode::MODE_MOUSE_ALL != 0
                    {
                        return Ok(None);
                    }
                }
                if plain_input_requires_key_dispatch(bytes, &target, state) {
                    return Ok(None);
                }
                if let Some(submitted) = submitted_text_before_enter(bytes) {
                    state
                        .record_attached_submitted_text(&target, submitted)
                        .map_err(io_other)?;
                }
                let clear_alerts_changed = state
                    .sessions
                    .clear_winlink_alert_flags(
                        &rmux_proto::WindowTarget::with_window(
                            session_name.clone(),
                            target.window_index(),
                        ),
                        rmux_core::WINLINK_ALERTFLAGS,
                    )
                    .unwrap_or(false);
                let writes =
                    prepare_attached_pane_input_writes(state, &target, bytes).map_err(io_other)?;
                Ok(Some((target, writes, clear_alerts_changed)))
            })
            .await?
            .flatten();
        let Some((target, writes, clear_alerts_changed)) = prepared else {
            return Ok(None);
        };

        self.emit_attached_client_active_if_changed(identity, &target, active_emit_cache)
            .await;
        for write in writes {
            write_attached_bytes_to_target_io(write, bytes.to_vec())
                .await
                .map_err(io_other)?;
        }
        if clear_alerts_changed {
            let handler = self.clone();
            let refresh_session_name = session_name.clone();
            tokio::spawn(async move {
                handler
                    .refresh_attached_session_for_session_identity(
                        &refresh_session_name,
                        session_id,
                    )
                    .await;
            });
        }
        Ok(Some(true))
    }

    pub(super) async fn fast_path_attached_session(
        &self,
        identity: ActiveAttachIdentity,
    ) -> io::Result<Option<(rmux_proto::SessionName, rmux_proto::SessionId)>> {
        let active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .ok_or_else(|| {
                io_other(rmux_proto::RmuxError::Server(
                    "attached client disappeared".to_owned(),
                ))
            })?;
        if !active.can_write || active.flags.contains(ClientFlags::READONLY) {
            return Ok(None);
        }
        if active.prompt.is_some()
            || active.mode_tree.is_some()
            || active.overlay.is_some()
            || active.display_panes.is_some()
            || active.transient_message.is_some()
            || active.key_table_name.is_some()
        {
            return Ok(None);
        }
        Ok(Some((active.session_name.clone(), active.session_id)))
    }

    pub(super) async fn attached_live_input_can_use_reroute_chunks(
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
            .ok_or_else(|| {
                io_other(rmux_proto::RmuxError::Server(
                    "attached client disappeared".to_owned(),
                ))
            })?;
        Ok(active.can_write
            && !active.flags.contains(ClientFlags::READONLY)
            && active.prompt.is_none()
            && active.mode_tree.is_none()
            && active.overlay.is_none()
            && active.display_panes.is_none()
            && active.transient_message.is_none())
    }
}

fn submitted_text_before_enter(bytes: &[u8]) -> Option<&[u8]> {
    let enter = bytes
        .iter()
        .position(|byte| matches!(*byte, b'\r' | b'\n'))?;
    (enter > 0).then_some(&bytes[..enter])
}
