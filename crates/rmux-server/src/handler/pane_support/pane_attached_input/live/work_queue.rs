use super::*;

impl RequestHandler {
    pub(in crate::handler::pane_support::pane_attached_input) async fn handle_attached_keyboard_legacy_input(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
        windows_console_key: Option<rmux_proto::AttachedWindowsConsoleKey>,
        cursor: &mut FrameCursor,
        active_emit_cache: &mut ActiveClientEmitCache,
    ) -> io::Result<bool> {
        if bytes.len() <= ATTACHED_LIVE_REROUTE_CHUNK_BYTES {
            return match self
                .handle_attached_live_input_chunk(
                    identity,
                    pending_input,
                    bytes,
                    windows_console_key,
                    cursor,
                    active_emit_cache,
                )
                .await?
            {
                AttachedLiveChunkResult::Complete(forwarded)
                | AttachedLiveChunkResult::Waits(forwarded) => Ok(forwarded),
                AttachedLiveChunkResult::Rechunk { bytes, forwarded } => {
                    self.handle_attached_live_input_work_queue(
                        identity,
                        pending_input,
                        AttachedLiveInputWork::new(Arc::from(bytes), None),
                        forwarded,
                        cursor,
                        active_emit_cache,
                    )
                    .await
                }
            };
        }

        // Preserve one-shot fast-path semantics for a large printable line,
        // including submitted-line tracking. Inputs that contain a prefix or
        // terminal control fall back to bounded reroute chunks below.
        if windows_console_key.is_none() {
            if let Some(forwarded) = self
                .try_forward_plain_attached_bytes_fast(
                    identity,
                    pending_input,
                    bytes,
                    active_emit_cache,
                )
                .await?
            {
                return Ok(forwarded);
            }
        }

        self.handle_attached_live_input_work_queue(
            identity,
            pending_input,
            AttachedLiveInputWork::new(Arc::from(bytes), windows_console_key),
            false,
            cursor,
            active_emit_cache,
        )
        .await
    }

    pub(super) async fn handle_attached_live_input_work_queue(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        initial_work: AttachedLiveInputWork,
        mut forwarded: bool,
        cursor: &mut FrameCursor,
        active_emit_cache: &mut ActiveClientEmitCache,
    ) -> io::Result<bool> {
        let mut work_queue = VecDeque::from([initial_work]);
        let mut first = true;
        while let Some(mut work) = work_queue.pop_front() {
            if !std::mem::replace(&mut first, false) {
                // A later piece of the frame, after a piece that may have changed the client's
                // state, waits like the rest of the frame.
                let rest = || {
                    std::iter::once(&work)
                        .chain(work_queue.iter())
                        .map(|work| FramePart::Bytes(work.as_bytes().to_vec()))
                        .collect()
                };
                if let Some(step_forwarded) = self
                    .defer_rest_of_frame(identity, cursor, pending_input, rest, active_emit_cache)
                    .await?
                {
                    return Ok(forwarded | step_forwarded);
                }
            }
            if work.len() > ATTACHED_LIVE_REROUTE_CHUNK_BYTES
                && self
                    .attached_live_input_can_use_reroute_chunks(identity)
                    .await?
            {
                let remaining = work.split_at(ATTACHED_LIVE_REROUTE_CHUNK_BYTES);
                work_queue.push_front(remaining);
            }

            let windows_console_key = work.windows_console_key.take();
            match self
                .handle_attached_live_input_chunk(
                    identity,
                    pending_input,
                    work.as_bytes(),
                    windows_console_key,
                    cursor,
                    active_emit_cache,
                )
                .await?
            {
                AttachedLiveChunkResult::Complete(step_forwarded)
                | AttachedLiveChunkResult::Waits(step_forwarded) => {
                    forwarded |= step_forwarded;
                }
                AttachedLiveChunkResult::Rechunk {
                    bytes,
                    forwarded: step_forwarded,
                } => {
                    forwarded |= step_forwarded;
                    work_queue.push_front(AttachedLiveInputWork::new(Arc::from(bytes), None));
                }
            }
        }
        Ok(forwarded)
    }

    pub(super) async fn handle_attached_live_input_chunk(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
        mut windows_console_key: Option<rmux_proto::AttachedWindowsConsoleKey>,
        cursor: &mut FrameCursor,
        active_emit_cache: &mut ActiveClientEmitCache,
    ) -> io::Result<AttachedLiveChunkResult> {
        let mut bytes = Cow::Borrowed(bytes);
        let mut forwarded = false;

        loop {
            match self
                .handle_attached_live_input_step(
                    identity,
                    pending_input,
                    bytes.as_ref(),
                    windows_console_key.take(),
                    active_emit_cache,
                )
                .await?
            {
                AttachedLiveInputStep::Complete(step_forwarded) => {
                    return Ok(AttachedLiveChunkResult::Complete(
                        forwarded | step_forwarded,
                    ));
                }
                AttachedLiveInputStep::Reroute {
                    bytes: remaining,
                    forwarded: step_forwarded,
                } => {
                    forwarded |= step_forwarded;
                    // The key may have closed the surface that took this frame or released a
                    // binding: what it hands back then waits like any ordinary input.
                    let rest = || vec![FramePart::Bytes(remaining.clone())];
                    if let Some(step_forwarded) = self
                        .defer_rest_of_frame(
                            identity,
                            cursor,
                            pending_input,
                            rest,
                            active_emit_cache,
                        )
                        .await?
                    {
                        return Ok(AttachedLiveChunkResult::Waits(forwarded | step_forwarded));
                    }
                    if remaining.len() > ATTACHED_LIVE_REROUTE_CHUNK_BYTES
                        && self
                            .attached_live_input_can_use_reroute_chunks(identity)
                            .await?
                    {
                        return Ok(AttachedLiveChunkResult::Rechunk {
                            bytes: remaining,
                            forwarded,
                        });
                    }
                    bytes = Cow::Owned(remaining);
                }
            }
        }
    }
}
