use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tokio::sync::mpsc;

#[derive(Debug)]
pub(in crate::handler) struct BoundedDispatchQueue<T> {
    sender: mpsc::Sender<T>,
    receiver: Mutex<Option<mpsc::Receiver<T>>>,
    active: AtomicBool,
}

impl<T> BoundedDispatchQueue<T> {
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
        self.sender.send(item).await.map(|()| true)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::BoundedDispatchQueue;

    #[tokio::test]
    async fn active_queue_backpressures_without_dropping_or_reordering() {
        let queue = Arc::new(BoundedDispatchQueue::new(2));
        let mut receiver = queue.activate().expect("first activation owns receiver");
        assert!(queue.activate().is_none(), "receiver must have one owner");

        queue.send_if_active(1).await.expect("queue first item");
        queue.send_if_active(2).await.expect("queue second item");
        let blocked_queue = Arc::clone(&queue);
        let mut blocked = tokio::spawn(async move { blocked_queue.send_if_active(3).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut blocked)
                .await
                .is_err(),
            "full queue must backpressure"
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
    async fn inactive_queue_does_not_retain_unit_test_events() {
        let queue = BoundedDispatchQueue::new(1);
        queue
            .send_if_active(1)
            .await
            .expect("inactive queue is intentionally bypassed");
        let mut receiver = queue.activate().expect("activate receiver");
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn deactivated_queue_does_not_accept_more_events() {
        let queue = BoundedDispatchQueue::new(1);
        let mut receiver = queue.activate().expect("activate receiver");
        queue.send_if_active(1).await.expect("queue active event");
        queue.deactivate();
        queue
            .send_if_active(2)
            .await
            .expect("deactivated queue bypasses later events");

        assert_eq!(receiver.try_recv(), Ok(1));
        assert!(receiver.try_recv().is_err());
    }
}
