use super::*;

impl RequestHandler {
    pub(super) async fn defer_behind_released_work(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
        windows_console_key: Option<rmux_proto::AttachedWindowsConsoleKey>,
        active_emit_cache: &mut ActiveClientEmitCache,
    ) -> io::Result<Deferral> {
        match self
            .order_input_behind_released_work(
                identity,
                pending_input,
                bytes,
                windows_console_key,
                InputArrival::Live,
                unfinished_live_tail,
            )
            .await
        {
            OrderedInput::HandleNow(cursor) => Ok(Deferral::HandleNow(cursor)),
            OrderedInput::Waits { focus_reports } => self
                .act_on_focus_reports(identity, focus_reports, active_emit_cache)
                .await
                .map(Deferral::Waited),
        }
    }

    /// Acts now on focus reports taken out of input that waits; tmux applies them as they arrive.
    pub(super) async fn act_on_focus_reports(
        &self,
        identity: ActiveAttachIdentity,
        focus_reports: Vec<u8>,
        active_emit_cache: &mut ActiveClientEmitCache,
    ) -> io::Result<bool> {
        if focus_reports.is_empty() {
            return Ok(false);
        }
        Box::pin(self.handle_attached_live_input_on_input_path(
            identity,
            &mut Vec::new(),
            &focus_reports,
            None,
            &mut FrameCursor::new(true),
            active_emit_cache,
        ))
        .await
    }

    /// Queues the rest of the frame when, after a part of it, its ordinary input has to wait;
    /// returns what acting on its focus reports forwarded, or `None` when the input path handles
    /// the rest now.
    pub(super) async fn defer_rest_of_frame(
        &self,
        identity: ActiveAttachIdentity,
        cursor: &mut FrameCursor,
        pending_input: &mut Vec<u8>,
        rest: impl FnOnce() -> Vec<FramePart>,
        active_emit_cache: &mut ActiveClientEmitCache,
    ) -> io::Result<Option<bool>> {
        if !self.rest_of_frame_may_wait(identity, cursor).await {
            return Ok(None);
        }
        let Some(focus_reports) = self
            .queue_rest_of_frame(
                identity,
                cursor,
                pending_input,
                rest(),
                unfinished_live_tail,
            )
            .await
        else {
            return Ok(None);
        };
        self.act_on_focus_reports(identity, focus_reports, active_emit_cache)
            .await
            .map(Some)
    }

    pub(super) async fn handle_attached_live_input_on_input_path(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
        windows_console_key: Option<rmux_proto::AttachedWindowsConsoleKey>,
        cursor: &mut FrameCursor,
        active_emit_cache: &mut ActiveClientEmitCache,
    ) -> io::Result<bool> {
        if !bytes.is_empty() {
            self.record_attached_input_activity(identity).await?;
        }
        let Some(inputs) = super::super::keyboard::split_input(pending_input, bytes) else {
            return self
                .handle_attached_keyboard_legacy_input(
                    identity,
                    pending_input,
                    bytes,
                    windows_console_key,
                    cursor,
                    active_emit_cache,
                )
                .await;
        };
        let native = windows_console_key.filter(|_| pending_input.is_empty() && inputs.len() == 1);
        pending_input.clear();
        let mut inputs = VecDeque::from(inputs);
        let mut forwarded = false;
        let mut first = true;
        while let Some(input) = inputs.pop_front() {
            if !std::mem::replace(&mut first, false)
                && !matches!(input, super::super::keyboard::Input::Pending(_))
            {
                #[cfg(test)]
                test_barrier::pass(test_barrier::Point::NextPart, identity.attach_pid()).await;
                // A part before may have closed the surface that took the frame or released a
                // binding: the rest of the frame, events included, then waits in order.
                let rest = || {
                    std::iter::once(&input)
                        .chain(inputs.iter())
                        .filter_map(|input| match input {
                            super::super::keyboard::Input::Bytes(bytes) => {
                                Some(FramePart::Bytes(bytes.clone()))
                            }
                            super::super::keyboard::Input::Event(event) => {
                                Some(FramePart::KeyboardEvent(event.clone()))
                            }
                            super::super::keyboard::Input::Pending(_) => None,
                        })
                        .collect()
                };
                if let Some(step_forwarded) = self
                    .defer_rest_of_frame(identity, cursor, pending_input, rest, active_emit_cache)
                    .await?
                {
                    for input in std::iter::once(input).chain(inputs) {
                        if let super::super::keyboard::Input::Pending(bytes) = input {
                            pending_input.extend(bytes);
                        }
                    }
                    return Ok(forwarded | step_forwarded);
                }
            }
            match input {
                super::super::keyboard::Input::Bytes(bytes) => {
                    forwarded |= self
                        .handle_attached_keyboard_legacy_input(
                            identity,
                            pending_input,
                            &bytes,
                            None,
                            cursor,
                            active_emit_cache,
                        )
                        .await?;
                }
                super::super::keyboard::Input::Event(event) => {
                    // An Escape prefix that the legacy decoder retained from this read
                    // (`ESC O` before a key frame) is resolved as at escape-time first:
                    // tmux 3.7b reads the bytes in order, so the event never overtakes them.
                    // A partial UTF-8 character keeps waiting for its last bytes.
                    if pending_input.first() == Some(&b'\x1b') {
                        forwarded |= self
                            .flush_attached_pending_escape_input_on_input_path(
                                identity,
                                pending_input,
                            )
                            .await?;
                    }
                    forwarded |= self
                        .handle_acquired_keyboard_event(identity, event, native, active_emit_cache)
                        .await?;
                }
                super::super::keyboard::Input::Pending(bytes) => pending_input.extend(bytes),
            }
        }
        Ok(forwarded)
    }
}
