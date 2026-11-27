use super::*;

impl ManagedSessionStore {
    /// Records pane activity once on the canonical window behind any alias.
    pub fn touch_window_pane_activity(
        &mut self,
        target: &WindowTarget,
        pane_index: u32,
    ) -> Result<bool, RmuxError> {
        let link = self.target_link(target)?;
        Ok(self
            .windows
            .resolve_mut(link)
            .expect("validated link")
            .touch_activity_for_pane(pane_index))
    }

    /// Drains queued canonical alerts without exposing a replaceable owner.
    pub fn take_window_alerts(
        &mut self,
        target: &WindowTarget,
        flags: AlertFlags,
    ) -> Result<AlertFlags, RmuxError> {
        let link = self.target_link(target)?;
        let window = self.windows.resolve_mut(link).expect("validated link");
        window.queue_alerts(flags);
        window.set_alerts_queued(true);
        let queued = window.take_alert_flags();
        window.set_alerts_queued(false);
        Ok(queued)
    }

    /// Clears shared window queue flags; occurrence alerts remain local.
    pub fn clear_window_alerts(
        &mut self,
        target: &WindowTarget,
        flags: AlertFlags,
    ) -> Result<(), RmuxError> {
        let link = self.target_link(target)?;
        self.windows
            .resolve_mut(link)
            .expect("validated link")
            .clear_alert_flags(flags);
        Ok(())
    }
}
