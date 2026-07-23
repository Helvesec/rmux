use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tokio::sync::mpsc;

/// Bounded FIFO ownership boundary between committed lifecycle events and hook
/// execution.
#[derive(Debug)]
pub(in crate::handler) struct LifecycleDispatchOutbox<T> {
    sender: mpsc::Sender<T>,
    receiver: Mutex<Option<mpsc::Receiver<T>>>,
    active: AtomicBool,
}

impl<T> LifecycleDispatchOutbox<T> {
    pub(in crate::handler) fn new(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self {
            sender,
            receiver: Mutex::new(Some(receiver)),
            active: AtomicBool::new(false),
        }
    }

    pub(in crate::handler) fn activate(&self) -> Option<mpsc::Receiver<T>> {
        let receiver = self
            .receiver
            .lock()
            .expect("lifecycle dispatch receiver mutex must not be poisoned")
            .take();
        if receiver.is_some() {
            self.active.store(true, Ordering::Release);
        }
        receiver
    }

    pub(in crate::handler) fn deactivate(&self) {
        self.active.store(false, Ordering::Release);
    }

    pub(in crate::handler) async fn send_if_active(
        &self,
        item: T,
    ) -> Result<bool, mpsc::error::SendError<T>> {
        if !self.active.load(Ordering::Acquire) {
            return Ok(false);
        }
        let permit = match self.sender.reserve().await {
            Ok(permit) => permit,
            Err(_) => return Err(mpsc::error::SendError(item)),
        };
        if !self.active.load(Ordering::Acquire) {
            return Ok(false);
        }
        permit.send(item);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::LifecycleDispatchOutbox;

    #[tokio::test]
    async fn active_outbox_backpressures_without_dropping_or_reordering() {
        let outbox = Arc::new(LifecycleDispatchOutbox::new(2));
        let mut receiver = outbox.activate().expect("first activation owns receiver");
        assert!(outbox.activate().is_none(), "receiver must have one owner");

        outbox.send_if_active(1).await.expect("queue first item");
        outbox.send_if_active(2).await.expect("queue second item");
        let blocked_outbox = Arc::clone(&outbox);
        let mut blocked = tokio::spawn(async move { blocked_outbox.send_if_active(3).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut blocked)
                .await
                .is_err(),
            "full outbox must backpressure"
        );

        assert_eq!(receiver.recv().await, Some(1));
        blocked
            .await
            .expect("blocked sender task")
            .expect("queue third item after capacity is available");
        assert_eq!(receiver.recv().await, Some(2));
        assert_eq!(receiver.recv().await, Some(3));
    }

    #[tokio::test]
    async fn inactive_outbox_does_not_retain_unit_test_events() {
        let outbox = LifecycleDispatchOutbox::new(1);
        outbox
            .send_if_active(1)
            .await
            .expect("inactive outbox is intentionally bypassed");
        let mut receiver = outbox.activate().expect("activate receiver");
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn deactivated_outbox_does_not_accept_more_events() {
        let outbox = LifecycleDispatchOutbox::new(1);
        let mut receiver = outbox.activate().expect("activate receiver");
        outbox.send_if_active(1).await.expect("queue active event");
        outbox.deactivate();
        outbox
            .send_if_active(2)
            .await
            .expect("deactivated outbox bypasses later events");

        assert_eq!(receiver.try_recv(), Ok(1));
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn deactivation_needs_receiver_drop_to_wake_a_reserved_sender() {
        let outbox = Arc::new(LifecycleDispatchOutbox::new(1));
        let receiver = outbox.activate().expect("activate receiver");
        outbox.send_if_active(1).await.expect("fill outbox");
        let blocked_outbox = Arc::clone(&outbox);
        let blocked = tokio::spawn(async move { blocked_outbox.send_if_active(2).await });
        tokio::task::yield_now().await;
        assert!(!blocked.is_finished(), "full outbox blocks the sender");

        outbox.deactivate();
        tokio::task::yield_now().await;
        assert!(
            !blocked.is_finished(),
            "deactivation alone cannot wake mpsc::Sender::reserve"
        );

        drop(receiver);
        assert!(
            blocked.await.expect("blocked sender task joins").is_err(),
            "dropping the unique receiver releases the reserved sender"
        );
    }
}
