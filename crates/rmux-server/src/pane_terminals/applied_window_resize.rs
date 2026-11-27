use rmux_core::WindowId;
use rmux_proto::WindowTarget;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppliedWindowResize {
    target: WindowTarget,
    window_id: Option<WindowId>,
    layout_change_prepared: bool,
}

impl AppliedWindowResize {
    pub(crate) fn window_id(&self) -> Option<WindowId> {
        self.window_id
    }

    pub(crate) fn into_parts(self) -> (WindowTarget, bool, Option<WindowId>) {
        (self.target, self.layout_change_prepared, self.window_id)
    }

    fn matches(&self, target: &WindowTarget, window_id: Option<WindowId>) -> bool {
        self.target == *target
            || self
                .window_id
                .zip(window_id)
                .is_some_and(|(pending, candidate)| pending == candidate)
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AppliedWindowResizeQueue {
    pending: Vec<AppliedWindowResize>,
}

impl AppliedWindowResizeQueue {
    pub(crate) fn record(&mut self, target: WindowTarget, window_id: Option<WindowId>) {
        if self
            .pending
            .iter()
            .any(|resize| resize.matches(&target, window_id))
        {
            return;
        }
        self.pending.push(AppliedWindowResize {
            target,
            window_id,
            layout_change_prepared: false,
        });
    }

    pub(crate) fn claim_layout_change(
        &mut self,
        target: &WindowTarget,
        window_id: Option<WindowId>,
    ) {
        if let Some(resize) = self
            .pending
            .iter_mut()
            .find(|resize| resize.matches(target, window_id))
        {
            resize.layout_change_prepared = true;
        }
    }

    pub(crate) fn take(&mut self) -> Vec<AppliedWindowResize> {
        std::mem::take(&mut self.pending)
    }

    pub(crate) fn take_for_window(
        &mut self,
        target: &WindowTarget,
        window_id: Option<WindowId>,
    ) -> Option<AppliedWindowResize> {
        let index = self
            .pending
            .iter()
            .position(|resize| resize.matches(target, window_id))?;
        Some(self.pending.remove(index))
    }
}

#[cfg(test)]
mod tests {
    use rmux_core::WindowId;
    use rmux_proto::{SessionName, WindowTarget};

    use super::AppliedWindowResizeQueue;

    #[test]
    fn linked_alias_claims_the_pending_layout_producer_by_window_identity() {
        let mut queue = AppliedWindowResizeQueue::default();
        let source = target("source", 0);
        let alias = target("alias", 7);
        let window_id = WindowId::new(42);
        queue.record(source, Some(window_id));

        queue.claim_layout_change(&alias, Some(window_id));

        let pending = queue.take();
        assert_eq!(pending.len(), 1);
        let (_, layout_change_prepared, identity) = pending
            .into_iter()
            .next()
            .expect("recorded resize")
            .into_parts();
        assert!(layout_change_prepared);
        assert_eq!(identity, Some(window_id));
    }

    #[test]
    fn unrelated_layout_event_does_not_claim_the_pending_resize() {
        let mut queue = AppliedWindowResizeQueue::default();
        queue.record(target("source", 0), Some(WindowId::new(42)));

        queue.claim_layout_change(&target("other", 0), Some(WindowId::new(43)));

        let (_, layout_change_prepared, _) = queue
            .take()
            .into_iter()
            .next()
            .expect("recorded resize")
            .into_parts();
        assert!(!layout_change_prepared);
    }

    fn target(session: &str, window_index: u32) -> WindowTarget {
        WindowTarget::with_window(
            SessionName::new(session).expect("test session name"),
            window_index,
        )
    }

    #[test]
    fn taking_one_window_preserves_other_pending_publications() {
        let mut queue = AppliedWindowResizeQueue::default();
        queue.record(target("first", 0), Some(WindowId::new(42)));
        queue.record(target("other", 1), Some(WindowId::new(43)));
        let resize = queue
            .take_for_window(&target("linked", 7), Some(WindowId::new(42)))
            .unwrap();
        assert_eq!(resize.window_id(), Some(WindowId::new(42)));
        let remaining = queue.take();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].window_id(), Some(WindowId::new(43)));
    }
}
