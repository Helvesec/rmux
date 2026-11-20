use super::*;

impl std::fmt::Debug for PaneOutputReceiver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaneOutputReceiver")
            .field("cursor", &self.cursor)
            .field(
                "passthrough_floor_sequence",
                &self.passthrough_floor_sequence,
            )
            .field("fast_path", &self.fast_rx.is_some())
            .finish_non_exhaustive()
    }
}

impl PaneOutputReceiver {
    pub(in crate::pane_io) fn shares_pane_source_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// The authoritative generation of the shared sender this receiver is
    /// subscribed to — the same value `PaneOutputSender::current_generation`
    /// reads and `reset_generation` stores at a respawn boundary.
    ///
    /// Two receivers of one pane report the same generation, which is why this
    /// and not `shares_pane_source_with` is what distinguishes a dead
    /// process's deferred work from its replacement's.
    pub(in crate::pane_io) fn current_generation(&self) -> u64 {
        self.inner.generation.load(Ordering::SeqCst)
    }

    pub(crate) const fn observed_invalidation_revision(&self) -> u64 {
        self.observed_invalidation_revision
    }

    pub(crate) const fn observed_process_exit_revision(&self) -> u64 {
        self.observed_process_exit_revision
    }

    /// Carries lifecycle delivery progress across an atomic rebase.
    ///
    /// The replacement receiver starts at the captured output boundary, but a
    /// child can exit between the old receiver observing an invalidation and
    /// that capture. Queuing only the missing lifecycle revisions keeps those
    /// exits observable without replaying bytes already represented by the
    /// rebase.
    pub(crate) fn preserve_process_exits_since(&mut self, observed_revision: u64) {
        self.pending_preserved_process_exits = self.pending_preserved_process_exits.max(
            self.observed_process_exit_revision
                .saturating_sub(observed_revision),
        );
    }
}

impl PaneOutputReceiver {
    #[cfg(any(feature = "web", test))]
    pub(crate) async fn recv_observed(&mut self) -> PaneObservationItem {
        loop {
            let inner = Arc::clone(&self.inner);
            let notified = inner.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(item) = self.try_recv_observed() {
                return item;
            }
            notified.await;
        }
    }

    pub(crate) fn try_recv_observed(&mut self) -> Option<PaneObservationItem> {
        let state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        if self.pending_preserved_process_exits > 0 {
            self.pending_preserved_process_exits -= 1;
            return Some(PaneObservationItem::ProcessExited {
                output_sequence: None,
            });
        }
        if state.invalidation_revision != self.observed_invalidation_revision {
            if self.observed_process_exit_revision < state.process_exit_revision_at_invalidation {
                self.observed_process_exit_revision =
                    self.observed_process_exit_revision.saturating_add(1);
                return Some(PaneObservationItem::ProcessExited {
                    output_sequence: None,
                });
            }
            self.observed_invalidation_revision = state.invalidation_revision;
            self.cursor = state.cursor_from_now();
            self.passthrough_floor_sequence = self.cursor.next_sequence();
            self.pending_observed_output = None;
            return Some(PaneObservationItem::Invalidated(PaneInvalidation {
                boundary: state.boundary(self.inner.generation.load(Ordering::SeqCst)),
                reason: state.last_invalidation_reason,
            }));
        }

        if self.pending_observed_output.is_some()
            && self.observed_process_exit_revision < state.process_exit_revision
        {
            self.observed_process_exit_revision =
                self.observed_process_exit_revision.saturating_add(1);
            return Some(PaneObservationItem::ProcessExited {
                output_sequence: None,
            });
        }
        if let Some(item) = self.pending_observed_output.take() {
            return Some(PaneObservationItem::Output(item));
        }

        loop {
            let item = state.poll_cursor(&mut self.cursor, self.passthrough_floor_sequence)?;
            match &item {
                OutputCursorItem::Gap(_)
                    if self.observed_process_exit_revision < state.process_exit_revision =>
                {
                    // A ring gap may have evicted the zero-byte EOF marker.
                    // Preserve the already-produced gap for the next poll,
                    // and surface lifecycle first so a following rebase does
                    // not silently erase the child exit.
                    self.pending_observed_output = Some(item);
                    self.observed_process_exit_revision =
                        self.observed_process_exit_revision.saturating_add(1);
                    return Some(PaneObservationItem::ProcessExited {
                        output_sequence: None,
                    });
                }
                OutputCursorItem::Event(event) if event.bytes().is_empty() => {
                    // EOF markers are represented as typed lifecycle for
                    // recoverable observers. If a preceding gap already
                    // accounted for this revision, consume the retained
                    // marker without emitting the lifecycle twice.
                    if self.observed_process_exit_revision < state.process_exit_revision {
                        self.observed_process_exit_revision =
                            self.observed_process_exit_revision.saturating_add(1);
                        return Some(PaneObservationItem::ProcessExited {
                            output_sequence: Some(event.sequence()),
                        });
                    }
                }
                _ => return Some(PaneObservationItem::Output(item)),
            }
        }
    }

    pub(crate) async fn recv(&mut self) -> OutputCursorItem {
        self.recv_drained().await.item
    }

    pub(in crate::pane_io) async fn recv_drained(&mut self) -> DrainedPaneOutput {
        loop {
            if let Some(item) = self.try_recv_fast_drained() {
                return item;
            }
            let inner = Arc::clone(&self.inner);
            let notified = inner.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(item) = self.try_recv_fast_drained() {
                return item;
            }
            if let Some(item) = self.try_recv_drained() {
                return item;
            }
            if self.fast_rx.is_some() {
                let fast = {
                    let fast_rx = self.fast_rx.as_mut().expect("fast receiver checked");
                    tokio::select! {
                        fast = fast_rx.recv() => fast,
                        _ = notified => None,
                    }
                };
                if let Some(fast) = fast {
                    if let Some(item) = self.accept_fast_item_drained(fast) {
                        return item;
                    }
                }
            } else {
                notified.await;
            }
        }
    }

    pub(super) fn try_recv_fast(&mut self) -> Option<OutputCursorItem> {
        self.try_recv_fast_drained().map(|drained| drained.item)
    }

    pub(super) fn try_recv_fast_drained(&mut self) -> Option<DrainedPaneOutput> {
        let inner = Arc::clone(&self.inner);
        let _state = inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        let generation = inner.generation.load(Ordering::SeqCst);
        let fast_epoch = inner.fast_epoch.load(Ordering::Acquire);
        self.try_recv_fast_at_epoch(fast_epoch)
            .map(|item| DrainedPaneOutput { generation, item })
    }

    pub(super) fn try_recv_fast_at_epoch(&mut self, fast_epoch: u64) -> Option<OutputCursorItem> {
        loop {
            let fast = match self.fast_rx.as_mut()?.try_recv() {
                Ok(fast) => fast,
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => {
                    self.disable_fast_rx();
                    return None;
                }
            };
            if let Some(item) = self.accept_fast_item_at_epoch(fast, fast_epoch) {
                return Some(item);
            }
        }
    }

    pub(super) fn accept_fast_item_drained(
        &mut self,
        fast: FastPaneOutput,
    ) -> Option<DrainedPaneOutput> {
        let inner = Arc::clone(&self.inner);
        let _state = inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        let generation = inner.generation.load(Ordering::SeqCst);
        let fast_epoch = inner.fast_epoch.load(Ordering::Acquire);
        self.accept_fast_item_at_epoch(fast, fast_epoch)
            .map(|item| DrainedPaneOutput { generation, item })
    }

    pub(super) fn accept_fast_item_at_epoch(
        &mut self,
        fast: FastPaneOutput,
        fast_epoch: u64,
    ) -> Option<OutputCursorItem> {
        if fast.epoch != fast_epoch {
            return None;
        }
        if !self.cursor.advance_past_sequence(fast.sequence) {
            return None;
        }
        Some(OutputCursorItem::Event(OutputEvent::from_shared(
            fast.sequence,
            fast.bytes,
            Vec::new(),
        )))
    }

    pub(super) fn disable_fast_rx(&mut self) {
        if self.fast_rx.take().is_some() {
            self.inner
                .fast_receiver_count
                .fetch_sub(1, Ordering::Relaxed);
            self.inner
                .fast_receivers
                .lock()
                .expect("pane output fast receiver list must not be poisoned")
                .retain(|receiver| !receiver.is_closed());
        }
    }

    pub(crate) fn try_recv(&mut self) -> Option<OutputCursorItem> {
        self.try_recv_drained().map(|drained| drained.item)
    }

    pub(in crate::pane_io) fn try_recv_drained(&mut self) -> Option<DrainedPaneOutput> {
        let inner = Arc::clone(&self.inner);
        let state = inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        let generation = inner.generation.load(Ordering::SeqCst);
        state
            .poll_cursor(&mut self.cursor, self.passthrough_floor_sequence)
            .map(|item| DrainedPaneOutput { generation, item })
    }

    /// Drains only while the receiver still observes `generation`.
    ///
    /// The generation check and every cursor advance share the state mutex. A
    /// concurrent reset therefore either lands before this call (which drains
    /// nothing) or after the whole returned slice; a caller cannot assign one
    /// birth generation to work from both sides of the boundary.
    pub(in crate::pane_io) fn try_recv_batch_for_generation(
        &mut self,
        generation: u64,
        limit: usize,
    ) -> Vec<OutputCursorItem> {
        let inner = Arc::clone(&self.inner);
        let state = inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        if inner.generation.load(Ordering::SeqCst) != generation {
            return Vec::new();
        }

        let fast_epoch = inner.fast_epoch.load(Ordering::Acquire);
        let mut items = Vec::new();
        for _ in 0..limit {
            let Some(item) = self.try_recv_fast_at_epoch(fast_epoch) else {
                break;
            };
            items.push(item);
        }
        let remaining = limit.saturating_sub(items.len());
        if remaining > 0 {
            items.extend(state.poll_cursor_batch(
                &mut self.cursor,
                self.passthrough_floor_sequence,
                remaining,
            ));
        }
        items
    }

    pub(in crate::pane_io) fn try_recv_for_generation(
        &mut self,
        generation: u64,
    ) -> Option<DrainedPaneOutput> {
        self.try_recv_batch_for_generation(generation, 1)
            .pop()
            .map(|item| DrainedPaneOutput { generation, item })
    }

    pub(crate) fn try_recv_batch(&mut self, limit: usize) -> Vec<OutputCursorItem> {
        let mut items = Vec::new();
        for _ in 0..limit {
            let Some(item) = self.try_recv_fast() else {
                break;
            };
            items.push(item);
        }

        let remaining = limit.saturating_sub(items.len());
        if remaining == 0 {
            return items;
        }
        let mut retained = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned")
            .poll_cursor_batch(&mut self.cursor, self.passthrough_floor_sequence, remaining);
        items.append(&mut retained);
        items
    }

    pub(crate) const fn cursor(&self) -> &OutputCursor {
        &self.cursor
    }
}

impl Drop for PaneOutputReceiver {
    fn drop(&mut self) {
        self.disable_fast_rx();
        self.inner.receiver_count.fetch_sub(1, Ordering::Relaxed);
        #[cfg(unix)]
        if self.control_cursor.take().is_some() {
            self.inner.control_progress.notify_waiters();
        }
    }
}
