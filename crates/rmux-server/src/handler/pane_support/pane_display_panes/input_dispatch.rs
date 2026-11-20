use std::io;

use rmux_core::{key_code_lookup_bits, key_code_to_bytes};
use rmux_proto::{
    ErrorResponse, OptionName, PaneTarget, Request, Response, RmuxError, SelectPaneRequest, Target,
};

use super::super::super::{
    attach_support::ActiveAttachIdentity,
    focus_support::prepare_server_focus_effects,
    prompt_support::{substitute_prompt_template, PromptInputEvent},
    scripting_support::{command_parser_from_state, QueueExecutionContext},
    with_expected_stable_target_identities, RequestHandler, RequesterOrigin, StableTargetIdentity,
};
use super::super::{
    decode_prompt_input_event, io_other, retain_partial_attached_escape_input,
    strip_bracketed_paste_markers_after_append,
};
use super::input_state::{update_display_panes_state, DisplayPanesOutcome};
use super::DEFAULT_DISPLAY_PANES_TEMPLATE;
use crate::key_table::{
    decode_attached_key, matches_prefix_key, session_option_key, AttachedKeyDecode,
};
use crate::pane_io::{AttachControl, AttachControlSender, OverlayFrame};

impl RequestHandler {
    async fn display_panes_active(&self, attach_pid: u32) -> bool {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .is_some_and(|active| active.display_panes.is_some())
    }

    async fn display_panes_active_for_identity(&self, identity: ActiveAttachIdentity) -> bool {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&identity.attach_pid())
            .is_some_and(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    && active.display_panes.is_some()
            })
    }

    async fn display_panes_prefix_input(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        input: &[u8],
    ) -> Result<DisplayPanesPrefixInput, RmuxError> {
        let session_name = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| identity.is_none_or(|identity| identity.matches_active(active)))
                .map(|active| active.session_name.clone())
        };
        let Some(session_name) = session_name else {
            return Ok(DisplayPanesPrefixInput::Other);
        };

        let (prefix_key, prefix2_key, prefix_bytes, prefix2_bytes, backspace) = {
            let state = self.state.lock().await;
            let prefix_key = session_option_key(&state, &session_name, OptionName::Prefix);
            let prefix2_key = session_option_key(&state, &session_name, OptionName::Prefix2);
            let prefix_bytes = prefix_key.and_then(key_code_to_bytes);
            let prefix2_bytes = prefix2_key.and_then(key_code_to_bytes);
            let backspace = state
                .options
                .resolve(None, OptionName::Backspace)
                .and_then(rmux_core::key_string_lookup_string)
                .and_then(key_code_to_bytes)
                .and_then(|bytes| (bytes.len() == 1).then_some(bytes[0]));
            (
                prefix_key,
                prefix2_key,
                prefix_bytes,
                prefix2_bytes,
                backspace,
            )
        };

        for prefix in [prefix_bytes.as_deref(), prefix2_bytes.as_deref()]
            .into_iter()
            .flatten()
        {
            if input == prefix {
                return Ok(DisplayPanesPrefixInput::Prefix);
            }
            if !input.is_empty() && prefix.starts_with(input) {
                return Ok(DisplayPanesPrefixInput::Partial);
            }
        }

        match decode_attached_key(input, backspace) {
            AttachedKeyDecode::Matched { key, .. }
                if matches_prefix_key(key_code_lookup_bits(key), prefix_key, prefix2_key) =>
            {
                Ok(DisplayPanesPrefixInput::Prefix)
            }
            _ => Ok(DisplayPanesPrefixInput::Other),
        }
    }

    pub(in crate::handler::pane_support) async fn handle_attached_display_panes_input_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> io::Result<Option<Vec<u8>>> {
        self.handle_attached_display_panes_input_with_identity(
            identity.attach_pid(),
            Some(identity),
            pending_input,
            bytes,
        )
        .await
    }

    async fn handle_attached_display_panes_input_with_identity(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> io::Result<Option<Vec<u8>>> {
        // See `handle_attached_prompt_input` for the paste-marker strip
        // rationale — display-panes decodes input as digit/letter keys and a
        // bracketed-paste envelope would cancel the overlay and leak the body.
        // The one-byte tmux prefix (e.g. C-b = 0x02) survives the strip so the
        // prefix passthrough path still triggers. Strip the CONCATENATED
        // buffer so a marker that straddles the pending_input / bytes seam
        // still collapses.
        let new_input_at = pending_input.len();
        pending_input.extend_from_slice(bytes);
        strip_bracketed_paste_markers_after_append(pending_input, new_input_at);
        match self
            .display_panes_prefix_input(attach_pid, identity, pending_input)
            .await
            .map_err(io_other)?
        {
            DisplayPanesPrefixInput::Prefix => {
                match identity {
                    Some(identity) => {
                        self.clear_display_panes_state_for_identity(identity, None, true)
                            .await
                    }
                    None => self.clear_display_panes_state(attach_pid, None, true).await,
                }
                .map_err(io_other)?;
                return Ok(Some(std::mem::take(pending_input)));
            }
            DisplayPanesPrefixInput::Partial => {
                retain_partial_attached_escape_input("display-panes prefix", pending_input)?;
                return Ok(None);
            }
            DisplayPanesPrefixInput::Other => {}
        }

        loop {
            let Some((event, consumed)) = decode_prompt_input_event(pending_input) else {
                retain_partial_attached_escape_input("display-panes prompt input", pending_input)?;
                return Ok(None);
            };
            pending_input.drain(..consumed);
            self.handle_display_panes_event(attach_pid, identity, event)
                .await
                .map_err(io_other)?;
            let active = match identity {
                Some(identity) => self.display_panes_active_for_identity(identity).await,
                None => self.display_panes_active(attach_pid).await,
            };
            if !active {
                break;
            }
        }

        Ok((!pending_input.is_empty()).then(|| std::mem::take(pending_input)))
    }

    async fn handle_display_panes_event(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        event: PromptInputEvent,
    ) -> Result<(), RmuxError> {
        let (action, focus_effects) =
            {
                let mut state = self.state.lock().await;
                let mut active_attach = self.active_attach.lock().await;
                let Some(active) = active_attach.by_pid.get_mut(&attach_pid).filter(|active| {
                    identity.is_none_or(|identity| identity.matches_active(active))
                }) else {
                    return Ok(());
                };
                let Some(display_state) = active.display_panes.as_mut() else {
                    return Ok(());
                };

                let action = match update_display_panes_state(display_state, event) {
                    DisplayPanesOutcome::Stay => None,
                    DisplayPanesOutcome::Close => {
                        let state = active
                            .display_panes
                            .take()
                            .expect("display-panes state exists");
                        active.overlay_generation = active.overlay_generation.saturating_add(1);
                        Some(DisplayPanesAction::Clear {
                            attach_pid,
                            control_tx: active.control_tx.clone(),
                            render_generation: active.render_generation,
                            overlay_generation: active.overlay_generation,
                            fallback_clear_frame: state.clear_frame,
                        })
                    }
                    DisplayPanesOutcome::Select(label) => {
                        let state = active
                            .display_panes
                            .take()
                            .expect("display-panes state exists");
                        active.overlay_generation = active.overlay_generation.saturating_add(1);
                        Some(DisplayPanesAction::Execute {
                            attach_pid,
                            origin: state.origin,
                            command_context: Box::new(state.command_context),
                            control_tx: active.control_tx.clone(),
                            render_generation: active.render_generation,
                            overlay_generation: active.overlay_generation,
                            fallback_clear_frame: state.clear_frame,
                            target: label.target,
                            target_string: label.target_string,
                            target_identity: Box::new(label.target_identity),
                            template: state.template,
                        })
                    }
                };
                let focus_effects = action
                    .is_some()
                    .then(|| prepare_server_focus_effects(&mut state, &mut active_attach));
                (action, focus_effects)
            };

        if let Some(focus_effects) = focus_effects {
            let _ = focus_effects.execute(self).await;
        }

        match action {
            None => {}
            Some(DisplayPanesAction::Clear {
                attach_pid,
                control_tx,
                render_generation,
                overlay_generation,
                fallback_clear_frame,
            }) => {
                if self.attached_persistent_overlay_active(attach_pid).await {
                    if !self.restore_mode_tree_overlay_if_active(attach_pid).await? {
                        let _ = self.refresh_mode_tree_overlay_if_active(attach_pid).await;
                    }
                } else {
                    let clear_frame = self
                        .render_attached_display_panes_clear_frame(attach_pid)
                        .await
                        .unwrap_or(fallback_clear_frame);
                    let overlay =
                        OverlayFrame::new(clear_frame, render_generation, overlay_generation);
                    let _ = control_tx.send(AttachControl::Overlay(overlay));
                }
                let _ = self.refresh_interactive_overlay_if_active(attach_pid).await;
            }
            Some(DisplayPanesAction::Execute {
                attach_pid,
                origin,
                command_context,
                control_tx,
                render_generation,
                overlay_generation,
                fallback_clear_frame,
                target,
                target_string,
                target_identity,
                template,
            }) => {
                let requester_pid = origin.requester_pid();
                let stable_target = Target::Pane(target.clone());
                let validation = {
                    let state = self.state.lock().await;
                    command_context
                        .require_live_retained_lifecycle_target(&state, "display-panes selection")
                        .and_then(|()| {
                            target_identity.require(
                                &state,
                                &stable_target,
                                "display-panes selection",
                            )
                        })
                };
                let _access = self.require_requester_origin_write(&origin).await?;
                let clear_frame = self
                    .render_attached_display_panes_clear_frame(attach_pid)
                    .await
                    .unwrap_or(fallback_clear_frame);
                let overlay = OverlayFrame::new(clear_frame, render_generation, overlay_generation);
                let _ = control_tx.send(AttachControl::Overlay(overlay));
                validation?;
                if let Some(template) = template {
                    if template == DEFAULT_DISPLAY_PANES_TEMPLATE {
                        let outcome = with_expected_stable_target_identities(
                            vec![*target_identity],
                            None,
                            None,
                            self.dispatch_for_connection(
                                requester_pid,
                                u64::from(requester_pid),
                                Request::SelectPane(Box::new(SelectPaneRequest {
                                    target,
                                    title: None,
                                    style: None,
                                    input_disabled: None,
                                    preserve_zoom: false,
                                })),
                            ),
                        )
                        .await;
                        match outcome.response {
                            Response::SelectPane(_) => return Ok(()),
                            Response::Error(ErrorResponse { error }) => return Err(error),
                            _ => {
                                return Err(RmuxError::Server(
                                    "display-panes select-pane returned unexpected response"
                                        .to_owned(),
                                ));
                            }
                        }
                    }
                    let substituted = substitute_prompt_template(&template, &[target_string]);
                    let parser = {
                        let state = self.state.lock().await;
                        command_parser_from_state(&state)
                    };
                    let parsed = parser.parse_one_group(&substituted).map_err(|error| {
                        RmuxError::Server(format!(
                            "display-panes command parse failed: {}",
                            error.message()
                        ))
                    })?;
                    let context = (*command_context)
                        .with_current_target(Some(stable_target))
                        .with_pinned_current_target_identity(Some(*target_identity));
                    let _ = self
                        .execute_parsed_commands(requester_pid, parsed, context)
                        .await?;
                }
            }
        }

        Ok(())
    }
}

enum DisplayPanesAction {
    Clear {
        attach_pid: u32,
        control_tx: AttachControlSender,
        render_generation: u64,
        overlay_generation: u64,
        fallback_clear_frame: Vec<u8>,
    },
    Execute {
        attach_pid: u32,
        origin: RequesterOrigin,
        command_context: Box<QueueExecutionContext>,
        control_tx: AttachControlSender,
        render_generation: u64,
        overlay_generation: u64,
        fallback_clear_frame: Vec<u8>,
        target: PaneTarget,
        target_string: String,
        target_identity: Box<StableTargetIdentity>,
        template: Option<String>,
    },
}

enum DisplayPanesPrefixInput {
    Prefix,
    Partial,
    Other,
}
