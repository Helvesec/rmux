use super::*;

impl PaneOutputSender {
    /// Unconditionally builds a publication under its generation guard.
    pub(crate) fn publish_for_generation_with_invalidation<R>(
        &self,
        generation: Option<u64>,
        bytes: Vec<u8>,
        build_side_effects: impl FnOnce(
            &[u8],
        )
            -> (R, Vec<TerminalPassthrough>, Option<PaneInvalidationReason>),
    ) -> Option<(u64, R)> {
        self.try_publish_for_generation_with_invalidation(generation, bytes, |bytes| {
            Some(build_side_effects(bytes))
        })
        .unwrap_or_else(|_| unreachable!("unconditional publication cannot request admission"))
    }

    /// The builder may refuse BEFORE effects when it needs an external permit.
    /// Refusal returns the unchanged bytes without advancing the ring, fast
    /// epoch, invalidation, or observer notifications. The caller can then wait
    /// outside all publication locks and retry the same feed.
    pub(crate) fn try_publish_for_generation_with_invalidation<R>(
        &self,
        generation: Option<u64>,
        bytes: Vec<u8>,
        build_side_effects: impl FnOnce(
            &[u8],
        ) -> Option<(
            R,
            Vec<TerminalPassthrough>,
            Option<PaneInvalidationReason>,
        )>,
    ) -> Result<Option<(u64, R)>, Vec<u8>> {
        let fast_receiver_count = self.inner.fast_receiver_count.load(Ordering::Acquire);
        let (sequence, result, fast_bytes, fast_epoch) = {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("pane output state mutex must not be poisoned");
            if !generation_matches(self.current_generation(), generation) {
                return Ok(None);
            }
            let Some((result, passthroughs, invalidation)) = build_side_effects(&bytes) else {
                return Err(bytes);
            };
            let retain_passthroughs = self.inner.receiver_count.load(Ordering::Acquire) > 0;
            let bytes: Arc<[u8]> = bytes.into();
            let fast_bytes = invalidation
                .is_none()
                .then(|| fast_output_candidate(fast_receiver_count, &bytes, &passthroughs))
                .flatten();
            let sequence = state.push(bytes, passthroughs, true, retain_passthroughs);
            if let Some(reason) = invalidation {
                state.invalidate(reason);
                self.inner.fast_epoch.fetch_add(1, Ordering::AcqRel);
            }
            let fast_epoch = self.inner.fast_epoch.load(Ordering::Acquire);
            (sequence, result, fast_bytes, fast_epoch)
        };
        #[cfg(test)]
        self.pause_before_fast_send();
        let fast_delivered = fast_bytes
            .map(|bytes| self.try_send_fast_output(fast_epoch, sequence, bytes))
            .unwrap_or(false);
        self.notify_receivers_after_fast(fast_delivered);
        Ok(Some((sequence, result)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_publication_preserves_bytes_and_all_observer_boundaries() {
        let output = super::super::super::pane_output_channel();
        output.set_generation(7);
        let (before, (), mut observer) = output.capture_with_observer(|| ());
        let bytes = b"unparsed\x1bkname\x1b\\".to_vec();
        let refused = output
            .try_publish_for_generation_with_invalidation::<()>(Some(7), bytes.clone(), |_| None)
            .unwrap_err();
        assert_eq!(refused, bytes);
        assert_eq!(output.capture_with_observer(|| ()).0, before);
        assert!(observer.try_recv_observed().is_none());
        let published = output
            .try_publish_for_generation_with_invalidation(Some(7), refused, |_| {
                Some((42, Vec::new(), None))
            })
            .unwrap()
            .unwrap();
        assert_eq!(published, (before.next_output_sequence, 42));
        assert!(
            matches!(observer.try_recv(), Some(OutputCursorItem::Event(event)) if event.bytes() == bytes)
        );
        assert!(observer.try_recv().is_none());
    }

    #[test]
    fn stale_conditional_publication_never_runs_the_builder() {
        let output = super::super::super::pane_output_channel();
        output.set_generation(7);
        let before = output.capture_with_observer(|| ()).0;
        assert!(output
            .try_publish_for_generation_with_invalidation::<()>(
                Some(6),
                b"stale".to_vec(),
                |_| panic!("stale builder"),
            )
            .unwrap()
            .is_none());
        assert_eq!(output.capture_with_observer(|| ()).0, before);
    }
}
