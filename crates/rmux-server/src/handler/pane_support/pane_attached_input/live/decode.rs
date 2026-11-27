use super::*;

pub(super) struct LiveInputDecode {
    pub(super) target: PaneTarget,
    pub(super) target_in_copy_mode: bool,
    pub(super) target_focus_events: bool,
    pub(super) backspace: Option<u8>,
    pub(super) paste_limit: u32,
    pub(super) projected_shift_enter: bool,
    #[cfg(windows)]
    pub(super) windows_console_key: Option<WindowsConsoleKeyEvent>,
}

impl RequestHandler {
    pub(super) async fn decode_attached_live_input(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
        decode: LiveInputDecode,
    ) -> io::Result<AttachedLiveInputStep> {
        let LiveInputDecode {
            target,
            target_in_copy_mode,
            target_focus_events,
            backspace,
            paste_limit,
            projected_shift_enter,
            #[cfg(windows)]
            windows_console_key,
        } = decode;
        let mut forwarded_to_pane = false;
        let user_keys = self.configured_user_keys().await;
        let new_input_at = pending_input.len();
        pending_input.extend_from_slice(bytes);
        let mut raw_start = 0;
        let mut offset = 0;
        let mut plain_key_dispatch = None;

        while offset < pending_input.len() {
            let slice = &pending_input[offset..];
            let mut paste_checked = false;
            let slice_new_input_at = new_input_at.saturating_sub(offset);
            match decode_bracketed_paste_after_append(slice, slice_new_input_at) {
                BracketedPasteDecode::Matched {
                    size,
                    body_start,
                    body_end,
                } => {
                    if target_in_copy_mode {
                        offset += size;
                        raw_start = offset;
                        continue;
                    }
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                    }
                    self.write_attached_bracketed_paste_for_identity(
                        identity,
                        &pending_input[offset + body_start..offset + body_end],
                    )
                    .await?;
                    forwarded_to_pane = true;
                    offset += size;
                    raw_start = offset;
                    continue;
                }
                BracketedPasteDecode::Partial => {
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                        forwarded_to_pane = true;
                    }
                    pending_input.drain(..offset);
                    if let Some(segment) = take_incomplete_bracketed_paste_segment(
                        pending_input,
                        MAX_RETAINED_ATTACHED_CONTROL_INPUT,
                    ) {
                        if !target_in_copy_mode {
                            self.write_attached_bracketed_paste_for_identity(identity, &segment)
                                .await?;
                            forwarded_to_pane = true;
                        }
                    }
                    retain_partial_attached_control_input("live bracketed paste", pending_input)?;
                    return Ok(AttachedLiveInputStep::Complete(forwarded_to_pane));
                }
                BracketedPasteDecode::NotPaste => {}
            }
            match decode_kitty_graphics_apc_after_append(slice, slice_new_input_at) {
                KittyGraphicsApcDecode::Matched { size } => {
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                    }
                    self.write_attached_target_bytes_for_identity(
                        identity,
                        &pending_input[offset..offset + size],
                    )
                    .await?;
                    forwarded_to_pane = true;
                    offset += size;
                    raw_start = offset;
                    continue;
                }
                KittyGraphicsApcDecode::Partial => {
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                        forwarded_to_pane = true;
                    }
                    pending_input.drain(..offset);
                    retain_partial_attached_control_input(
                        "live kitty graphics APC",
                        pending_input,
                    )?;
                    return Ok(AttachedLiveInputStep::Complete(forwarded_to_pane));
                }
                KittyGraphicsApcDecode::NotKittyGraphics => {}
            }
            if let Some(event) = decode_focus_event(slice) {
                if raw_start < offset {
                    self.write_attached_bytes_for_identity(
                        identity,
                        &pending_input[raw_start..offset],
                    )
                    .await?;
                    forwarded_to_pane = true;
                }
                forwarded_to_pane |= self
                    .handle_attached_terminal_control_event(identity, &target, event)
                    .await?;
                offset += 3;
                raw_start = offset;
                if let Some(remaining) = take_attached_remaining_input(pending_input, raw_start) {
                    return Ok(AttachedLiveInputStep::Reroute {
                        bytes: remaining,
                        forwarded: forwarded_to_pane,
                    });
                }
                continue;
            }
            match decode_attached_terminal_control_after_append(
                slice,
                target_focus_events,
                slice_new_input_at,
            ) {
                TerminalResponseDecode::Matched { size, event } => {
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                        forwarded_to_pane = true;
                    }
                    if let Some(event) = event {
                        forwarded_to_pane |= self
                            .handle_attached_terminal_control_event(identity, &target, event)
                            .await?;
                    }
                    offset += size;
                    raw_start = offset;
                    if event.is_some() {
                        if let Some(remaining) =
                            take_attached_remaining_input(pending_input, raw_start)
                        {
                            return Ok(AttachedLiveInputStep::Reroute {
                                bytes: remaining,
                                forwarded: forwarded_to_pane,
                            });
                        }
                    }
                    continue;
                }
                TerminalResponseDecode::PaneBound { size } => {
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                    }
                    self.write_attached_target_bytes_for_identity(
                        identity,
                        &pending_input[offset..offset + size],
                    )
                    .await?;
                    forwarded_to_pane = true;
                    offset += size;
                    raw_start = offset;
                    continue;
                }
                TerminalResponseDecode::PaletteResponse { size, index } => {
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                        forwarded_to_pane = true;
                    }
                    forwarded_to_pane |= self
                        .write_attached_palette_response_for_identity(
                            identity,
                            index,
                            &pending_input[offset..offset + size],
                        )
                        .await?;
                    offset += size;
                    raw_start = offset;
                    continue;
                }
                TerminalResponseDecode::ClipboardResponse {
                    size,
                    selection,
                    content,
                } => {
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                        forwarded_to_pane = true;
                    }
                    forwarded_to_pane |= self
                        .handle_attached_clipboard_response(identity, selection, content)
                        .await?;
                    offset += size;
                    raw_start = offset;
                    continue;
                }
                TerminalResponseDecode::Partial => {
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                        forwarded_to_pane = true;
                    }
                    pending_input.drain(..offset);
                    retain_partial_attached_control_input("live terminal response", pending_input)?;
                    return Ok(AttachedLiveInputStep::Complete(forwarded_to_pane));
                }
                TerminalResponseDecode::NotResponse => {}
            }
            match decode_pane_bound_terminal_string(slice) {
                PaneBoundTerminalStringDecode::Matched { size } => {
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                    }
                    self.write_attached_target_bytes_for_identity(
                        identity,
                        &pending_input[offset..offset + size],
                    )
                    .await?;
                    forwarded_to_pane = true;
                    offset += size;
                    raw_start = offset;
                    continue;
                }
                PaneBoundTerminalStringDecode::Partial => {
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                        forwarded_to_pane = true;
                    }
                    pending_input.drain(..offset);
                    retain_partial_attached_control_input(
                        "live pane-bound terminal string",
                        pending_input,
                    )?;
                    return Ok(AttachedLiveInputStep::Complete(forwarded_to_pane));
                }
                PaneBoundTerminalStringDecode::NotString => {}
            }
            if is_mouse_prefix(slice) {
                if raw_start < offset {
                    self.write_attached_bytes_for_identity(
                        identity,
                        &pending_input[raw_start..offset],
                    )
                    .await?;
                    forwarded_to_pane = true;
                }
                let last_mouse = self.attached_last_mouse_event_for_identity(identity).await;
                match decode_mouse(slice, last_mouse) {
                    MouseDecode::Matched { size, event } => {
                        self.handle_attached_live_mouse(identity, event).await?;
                        offset += size;
                        raw_start = offset;
                        if let Some(remaining) =
                            take_attached_remaining_input(pending_input, raw_start)
                        {
                            return Ok(AttachedLiveInputStep::Reroute {
                                bytes: remaining,
                                forwarded: forwarded_to_pane,
                            });
                        }
                    }
                    MouseDecode::Discard { size } => {
                        offset += size;
                        raw_start = offset;
                    }
                    MouseDecode::Partial | MouseDecode::Overlong => {
                        pending_input.drain(..offset);
                        retain_partial_attached_escape_input("live mouse", pending_input)?;
                        return Ok(AttachedLiveInputStep::Complete(forwarded_to_pane));
                    }
                    MouseDecode::Invalid => {
                        raw_start = offset;
                        offset += 1;
                    }
                }
                continue;
            }
            let user_key = super::super::user_key_input::decode_user_key(slice, &user_keys);
            if user_key.is_none() && is_extended_key_prefix(slice) {
                if raw_start < offset {
                    self.write_attached_bytes_for_identity(
                        identity,
                        &pending_input[raw_start..offset],
                    )
                    .await?;
                    forwarded_to_pane = true;
                }
                match decode_extended_key(slice, backspace) {
                    ExtendedKeyDecode::Matched { size, key } => {
                        if self.assume_paste_key(identity, paste_limit).await {
                            if !target_in_copy_mode {
                                self.write_attached_bytes_for_identity(
                                    identity,
                                    &pending_input[offset..offset + size],
                                )
                                .await?;
                                forwarded_to_pane = true;
                            }
                            offset += size;
                            raw_start = offset;
                            continue;
                        }
                        if raw_start < offset && is_enter_key(key) {
                            self.record_attached_submitted_text(
                                identity,
                                &pending_input[raw_start..offset],
                            )
                            .await?;
                        }
                        #[cfg(windows)]
                        let handled = if let Some(key_event) = windows_console_key
                            .filter(|_| {
                                raw_start == offset
                                    && offset == 0
                                    && size == pending_input.len()
                                    && size == bytes.len()
                            })
                            .or_else(|| windows_synthetic_console_key_for_decoded_key(key))
                        {
                            let key = windows_console_binding_key(key, key_event);
                            self.handle_attached_live_key_inner(
                                identity,
                                key,
                                super::super::AttachedPaneForward::WindowsConsoleKey {
                                    key: key_event,
                                    bytes: &pending_input[offset..offset + size],
                                },
                            )
                            .await?
                        } else {
                            self.handle_attached_live_key(identity, key).await?
                        };
                        #[cfg(not(windows))]
                        let handled = self.handle_attached_live_key(identity, key).await?;
                        if !handled {
                            forwarded_to_pane = true;
                        }
                        offset += size;
                        raw_start = offset;
                        if handled {
                            if let Some(remaining) =
                                take_attached_remaining_input(pending_input, raw_start)
                            {
                                return Ok(AttachedLiveInputStep::Reroute {
                                    bytes: remaining,
                                    forwarded: forwarded_to_pane,
                                });
                            }
                        }
                        if input_arrives_now(identity)
                            && self.prompt_active_for_identity(identity).await
                        {
                            break;
                        }
                        continue;
                    }
                    ExtendedKeyDecode::Partial => {
                        pending_input.drain(..offset);
                        retain_partial_attached_escape_input("live extended key", pending_input)?;
                        return Ok(AttachedLiveInputStep::Complete(forwarded_to_pane));
                    }
                    ExtendedKeyDecode::Invalid => {
                        raw_start = offset;
                    }
                }
            }
            let key_table_active = self.attached_key_table_active(identity).await;
            if user_key.is_none() && !key_table_active && !target_in_copy_mode {
                let first = slice[0];
                if !first.is_ascii_control() {
                    let (key, size) = if first.is_ascii() {
                        (KeyCode::from(first), 1)
                    } else if let Some((character, size)) = decode_utf8_char(slice) {
                        (KeyCode::from(character), size)
                    } else {
                        if is_utf8_lead_byte(first) && slice.len() < utf8_expected_len(first) {
                            if raw_start < offset {
                                self.write_attached_bytes_for_identity(
                                    identity,
                                    &pending_input[raw_start..offset],
                                )
                                .await?;
                                forwarded_to_pane = true;
                            }
                            pending_input.drain(..offset);
                            retain_partial_attached_escape_input("live utf-8", pending_input)?;
                            return Ok(AttachedLiveInputStep::Complete(forwarded_to_pane));
                        }
                        offset += 1;
                        continue;
                    };
                    paste_checked = true;
                    if self.assume_paste_key(identity, paste_limit).await {
                        offset += size;
                        continue;
                    }
                    if plain_key_dispatch.is_none() {
                        let state = self.state.lock().await;
                        plain_key_dispatch = Some(PlainKeyDispatchSnapshot::new(&state, &target));
                    }
                    if !plain_key_dispatch
                        .as_ref()
                        .expect("plain key dispatch snapshot must be initialized")
                        .requires_dispatch(key)
                    {
                        offset += size;
                        continue;
                    }
                }
            }
            match user_key.unwrap_or_else(|| decode_live_attached_key(slice, backspace)) {
                AttachedKeyDecode::Matched { size, key } => {
                    if raw_start < offset && is_enter_key(key) {
                        self.record_attached_submitted_text(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                    }
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                        forwarded_to_pane = true;
                    }
                    if !paste_checked && self.assume_paste_key(identity, paste_limit).await {
                        if !target_in_copy_mode {
                            self.write_attached_bytes_for_identity(
                                identity,
                                &pending_input[offset..offset + size],
                            )
                            .await?;
                            forwarded_to_pane = true;
                        }
                        offset += size;
                        raw_start = offset;
                        continue;
                    }
                    let forward = if projected_shift_enter
                        && key == u64::from(b'\r')
                        && size == pending_input.len()
                    {
                        super::super::AttachedPaneForward::ProjectedShiftEnter
                    } else {
                        super::super::AttachedPaneForward::EncodedKey(std::marker::PhantomData)
                    };
                    #[cfg(windows)]
                    let handled = if let Some(key_event) = windows_console_key
                        .filter(|_| {
                            raw_start == offset
                                && offset == 0
                                && size == pending_input.len()
                                && size == bytes.len()
                        })
                        .or_else(|| windows_synthetic_console_key_for_decoded_key(key))
                    {
                        let key = windows_console_binding_key(key, key_event);
                        self.handle_attached_live_key_inner(
                            identity,
                            key,
                            super::super::AttachedPaneForward::WindowsConsoleKey {
                                key: key_event,
                                bytes: &pending_input[offset..offset + size],
                            },
                        )
                        .await?
                    } else {
                        self.handle_attached_live_key_inner(identity, key, forward)
                            .await?
                    };
                    #[cfg(not(windows))]
                    let handled = self
                        .handle_attached_live_key_inner(identity, key, forward)
                        .await?;
                    if !handled {
                        forwarded_to_pane = true;
                    }
                    offset += size;
                    raw_start = offset;
                    if handled {
                        if let Some(remaining) =
                            take_attached_remaining_input(pending_input, raw_start)
                        {
                            return Ok(AttachedLiveInputStep::Reroute {
                                bytes: remaining,
                                forwarded: forwarded_to_pane,
                            });
                        }
                    }
                    if input_arrives_now(identity)
                        && self.prompt_active_for_identity(identity).await
                    {
                        break;
                    }
                    continue;
                }
                AttachedKeyDecode::Partial => {
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                        forwarded_to_pane = true;
                    }
                    pending_input.drain(..offset);
                    retain_partial_attached_escape_input("live attached key", pending_input)?;
                    return Ok(AttachedLiveInputStep::Complete(forwarded_to_pane));
                }
                AttachedKeyDecode::Invalid => {}
            }
            offset += 1;
        }

        if input_arrives_now(identity)
            && self.prompt_active_for_identity(identity).await
            && raw_start < pending_input.len()
        {
            let remaining = pending_input[raw_start..].to_vec();
            pending_input.clear();
            return Ok(AttachedLiveInputStep::Reroute {
                bytes: remaining,
                forwarded: forwarded_to_pane,
            });
        }

        if raw_start < pending_input.len() {
            self.write_attached_bytes_for_identity(identity, &pending_input[raw_start..])
                .await?;
            forwarded_to_pane = true;
        }
        pending_input.clear();
        Ok(AttachedLiveInputStep::Complete(forwarded_to_pane))
    }
}
