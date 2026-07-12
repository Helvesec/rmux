use std::time::{Duration, Instant};

use tokio::time::sleep;

use super::super::RequestHandler;

impl RequestHandler {
    pub(in crate::handler) async fn set_attached_key_table(
        &self,
        attach_pid: u32,
        key_table_name: Option<String>,
        key_table_set_at: Option<Instant>,
    ) -> Result<(), rmux_proto::RmuxError> {
        self.set_attached_key_table_with_expected_identity(
            attach_pid,
            None,
            key_table_name,
            key_table_set_at,
        )
        .await
    }

    pub(in crate::handler) async fn set_attached_key_table_for_client_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        key_table_name: Option<String>,
        key_table_set_at: Option<Instant>,
    ) -> Result<(), rmux_proto::RmuxError> {
        self.set_attached_key_table_with_expected_identity(
            attach_pid,
            Some(expected_attach_id),
            key_table_name,
            key_table_set_at,
        )
        .await
    }

    async fn set_attached_key_table_with_expected_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: Option<u64>,
        key_table_name: Option<String>,
        key_table_set_at: Option<Instant>,
    ) -> Result<(), rmux_proto::RmuxError> {
        let (previous_key_table, session_name) = {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach.by_pid.get_mut(&attach_pid).ok_or_else(|| {
                rmux_proto::RmuxError::Server("attached client disappeared".to_owned())
            })?;
            if expected_attach_id.is_some_and(|expected| active.id != expected) {
                return Err(rmux_proto::RmuxError::Server(
                    "attached client disappeared".to_owned(),
                ));
            }
            if active.key_table_name == key_table_name {
                active.key_table_set_at = key_table_set_at.filter(|_| key_table_name.is_some());
                return Ok(());
            }

            let previous = active.key_table_name.clone();
            active.key_table_name = key_table_name.clone();
            active.key_table_set_at = key_table_set_at.filter(|_| key_table_name.is_some());
            (previous, active.session_name.clone())
        };

        {
            let mut state = self.state.lock().await;
            if let Some(table_name) = key_table_name {
                let _ = state.key_bindings.get_table(&table_name, true);
            }
            if let Some(table_name) = previous_key_table {
                state.key_bindings.unref_table(&table_name);
            }
        }

        // The key table just changed (e.g. entering or leaving the "prefix"
        // table), so repaint the status bar immediately. Otherwise
        // #{client_prefix} -- and any prefix-pressed indicator built on it --
        // only refreshes on the next key event, which is too late to show the
        // prefix while it is actually being held.
        let _ = match expected_attach_id {
            Some(attach_id) => {
                self.refresh_attached_client_status_for_identity(
                    attach_pid,
                    attach_id,
                    &session_name,
                )
                .await
            }
            None => {
                self.refresh_attached_client_status(attach_pid, &session_name)
                    .await
            }
        };
        Ok(())
    }

    pub(in crate::handler) fn schedule_attached_prefix_timeout(
        &self,
        attach_pid: u32,
        key_table_set_at: Instant,
        timeout_ms: u64,
    ) {
        let handler = self.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(timeout_ms)).await;
            handler
                .clear_attached_prefix_table_if_current(attach_pid, key_table_set_at)
                .await;
        });
    }

    pub(in crate::handler) fn schedule_attached_repeat_timeout(
        &self,
        attach_pid: u32,
        repeat_deadline: Instant,
    ) {
        let handler = self.clone();
        tokio::spawn(async move {
            sleep(repeat_deadline.saturating_duration_since(Instant::now())).await;
            handler
                .clear_attached_repeat_state_if_current(attach_pid, repeat_deadline)
                .await;
        });
    }

    async fn clear_attached_prefix_table_if_current(
        &self,
        attach_pid: u32,
        key_table_set_at: Instant,
    ) {
        let (previous_key_table, session_name) = {
            let mut active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
                return;
            };
            if active.key_table_name.as_deref() != Some("prefix")
                || active.key_table_set_at != Some(key_table_set_at)
                || active.repeat_active
            {
                return;
            }

            let previous = active.key_table_name.clone();
            active.key_table_name = None;
            active.key_table_set_at = None;
            active.repeat_deadline = None;
            active.repeat_active = false;
            active.last_key = None;
            (previous, active.session_name.clone())
        };

        if let Some(table_name) = previous_key_table {
            let mut state = self.state.lock().await;
            state.key_bindings.unref_table(&table_name);
        }

        // Prefix timed out without a follow-up key; repaint so the indicator clears.
        let _ = self
            .refresh_attached_client_status(attach_pid, &session_name)
            .await;
    }

    async fn clear_attached_repeat_state_if_current(
        &self,
        attach_pid: u32,
        repeat_deadline: Instant,
    ) {
        let (previous_key_table, session_name) = {
            let mut active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
                return;
            };
            if active.repeat_deadline != Some(repeat_deadline) {
                return;
            }

            let previous = active.key_table_name.clone();
            active.key_table_name = None;
            active.key_table_set_at = None;
            active.repeat_deadline = None;
            active.repeat_active = false;
            active.last_key = None;
            (previous, active.session_name.clone())
        };

        if let Some(table_name) = previous_key_table {
            let mut state = self.state.lock().await;
            state.key_bindings.unref_table(&table_name);
        }

        let _ = self
            .refresh_attached_client_status(attach_pid, &session_name)
            .await;
    }
}
