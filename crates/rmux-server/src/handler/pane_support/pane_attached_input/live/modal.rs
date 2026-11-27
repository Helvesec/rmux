use super::*;

impl RequestHandler {
    pub(super) async fn handle_ignored_message_input_step(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> io::Result<Option<AttachedLiveInputStep>> {
        let input = self
            .handle_ignored_transient_message_input_for_identity(identity, pending_input, bytes)
            .await;
        if matches!(input, TransientMessageInput::Inactive) {
            return Ok(None);
        }
        self.clear_attached_focus_alerts(identity).await;
        let forwarded = match input {
            TransientMessageInput::TerminalControls(controls) => {
                self.handle_transient_terminal_controls(identity, controls)
                    .await?
            }
            TransientMessageInput::Dismissed(_) => {
                debug_assert!(
                    false,
                    "ignore-only transient input cannot dismiss a message"
                );
                false
            }
            TransientMessageInput::Consumed | TransientMessageInput::Inactive => false,
        };
        Ok(Some(AttachedLiveInputStep::Complete(forwarded)))
    }

    pub(super) async fn handle_attached_modal_input_step(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> io::Result<Option<AttachedLiveInputStep>> {
        match self
            .handle_transient_message_input_for_identity(identity, pending_input, bytes)
            .await
        {
            TransientMessageInput::Inactive => {
                let residual = self
                    .take_transient_terminal_prefix_for_identity(identity)
                    .await;
                if !residual.is_empty() {
                    pending_input.extend(residual);
                    return Ok(None);
                }
            }
            TransientMessageInput::Consumed => {
                return Ok(Some(AttachedLiveInputStep::Complete(false)));
            }
            TransientMessageInput::TerminalControls(controls) => {
                return Ok(Some(AttachedLiveInputStep::Complete(
                    self.handle_transient_terminal_controls(identity, controls)
                        .await?,
                )));
            }
            TransientMessageInput::Dismissed(bytes) => {
                return Ok(Some(AttachedLiveInputStep::Reroute {
                    bytes,
                    forwarded: false,
                }));
            }
        }
        let arriving = input_arrives_now(identity);
        if arriving && self.prompt_active_for_identity(identity).await {
            let remaining = self
                .handle_attached_prompt_input(identity, pending_input, bytes)
                .await?;
            return Ok(Some(attached_mode_input_step(remaining)));
        }
        if self.mode_tree_active_for_identity(identity).await {
            let remaining = self
                .handle_attached_mode_tree_input(identity, pending_input, bytes)
                .await?;
            return Ok(Some(attached_mode_input_step(remaining)));
        }
        if arriving && self.overlay_active_for_identity(identity).await {
            return match self
                .handle_attached_overlay_input_for_identity(identity, pending_input, bytes)
                .await?
            {
                AttachedOverlayInput::Consumed => Ok(Some(AttachedLiveInputStep::Complete(false))),
                AttachedOverlayInput::Reroute(bytes) => Ok(Some(AttachedLiveInputStep::Reroute {
                    bytes,
                    forwarded: false,
                })),
            };
        }
        if arriving && self.display_panes_takes_input_for_identity(identity).await {
            let remaining = self
                .handle_attached_display_panes_input_for_identity(identity, pending_input, bytes)
                .await?;
            return Ok(Some(attached_mode_input_step(remaining)));
        }
        Ok(None)
    }

    pub(in crate::handler::pane_support::pane_attached_input) async fn handle_transient_terminal_controls(
        &self,
        identity: ActiveAttachIdentity,
        controls: Vec<Vec<u8>>,
    ) -> io::Result<bool> {
        let mut forwarded = false;
        for bytes in controls {
            // Hooks emitted by one report may switch the client, pane, or
            // terminal mode. Match the ordinary live path by re-resolving
            // before every subsequent report in the same input chunk.
            let (target, _) = self
                .attached_input_target_identity(identity)
                .await
                .map_err(io_other)?;
            let decoded = decode_focus_event(&bytes).map_or_else(
                || decode_attached_terminal_control_after_append(&bytes, false, 0),
                |event| TerminalResponseDecode::Matched {
                    size: bytes.len(),
                    event: Some(event),
                },
            );
            match decoded {
                TerminalResponseDecode::PaneBound { .. } => {
                    // Without a pane-originated outer-terminal query token,
                    // `CSI ... R` is ambiguous with xterm modified F-keys
                    // (for example Shift-F3 is `CSI 1;2 R`). Under `-N` it
                    // must remain ignored like every other key press.
                }
                TerminalResponseDecode::PaletteResponse { index, .. } => {
                    forwarded |= self
                        .write_attached_palette_response_for_identity(identity, index, &bytes)
                        .await?;
                }
                TerminalResponseDecode::ClipboardResponse {
                    selection, content, ..
                } => {
                    forwarded |= self
                        .handle_attached_clipboard_response(identity, selection, content)
                        .await?;
                }
                TerminalResponseDecode::Matched {
                    event: Some(event), ..
                } => {
                    forwarded |= self
                        .handle_attached_terminal_control_event(identity, &target, event)
                        .await?;
                }
                TerminalResponseDecode::Matched { event: None, .. } => {}
                TerminalResponseDecode::NotResponse | TerminalResponseDecode::Partial => {
                    debug_assert!(false, "transient input emitted an unexpected control");
                }
            }
        }
        Ok(forwarded)
    }

    pub(super) async fn handle_attached_segmented_input(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        input: ModalPaletteInput,
        active_emit_cache: &mut ActiveClientEmitCache,
    ) -> io::Result<AttachedLiveInputStep> {
        let ModalPaletteInput { segments, retained } = input;
        let mut segments = VecDeque::from(segments);
        let mut modal_pending = Vec::new();
        let mut forwarded = false;

        while let Some(segment) = segments.pop_front() {
            let bytes = match segment {
                ModalPaletteInputSegment::Input(bytes) => bytes,
                ModalPaletteInputSegment::TerminalControl { event, .. } => {
                    let (target, _) = self
                        .attached_input_target_identity(identity)
                        .await
                        .map_err(io_other)?;
                    forwarded |= self
                        .handle_attached_terminal_control_event(identity, &target, event)
                        .await?;
                    continue;
                }
                ModalPaletteInputSegment::PaneBound(bytes) => {
                    self.write_attached_target_bytes_for_identity(identity, &bytes)
                        .await?;
                    forwarded = true;
                    continue;
                }
                ModalPaletteInputSegment::Response { index, bytes } => {
                    if self
                        .write_attached_palette_response_for_identity(identity, index, &bytes)
                        .await?
                    {
                        forwarded = true;
                    }
                    // A syntactically valid OSC 4 response is terminal
                    // protocol input, never a modal keystroke. Matching
                    // responses reach the pane above; unsolicited, mismatched,
                    // or duplicate responses are discarded just as they are
                    // on the ordinary live-input path. Feeding a duplicate
                    // through a prompt or mode-tree would let its leading ESC
                    // close the surface after another attached client had
                    // already consumed the shared query slot.
                    continue;
                }
                ModalPaletteInputSegment::ClipboardResponse {
                    selection, content, ..
                } => {
                    forwarded |= self
                        .handle_attached_clipboard_response(identity, selection, content)
                        .await?;
                    continue;
                }
            };

            let step = self
                .handle_attached_live_input_step_after_controller(
                    identity,
                    &mut modal_pending,
                    LegacyInput::Bytes(&bytes),
                    None,
                    active_emit_cache,
                    false,
                )
                .await?;
            match step {
                AttachedLiveInputStep::Complete(step_forwarded) => {
                    forwarded |= step_forwarded;
                }
                AttachedLiveInputStep::Reroute {
                    bytes,
                    forwarded: step_forwarded,
                } => {
                    let mut rerouted = modal_pending;
                    rerouted.extend_from_slice(&bytes);
                    append_modal_palette_remainder(&mut rerouted, segments, &retained);
                    return Ok(AttachedLiveInputStep::Reroute {
                        bytes: rerouted,
                        forwarded: forwarded | step_forwarded,
                    });
                }
            }
        }

        modal_pending.extend_from_slice(&retained);
        if modal_pending.is_empty() {
            return Ok(AttachedLiveInputStep::Complete(forwarded));
        }
        *pending_input = modal_pending;
        retain_partial_attached_control_input("attached terminal control", pending_input)?;
        Ok(AttachedLiveInputStep::Complete(forwarded))
    }

    /// Whether a modal surface of the client takes this input; a popup, menu, prompt or
    /// display-panes takes only input that arrives while it is up (see `input_arrives_now`).
    pub(in crate::handler::pane_support::pane_attached_input) async fn attached_modal_surface_takes_input_for_identity(
        &self,
        identity: ActiveAttachIdentity,
    ) -> bool {
        let arriving = input_arrives_now(identity);
        let active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
        else {
            return false;
        };
        active.mode_tree.is_some()
            || active.transient_message.is_some()
            || (arriving
                && (active.prompt.is_some()
                    || active.overlay.is_some()
                    || active.display_panes.is_some()))
    }
}

pub(super) fn attached_mode_input_step(remaining: Option<Vec<u8>>) -> AttachedLiveInputStep {
    match remaining {
        Some(bytes) => AttachedLiveInputStep::Reroute {
            bytes,
            forwarded: false,
        },
        None => AttachedLiveInputStep::Complete(false),
    }
}

fn append_modal_palette_remainder(
    bytes: &mut Vec<u8>,
    mut segments: VecDeque<ModalPaletteInputSegment>,
    retained: &[u8],
) {
    while let Some(segment) = segments.pop_front() {
        bytes.extend_from_slice(&segment.into_bytes());
    }
    bytes.extend_from_slice(retained);
}
