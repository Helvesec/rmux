use super::*;

impl std::fmt::Debug for PaneOutputSender {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaneOutputSender")
            .finish_non_exhaustive()
    }
}

impl PaneOutputSender {
    #[cfg(test)]
    pub(crate) fn poison_state_for_test(&self) {
        let inner = Arc::clone(&self.inner);
        let _ = std::panic::catch_unwind(move || {
            let _guard = inner
                .state
                .lock()
                .expect("pane output state mutex is healthy before poison");
            panic!("poison pane output state mutex for timer worker test");
        });
    }

    #[cfg(test)]
    pub(crate) fn receiver_count_for_test(&self) -> usize {
        self.inner.receiver_count.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn send(&self, bytes: Vec<u8>) -> u64 {
        self.push_for_generation(None, bytes, Vec::new())
            .expect("unguarded pane output send should always be accepted")
    }

    #[cfg(any(test, windows))]
    pub(crate) fn send_for_generation(
        &self,
        generation: Option<u64>,
        bytes: Vec<u8>,
    ) -> Option<u64> {
        self.push_for_generation(generation, bytes, Vec::new())
    }

    #[cfg(test)]
    pub(crate) fn send_for_generation_with_passthroughs(
        &self,
        generation: Option<u64>,
        bytes: Vec<u8>,
        passthroughs: Vec<TerminalPassthrough>,
    ) -> Option<u64> {
        self.push_for_generation(generation, bytes, passthroughs)
    }

    #[cfg(test)]
    pub(crate) fn publish_for_generation<R>(
        &self,
        generation: Option<u64>,
        bytes: Vec<u8>,
        build_side_effects: impl FnOnce(&[u8]) -> (R, Vec<TerminalPassthrough>),
    ) -> Option<(u64, R)> {
        self.publish_for_generation_with_invalidation(generation, bytes, |bytes| {
            let (result, passthroughs) = build_side_effects(bytes);
            (result, passthroughs, None)
        })
    }

    pub(crate) fn accepts_generation(&self, generation: Option<u64>) -> bool {
        generation_matches(self.current_generation(), generation)
    }

    pub(crate) fn set_generation(&self, generation: u64) {
        // Keep generation switches ordered with generation-guarded ring
        // pushes, so stale readers cannot pass a check from the old process
        // generation and then publish after a respawn.
        let mut state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        self.inner.generation.store(generation, Ordering::SeqCst);
        self.inner.reader_retired.store(false, Ordering::Release);
        #[cfg(windows)]
        self.inner.reader_draining.store(false, Ordering::Release);
        state.invalidate(PaneInvalidationReason::GenerationChanged);
        self.inner.fast_epoch.fetch_add(1, Ordering::AcqRel);
        drop(state);
        self.notify_receivers();
        self.notify_reader_lifecycle();
    }

    /// Atomically fences the old process generation and drops its retained
    /// output before a respawn starts publishing.
    pub(crate) fn reset_generation(&self, generation: u64) {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        self.inner.generation.store(generation, Ordering::SeqCst);
        self.inner.reader_retired.store(false, Ordering::Release);
        #[cfg(windows)]
        self.inner.reader_draining.store(false, Ordering::Release);
        state.clear_retained();
        state.invalidate(PaneInvalidationReason::GenerationChanged);
        self.inner.fast_epoch.fetch_add(1, Ordering::AcqRel);
        drop(state);
        self.notify_receivers();
        self.notify_reader_lifecycle();
    }

    /// After process-tree exit, drain the finite ConPTY tail through EOF even
    /// when all clients remain OFF. A late watcher must not wake a respawn.
    #[cfg(windows)]
    pub(crate) fn drain_reader_for_generation(&self, generation: Option<u64>) {
        let state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        if !self.reader_generation_active(generation) {
            return;
        }
        self.inner.reader_draining.store(true, Ordering::Release);
        drop(state);
        self.notify_reader_lifecycle();
    }

    pub(crate) fn retire_reader_for_generation(&self, generation: Option<u64>) {
        let state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        if !generation_matches(self.current_generation(), generation) {
            return;
        }
        self.inner.reader_retired.store(true, Ordering::Release);
        drop(state);
        self.notify_reader_lifecycle();
    }

    pub(crate) fn retire_reader(&self) {
        self.inner.reader_retired.store(true, Ordering::Release);
        self.notify_reader_lifecycle();
    }

    pub(crate) fn current_generation(&self) -> u64 {
        self.inner.generation.load(Ordering::SeqCst)
    }

    pub(crate) fn subscribe(&self) -> PaneOutputReceiver {
        let state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        let cursor = state.cursor_from_now();
        let passthrough_floor_sequence = cursor.next_sequence();
        self.receiver(
            cursor,
            passthrough_floor_sequence,
            state.invalidation_revision,
            state.process_exit_revision,
            None,
        )
    }

    pub(crate) fn subscribe_from_oldest(&self) -> PaneOutputReceiver {
        let state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        self.receiver(
            state.cursor_from_oldest(),
            state.next_sequence(),
            state.invalidation_revision,
            state.process_exit_revision,
            None,
        )
    }

    pub(crate) fn subscribe_live_from_now(&self) -> (u64, PaneOutputReceiver) {
        let state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        self.subscribe_live_under(&state)
    }

    /// Client renders subscribe to the active pane each client shows, for
    /// requests that never named it. A poisoned output state has no boundary
    /// to subscribe at: `None` lets the render report the pane unavailable
    /// instead of aborting the request that refreshed the client.
    pub(crate) fn subscribe_live_from_now_in_sweep(&self) -> Option<(u64, PaneOutputReceiver)> {
        let state = self.inner.state.lock().ok()?;
        Some(self.subscribe_live_under(&state))
    }

    fn subscribe_live_under(&self, state: &PaneOutputState) -> (u64, PaneOutputReceiver) {
        let sequence = state.next_sequence();
        let receiver = self.register_live_receiver(
            sequence,
            state.invalidation_revision,
            state.process_exit_revision,
        );
        (sequence, receiver)
    }

    #[cfg(test)]
    pub(crate) fn subscribe_live_from_sequence(&self, sequence: u64) -> PaneOutputReceiver {
        let state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        self.register_live_receiver(
            sequence,
            state.invalidation_revision,
            state.process_exit_revision,
        )
    }

    pub(super) fn register_live_receiver(
        &self,
        sequence: u64,
        observed_invalidation_revision: u64,
        observed_process_exit_revision: u64,
    ) -> PaneOutputReceiver {
        let (fast_tx, fast_rx) = mpsc::channel(FAST_PANE_OUTPUT_CHANNEL_CAPACITY);
        self.inner
            .fast_receivers
            .lock()
            .expect("pane output fast receiver list must not be poisoned")
            .push(fast_tx);
        self.inner
            .fast_receiver_count
            .fetch_add(1, Ordering::Relaxed);
        self.receiver(
            OutputRing::cursor_from_sequence(sequence),
            sequence,
            observed_invalidation_revision,
            observed_process_exit_revision,
            Some(fast_rx),
        )
    }

    /// Captures owned pane state and registers a recoverable receiver at the
    /// exact same output/invalidation boundary.
    ///
    /// The closure should only copy the state needed by its consumer.
    /// Expensive ANSI or DTO serialization belongs after this method returns.
    pub(crate) fn capture_with_observer<T>(
        &self,
        capture: impl FnOnce() -> T,
    ) -> (PaneBoundary, T, PaneOutputReceiver) {
        let state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        let captured = capture();
        let boundary = state.boundary(self.current_generation());
        let receiver = self.receiver(
            OutputRing::cursor_from_sequence(boundary.next_output_sequence),
            boundary.next_output_sequence,
            boundary.invalidation_revision,
            boundary.process_exit_revision,
            None,
        );
        (boundary, captured, receiver)
    }

    /// Registers a receiver at a cached boundary if it is still current.
    ///
    /// The equality check and receiver registration share the output-state
    /// lock, so a cached keyframe can be reused without reopening the
    /// snapshot/subscribe race.
    pub(crate) fn subscribe_at_boundary(
        &self,
        expected: PaneBoundary,
    ) -> Option<PaneOutputReceiver> {
        let state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        let current = state.boundary(self.current_generation());
        if current != expected {
            return None;
        }
        Some(self.receiver(
            OutputRing::cursor_from_sequence(current.next_output_sequence),
            current.next_output_sequence,
            current.invalidation_revision,
            current.process_exit_revision,
            None,
        ))
    }

    /// Reports whether a previously captured boundary is still current.
    ///
    /// Admission paths use this as their linearization point after expensive
    /// projection serialization. Unlike `subscribe_at_boundary`, this does
    /// not allocate a receiver that the caller would immediately discard.
    pub(crate) fn is_current_boundary(&self, expected: PaneBoundary) -> bool {
        let state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        state.boundary(self.current_generation()) == expected
    }

    /// Applies a transcript mutation and its invalidation at one linearization
    /// point. Output publication uses the same output-state -> transcript lock
    /// order, so bytes cannot appear between mutation and revision bump.
    pub(crate) fn mutate_transcript<R>(
        &self,
        transcript: &crate::pane_transcript::SharedPaneTranscript,
        reason: PaneInvalidationReason,
        mutation: impl FnOnce(&mut crate::pane_transcript::PaneTranscript) -> (R, bool),
    ) -> R {
        let state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        self.mutate_transcript_under(state, transcript, reason, mutation)
    }

    /// Option sweeps reach panes the request never named. A pane whose output
    /// state is poisoned is skipped instead of failing that request; a
    /// mutation that does run keeps the policy of [`Self::mutate_transcript`].
    pub(crate) fn mutate_transcript_in_sweep<R>(
        &self,
        transcript: &crate::pane_transcript::SharedPaneTranscript,
        reason: PaneInvalidationReason,
        mutation: impl FnOnce(&mut crate::pane_transcript::PaneTranscript) -> (R, bool),
    ) -> Option<R> {
        let state = self.inner.state.lock().ok()?;
        Some(self.mutate_transcript_under(state, transcript, reason, mutation))
    }

    fn mutate_transcript_under<R>(
        &self,
        mut state: std::sync::MutexGuard<'_, PaneOutputState>,
        transcript: &crate::pane_transcript::SharedPaneTranscript,
        reason: PaneInvalidationReason,
        mutation: impl FnOnce(&mut crate::pane_transcript::PaneTranscript) -> (R, bool),
    ) -> R {
        let (result, changed) = {
            let mut transcript = transcript.lock_for_output_mutation();
            let (result, changed) = mutation(&mut transcript);
            if changed {
                state.invalidate(reason);
                self.inner.fast_epoch.fetch_add(1, Ordering::AcqRel);
            }
            (result, changed)
        };
        drop(state);
        if changed {
            self.notify_receivers();
        }
        result
    }

    #[cfg(test)]
    pub(crate) fn clear_retained(&self) {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        state.clear_retained();
        state.invalidate(PaneInvalidationReason::TranscriptMutation);
        self.inner.fast_epoch.fetch_add(1, Ordering::AcqRel);
        drop(state);
        self.notify_receivers();
    }

    #[cfg(any(test, windows))]
    pub(super) fn push_for_generation(
        &self,
        generation: Option<u64>,
        bytes: Vec<u8>,
        passthroughs: Vec<TerminalPassthrough>,
    ) -> Option<u64> {
        let fast_receiver_count = self.inner.fast_receiver_count.load(Ordering::Acquire);
        let (sequence, fast_bytes, fast_epoch) = {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("pane output state mutex must not be poisoned");
            if !generation_matches(self.current_generation(), generation) {
                return None;
            }
            let retain_passthroughs = self.inner.receiver_count.load(Ordering::Acquire) > 0;
            let bytes: Arc<[u8]> = bytes.into();
            let fast_bytes = fast_output_candidate(fast_receiver_count, &bytes, &passthroughs);
            let sequence = state.push(bytes, passthroughs, true, retain_passthroughs);
            let fast_epoch = self.inner.fast_epoch.load(Ordering::Acquire);
            (sequence, fast_bytes, fast_epoch)
        };
        #[cfg(test)]
        self.pause_before_fast_send();
        let fast_delivered = fast_bytes
            .map(|bytes| self.try_send_fast_output(fast_epoch, sequence, bytes))
            .unwrap_or(false);
        self.notify_receivers_after_fast(fast_delivered);
        Some(sequence)
    }

    pub(super) fn receiver(
        &self,
        cursor: OutputCursor,
        passthrough_floor_sequence: u64,
        observed_invalidation_revision: u64,
        observed_process_exit_revision: u64,
        fast_rx: Option<mpsc::Receiver<FastPaneOutput>>,
    ) -> PaneOutputReceiver {
        self.inner.receiver_count.fetch_add(1, Ordering::Relaxed);
        PaneOutputReceiver {
            inner: Arc::clone(&self.inner),
            cursor,
            #[cfg(unix)]
            control_cursor: None,
            passthrough_floor_sequence,
            observed_invalidation_revision,
            observed_process_exit_revision,
            pending_preserved_process_exits: 0,
            pending_observed_output: None,
            fast_rx,
        }
    }

    pub(super) fn notify_receivers(&self) {
        match self.inner.receiver_count.load(Ordering::Acquire) {
            0 => {}
            1 => self.inner.notify.notify_one(),
            _ => self.inner.notify.notify_waiters(),
        }
    }

    pub(super) fn notify_receivers_after_fast(&self, fast_delivered: bool) {
        let receiver_count = self.inner.receiver_count.load(Ordering::Acquire);
        if receiver_count == 0 {
            return;
        }
        if fast_delivered
            && receiver_count == self.inner.fast_receiver_count.load(Ordering::Acquire)
        {
            return;
        }
        match receiver_count {
            1 => self.inner.notify.notify_one(),
            _ => self.inner.notify.notify_waiters(),
        }
    }

    pub(super) fn try_send_fast_output(&self, epoch: u64, sequence: u64, bytes: Arc<[u8]>) -> bool {
        let mut receivers = self
            .inner
            .fast_receivers
            .lock()
            .expect("pane output fast receiver list must not be poisoned");
        if receivers.len() == 1 {
            return match receivers[0].try_send(FastPaneOutput {
                epoch,
                sequence,
                bytes,
            }) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => false,
                Err(TrySendError::Closed(_)) => {
                    receivers.clear();
                    false
                }
            };
        }

        let mut delivered = 0_usize;
        let mut missed = false;
        receivers.retain(|receiver| {
            match receiver.try_send(FastPaneOutput {
                epoch,
                sequence,
                bytes: Arc::clone(&bytes),
            }) {
                Ok(()) => {
                    delivered = delivered.saturating_add(1);
                    true
                }
                Err(TrySendError::Full(_)) => {
                    missed = true;
                    true
                }
                Err(TrySendError::Closed(_)) => false,
            }
        });
        delivered > 0 && !missed
    }

    #[cfg(test)]
    pub(super) fn install_fast_send_pause(&self) -> Arc<FastSendPause> {
        let pause = Arc::new(FastSendPause {
            reached: std::sync::Barrier::new(2),
            release: std::sync::Barrier::new(2),
        });
        *self
            .inner
            .fast_send_pause
            .lock()
            .expect("fast send pause mutex must not be poisoned") = Some(Arc::clone(&pause));
        pause
    }

    #[cfg(test)]
    pub(super) fn pause_before_fast_send(&self) {
        let pause = self
            .inner
            .fast_send_pause
            .lock()
            .expect("fast send pause mutex must not be poisoned")
            .take();
        if let Some(pause) = pause {
            pause.reached.wait();
            pause.release.wait();
        }
    }
}
