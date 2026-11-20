use super::modal::attached_mode_input_step;
use super::*;

impl RequestHandler {
    pub(super) async fn handle_attached_live_input_step(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
        windows_console_key: Option<rmux_proto::AttachedWindowsConsoleKey>,
        active_emit_cache: &mut ActiveClientEmitCache,
    ) -> io::Result<AttachedLiveInputStep> {
        #[cfg(test)]
        self.live_input_work
            .enter_step(pending_input.len() + bytes.len());
        let initial_transient_prefix = self
            .take_transient_terminal_prefix_for_identity(identity)
            .await;
        let has_transient_terminal_prefix = !initial_transient_prefix.is_empty();
        pending_input.extend(initial_transient_prefix);
        if let Some(input) = self.split_attached_modal_palette_input(pending_input, bytes) {
            // -N owns opaque strings and fragmented replies. The controller
            // splitter must not forward those strings or retain its fragments.
            if !self
                .attached_client_input_is_read_only_for_identity(identity)
                .await?
            {
                if let Some(step) = self
                    .handle_ignored_message_input_step(identity, pending_input, bytes)
                    .await?
                {
                    return Ok(step);
                }
            }
            pending_input.clear();
            return self
                .handle_attached_segmented_input(identity, pending_input, input, active_emit_cache)
                .await;
        }
        self.handle_attached_live_input_step_after_controller(
            identity,
            pending_input,
            bytes,
            windows_console_key,
            active_emit_cache,
            has_transient_terminal_prefix,
        )
        .await
    }

    pub(super) async fn handle_attached_live_input_step_after_controller(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
        windows_console_key: Option<rmux_proto::AttachedWindowsConsoleKey>,
        active_emit_cache: &mut ActiveClientEmitCache,
        mut has_transient_terminal_prefix: bool,
    ) -> io::Result<AttachedLiveInputStep> {
        #[cfg(not(windows))]
        let _ = windows_console_key;
        #[cfg(windows)]
        let windows_console_key = windows_console_key
            .filter(|_| pending_input.is_empty() && !bytes.is_empty())
            .map(windows_console_key_event);
        #[cfg(windows)]
        let try_plain_fast_path = windows_console_key.is_none();
        #[cfg(not(windows))]
        let try_plain_fast_path = true;
        if try_plain_fast_path {
            if let Some(forwarded) = self
                .try_forward_plain_attached_bytes_fast(
                    identity,
                    pending_input,
                    bytes,
                    active_emit_cache,
                )
                .await?
            {
                return Ok(AttachedLiveInputStep::Complete(forwarded));
            }
        }
        if self
            .attached_client_input_is_read_only_for_identity(identity)
            .await?
        {
            if has_transient_terminal_prefix {
                pending_input.clear();
            }
            let backspace = self.attached_backspace_byte().await;
            #[cfg(windows)]
            let windows_key_override = windows_console_key.and_then(|key_event| {
                let AttachedKeyDecode::Matched { size, key } =
                    decode_live_attached_key(bytes, backspace)
                else {
                    return None;
                };
                (size == bytes.len()).then(|| windows_console_binding_key(key, key_event))
            });
            #[cfg(not(windows))]
            let windows_key_override = None;
            self.handle_read_only_client_action_input(
                identity,
                pending_input,
                bytes,
                backspace,
                windows_key_override,
            )
            .await?;
            return Ok(AttachedLiveInputStep::Complete(false));
        }
        if let Some(step) = self
            .handle_ignored_message_input_step(identity, pending_input, bytes)
            .await?
        {
            return Ok(step);
        }
        self.clear_attached_focus_alerts(identity).await;
        let pending_was_empty = pending_input.is_empty();
        match self
            .handle_transient_message_input_for_identity(identity, pending_input, bytes)
            .await
        {
            // The timer may expire between the first residual-prefix drain
            // and this identity-guarded mutation. Pull the prefix once more
            // before ordinary decoding so a split terminal response remains
            // contiguous across the expiry boundary.
            TransientMessageInput::Inactive => {
                let residual = self
                    .take_transient_terminal_prefix_for_identity(identity)
                    .await;
                has_transient_terminal_prefix |= !residual.is_empty();
                pending_input.extend(residual);
            }
            TransientMessageInput::Consumed => {
                return Ok(AttachedLiveInputStep::Complete(false));
            }
            TransientMessageInput::TerminalControls(controls) => {
                return Ok(AttachedLiveInputStep::Complete(
                    self.handle_transient_terminal_controls(identity, controls)
                        .await?,
                ));
            }
            TransientMessageInput::Dismissed(rerouted)
                if pending_was_empty && rerouted.as_slice() == bytes =>
            {
                // Continue in this same step. Besides avoiding a second
                // parse, this preserves the native Windows KEY_EVENT that
                // belongs to these exact bytes.
                debug_assert!(pending_input.is_empty());
            }
            TransientMessageInput::Dismissed(bytes) => {
                return Ok(AttachedLiveInputStep::Reroute {
                    bytes,
                    forwarded: false,
                });
            }
        }
        if !has_transient_terminal_prefix {
            if let Some(step) = self
                .handle_attached_modal_input_step(identity, pending_input, bytes)
                .await?
            {
                return Ok(step);
            }
        }
        let (target, target_session_id) = self
            .attached_input_target_identity(identity)
            .await
            .map_err(io_other)?;
        self.emit_attached_client_active_if_changed(identity, &target, active_emit_cache)
            .await;
        if self
            .target_is_in_clock_mode(&target)
            .await
            .map_err(io_other)?
        {
            let new_input_at = pending_input.len();
            pending_input.extend_from_slice(bytes);
            match decode_bracketed_paste_after_append(pending_input, new_input_at) {
                BracketedPasteDecode::Matched { .. } => {
                    #[cfg(test)]
                    self.pause_before_live_clock_mode_exit_for_test(identity)
                        .await;
                    let _ = self
                        .exit_clock_mode_for_attached_identity(&target, identity, target_session_id)
                        .await
                        .map_err(io_other)?;
                    return Ok(AttachedLiveInputStep::Reroute {
                        bytes: std::mem::take(pending_input),
                        forwarded: false,
                    });
                }
                BracketedPasteDecode::Partial => {
                    if pending_input.len() > MAX_RETAINED_ATTACHED_CONTROL_INPUT {
                        #[cfg(test)]
                        self.pause_before_live_clock_mode_exit_for_test(identity)
                            .await;
                        let _ = self
                            .exit_clock_mode_for_attached_identity(
                                &target,
                                identity,
                                target_session_id,
                            )
                            .await
                            .map_err(io_other)?;
                        return Ok(AttachedLiveInputStep::Reroute {
                            bytes: std::mem::take(pending_input),
                            forwarded: false,
                        });
                    }
                    retain_partial_attached_control_input(
                        "clock mode bracketed paste",
                        pending_input,
                    )?;
                    return Ok(AttachedLiveInputStep::Complete(false));
                }
                BracketedPasteDecode::NotPaste => {}
            }
            if is_mouse_prefix(pending_input) {
                let last_mouse = self.attached_last_mouse_event_for_identity(identity).await;
                let consumed = match decode_mouse(pending_input, last_mouse) {
                    MouseDecode::Matched { size, .. } | MouseDecode::Discard { size } => size,
                    MouseDecode::Partial | MouseDecode::Overlong => {
                        retain_partial_attached_escape_input(
                            "clock mode mouse input",
                            pending_input,
                        )?;
                        return Ok(AttachedLiveInputStep::Complete(false));
                    }
                    MouseDecode::Invalid => 0,
                };
                if consumed > 0 {
                    pending_input.drain(..consumed);
                    #[cfg(test)]
                    self.pause_before_live_clock_mode_exit_for_test(identity)
                        .await;
                    let _ = self
                        .exit_clock_mode_for_attached_identity(&target, identity, target_session_id)
                        .await
                        .map_err(io_other)?;
                    let remaining =
                        (!pending_input.is_empty()).then(|| std::mem::take(pending_input));
                    return Ok(attached_mode_input_step(remaining));
                }
            }
            let backspace = self.attached_backspace_byte().await;
            let consumed = match self
                .decode_configured_user_key(pending_input)
                .await
                .unwrap_or_else(|| decode_attached_key(pending_input, backspace))
            {
                AttachedKeyDecode::Matched { size, .. } => size,
                AttachedKeyDecode::Partial => {
                    retain_partial_attached_escape_input("clock mode input", pending_input)?;
                    return Ok(AttachedLiveInputStep::Complete(false));
                }
                AttachedKeyDecode::Invalid => {
                    let Some((_, consumed)) = decode_prompt_input_event(pending_input) else {
                        retain_partial_attached_escape_input("clock mode input", pending_input)?;
                        return Ok(AttachedLiveInputStep::Complete(false));
                    };
                    consumed
                }
            };
            pending_input.drain(..consumed);
            #[cfg(test)]
            self.pause_before_live_clock_mode_exit_for_test(identity)
                .await;
            let _ = self
                .exit_clock_mode_for_attached_identity(&target, identity, target_session_id)
                .await
                .map_err(io_other)?;
            let remaining = (!pending_input.is_empty()).then(|| std::mem::take(pending_input));
            return Ok(attached_mode_input_step(remaining));
        }
        let target_in_copy_mode = self
            .target_is_in_copy_mode(&target)
            .await
            .map_err(io_other)?;
        let target_mode = self.target_pane_mode(&target).await.map_err(io_other)?;
        let target_focus_events = target_mode & mode::MODE_FOCUSON != 0;
        let backspace = self.attached_backspace_byte().await;

        #[cfg(windows)]
        if pending_input.is_empty() && bytes == b"\x04" {
            if let Some(key) = windows_key_code_named("C-d") {
                let handled = self
                    .handle_attached_live_key_inner(
                        identity,
                        key,
                        super::super::AttachedPaneForward::WindowsConsoleKey {
                            key: WindowsConsoleKeyEvent::ctrl_d(),
                            bytes,
                        },
                    )
                    .await?;
                return Ok(AttachedLiveInputStep::Complete(!handled));
            }
        }

        #[cfg(windows)]
        if let Some(key_event) = windows_console_key.filter(|_| pending_input.is_empty()) {
            if let AttachedKeyDecode::Matched { size, key } = decode_attached_key(bytes, backspace)
            {
                if size == bytes.len() {
                    if let Some(key) = windows_console_binding_override_key(key, key_event) {
                        let handled = self
                            .handle_attached_live_key_inner(
                                identity,
                                key,
                                super::super::AttachedPaneForward::WindowsConsoleKey {
                                    key: key_event,
                                    bytes,
                                },
                            )
                            .await?;
                        return Ok(AttachedLiveInputStep::Complete(!handled));
                    }
                }
            }
        }

        let paste_limit = self.attached_paste_limit(identity).await;
        if paste_limit == 0
            && pending_input.is_empty()
            && !self.attached_key_table_active(identity).await
            && self
                .dispatch_immediate_prefix_detach(identity, &target, bytes, backspace)
                .await?
        {
            return Ok(AttachedLiveInputStep::Complete(false));
        }

        self.decode_attached_live_input(
            identity,
            pending_input,
            bytes,
            LiveInputDecode {
                target,
                target_in_copy_mode,
                target_focus_events,
                backspace,
                paste_limit,
                #[cfg(windows)]
                windows_console_key,
            },
        )
        .await
    }
}
