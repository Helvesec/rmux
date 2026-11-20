use super::*;

impl PaneTranscript {
    pub(super) fn refresh_ground_timer(&mut self, now: Instant) -> Option<PaneGroundTimer> {
        if self.terminal.ground_timer_active() {
            if self.ground_timer_started_at.is_none() {
                self.ground_timer_started_at = Some(now);
                self.ground_timer_token = self.ground_timer_token.saturating_add(1);
                return Some(PaneGroundTimer {
                    deadline: now + PANE_INPUT_GROUND_TIMEOUT,
                    token: self.ground_timer_token,
                });
            }
            return None;
        }

        if self.ground_timer_started_at.take().is_some() {
            self.ground_timer_token = self.ground_timer_token.saturating_add(1);
        }
        None
    }

    pub(super) fn expire_ground_timer_if_due(&mut self, now: Instant) -> bool {
        let Some(started_at) = self.ground_timer_started_at else {
            return false;
        };
        if now.duration_since(started_at) < PANE_INPUT_GROUND_TIMEOUT {
            return false;
        }
        self.expire_ground_timer_now()
    }

    pub(crate) fn expire_ground_timer(&mut self, timer: PaneGroundTimer) -> bool {
        if self.ground_timer_token != timer.token || Instant::now() < timer.deadline {
            return false;
        }
        self.expire_ground_timer_now()
    }

    fn expire_ground_timer_now(&mut self) -> bool {
        if self.ground_timer_started_at.is_none() || !self.terminal.ground_timer_active() {
            return false;
        }
        self.terminal.ground_timer_expired();
        self.ground_timer_started_at = None;
        self.ground_timer_token = self.ground_timer_token.saturating_add(1);
        true
    }

    pub(super) fn refresh_synchronized_output_timer(
        &mut self,
        now: Instant,
        synchronized_output_started: bool,
    ) -> Option<PaneSynchronizedOutputTimer> {
        if self.terminal.synchronized_output_active() {
            if self.synchronized_output_started_at.is_none() || synchronized_output_started {
                self.synchronized_output_started_at = Some(now);
                self.synchronized_output_timer_token =
                    self.synchronized_output_timer_token.saturating_add(1);
            }
            // A successful compare-and-set transfers the sole claim to the
            // returned timer. It remains held until that timer is consumed or
            // dropped, so a rapid re-entry reuses its one live worker job.
            if self
                .synchronized_output_timer_job_claimed
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return None;
            }
            let started_at = self
                .synchronized_output_started_at
                .expect("active synchronized output has a timeout origin");
            return Some(PaneSynchronizedOutputTimer {
                deadline: started_at + PANE_SYNCHRONIZED_OUTPUT_TIMEOUT,
                token: self.synchronized_output_timer_token,
                job_ownership: PaneSynchronizedOutputTimerJobOwnership {
                    claimed: Arc::clone(&self.synchronized_output_timer_job_claimed),
                },
            });
        }

        self.clear_synchronized_output_timer();
        None
    }

    pub(super) fn expire_synchronized_output_if_due(&mut self, now: Instant) -> bool {
        let Some(started_at) = self.synchronized_output_started_at else {
            return false;
        };
        if now.duration_since(started_at) < PANE_SYNCHRONIZED_OUTPUT_TIMEOUT {
            return false;
        }
        self.finish_synchronized_output()
    }

    pub(crate) fn handle_synchronized_output_timer(
        &mut self,
        timer: PaneSynchronizedOutputTimer,
    ) -> (bool, Option<PaneSynchronizedOutputTimer>) {
        // Consume the timer's unique guard. An expired/inactive timer drops it;
        // a stale timer moves it into the replacement, preserving one live job
        // across release/re-entry without opening a second queue slot.
        let PaneSynchronizedOutputTimer {
            deadline,
            token,
            job_ownership,
        } = timer;
        if self.synchronized_output_timer_token == token && Instant::now() >= deadline {
            let changed = self.finish_synchronized_output();
            drop(job_ownership);
            return (changed, None);
        }

        let replacement =
            self.synchronized_output_started_at
                .map(|started_at| PaneSynchronizedOutputTimer {
                    deadline: started_at + PANE_SYNCHRONIZED_OUTPUT_TIMEOUT,
                    token: self.synchronized_output_timer_token,
                    job_ownership,
                });
        (false, replacement)
    }

    pub(crate) fn finish_synchronized_output(&mut self) -> bool {
        let changed = self.terminal.finish_synchronized_output();
        self.clear_synchronized_output_timer();
        changed
    }

    pub(super) fn clear_synchronized_output_timer(&mut self) {
        if self.synchronized_output_started_at.take().is_some() {
            self.synchronized_output_timer_token =
                self.synchronized_output_timer_token.saturating_add(1);
        }
        // Do not clear `synchronized_output_timer_job_claimed` here. A queued
        // job may still need to observe this token change and re-arm a later
        // interval; only its ownership guard may release the claim.
    }

    #[cfg(test)]
    pub(crate) fn force_ground_timer_expired_for_test(&mut self, timer: PaneGroundTimer) -> bool {
        if self.ground_timer_token != timer.token {
            return false;
        }
        self.expire_ground_timer_now()
    }

    #[cfg(test)]
    pub(crate) fn make_ground_timer_due_for_test(&mut self) {
        if self.ground_timer_started_at.is_some() {
            self.ground_timer_started_at = Some(Instant::now() - PANE_INPUT_GROUND_TIMEOUT);
        }
    }
}
