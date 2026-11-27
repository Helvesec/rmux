use super::*;

impl PaneOutputReceiver {
    #[cfg(unix)]
    pub(crate) fn protect_control_cursor(&mut self) {
        let cursor = Arc::new(AtomicU64::new(self.cursor.next_sequence()));
        let mut cursors = self
            .inner
            .control_cursors
            .lock()
            .expect("control output cursors");
        cursors.retain(|cursor| cursor.strong_count() != 0);
        cursors.push(Arc::downgrade(&cursor));
        self.control_cursor = Some(cursor);
    }

    pub(crate) fn control_next_sequence(&self) -> u64 {
        self.cursor.next_sequence()
    }

    // Obtain the copy permit before advancing the protected source cursor.
    pub(crate) fn try_recv_admitted<T>(
        &mut self,
        admit: impl FnOnce(usize) -> Option<T>,
    ) -> Option<(OutputCursorItem, T)> {
        let state = self.inner.state.lock().expect("pane output state");
        let bytes = state.ring.next_cursor_byte_len(&self.cursor)?;
        let admission = admit(bytes)?;
        let item = state.ring.poll_cursor_without_recovery(&mut self.cursor)?;
        #[cfg(unix)]
        if let Some(cursor) = &self.control_cursor {
            cursor.store(self.cursor.next_sequence(), Ordering::Release);
            self.inner.control_progress.notify_waiters();
        }
        Some((item, admission))
    }
}

impl PaneOutputSender {
    #[cfg(unix)]
    pub(in crate::pane_io) fn has_ring_receivers(&self) -> bool {
        self.inner.receiver_count.load(Ordering::Acquire)
            > self.inner.fast_receiver_count.load(Ordering::Acquire)
    }

    #[cfg(unix)]
    pub(crate) async fn wait_for_control_admission(&self, generation: Option<u64>) -> bool {
        loop {
            let progress = self.inner.control_progress.notified();
            let lifecycle = self.inner.reader_lifecycle.notified();
            tokio::pin!(progress, lifecycle);
            progress.as_mut().enable();
            lifecycle.as_mut().enable();
            if !self.reader_generation_active(generation) {
                return false;
            }
            // Await every control copy before reading again, so peers cannot evict unread bytes.
            let frontier = {
                let state = self.inner.state.lock().expect("pane output state");
                state.next_sequence()
            };
            let ready = {
                let mut cursors = self
                    .inner
                    .control_cursors
                    .lock()
                    .expect("control output cursors");
                cursors.retain(|cursor| cursor.strong_count() != 0);
                cursors
                    .iter()
                    .filter_map(std::sync::Weak::upgrade)
                    .all(|cursor| cursor.load(Ordering::Acquire) >= frontier)
            };
            if ready {
                return true;
            }
            tokio::select! {
                _ = progress => {}
                _ = lifecycle => {}
            }
        }
    }

    pub(crate) fn control_output_ready(&self) -> tokio::sync::futures::Notified<'_> {
        self.inner.notify.notified()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::future::{poll_fn, Future};
    use std::task::Poll;
    use std::time::Duration;

    async fn pending(mut future: std::pin::Pin<&mut impl Future>) {
        assert!(poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx)))
            .await
            .is_pending());
    }

    #[tokio::test]
    async fn control_admission_waits_for_every_reader_and_releases_on_stop() {
        let source = super::super::super::pane_output_channel();
        let mut fast = source.subscribe();
        let mut slow = source.subscribe();
        fast.protect_control_cursor();
        slow.protect_control_cursor();
        source.send(b"first".to_vec());
        let admission = source.wait_for_control_admission(None);
        tokio::pin!(admission);
        pending(admission.as_mut()).await;
        assert!(slow.try_recv_admitted(|_| None::<()>).is_none());
        let (event, ()) = fast.try_recv_admitted(|_| Some(())).unwrap();
        assert!(matches!(event, OutputCursorItem::Event(event) if event.bytes() == b"first"));
        pending(admission.as_mut()).await;
        let (event, ()) = slow.try_recv_admitted(|_| Some(())).unwrap();
        assert!(matches!(event, OutputCursorItem::Event(event) if event.bytes() == b"first"));
        tokio::time::timeout(Duration::from_secs(1), admission)
            .await
            .unwrap();
        source.send(b"second".to_vec());
        let admission = source.wait_for_control_admission(None);
        tokio::pin!(admission);
        pending(admission.as_mut()).await;
        drop(fast);
        pending(admission.as_mut()).await;
        drop(slow);
        assert!(tokio::time::timeout(Duration::from_secs(1), admission)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn control_admission_cancels_on_generation_retirement_without_blocking_observers() {
        let source = super::super::super::pane_output_channel();
        source.set_generation(1);
        let _observer = source.subscribe();
        source.send(b"observed".to_vec());
        assert!(source.wait_for_control_admission(Some(1)).await);
        let mut control = source.subscribe();
        control.protect_control_cursor();
        source.send(b"pending".to_vec());
        let admission = source.wait_for_control_admission(Some(1));
        tokio::pin!(admission);
        pending(admission.as_mut()).await;
        source.retire_reader_for_generation(Some(1));
        assert!(!tokio::time::timeout(Duration::from_secs(1), admission)
            .await
            .unwrap());
    }

    #[test]
    fn idle_control_subscription_churn_does_not_retain_stopped_cursors() {
        let source = super::super::super::pane_output_channel();
        for _ in 0..1024 {
            let mut control = source.subscribe();
            control.protect_control_cursor();
        }
        assert_eq!(source.inner.control_cursors.lock().unwrap().len(), 1);
    }
}
