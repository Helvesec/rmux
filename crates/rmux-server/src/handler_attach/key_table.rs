use std::time::{Duration, Instant};

use tokio::time::sleep;

use super::super::lifecycle_producer_tasks::{
    begin_current_lifecycle_mutation, LifecycleProducerLane,
};
use super::super::RequestHandler;

const ATTACHED_KEY_TABLE_TIMER_TASK: &str = "rmux-attached-key-table-timer";

#[cfg(test)]
#[path = "key_table/test_support.rs"]
mod test_support;

pub(in crate::handler) struct AttachedKeyTableCommit {
    pub(in crate::handler) identity: super::ActiveAttachIdentity,
    pub(in crate::handler) session_name: rmux_proto::SessionName,
    pub(in crate::handler) key_table_generation: u64,
    pub(in crate::handler) prefix_timeout_ms: u64,
}

#[derive(Clone, Copy)]
struct AttachedKeyTableExpectation<'a> {
    attach_pid: u32,
    attach_id: Option<u64>,
    session: Option<(&'a rmux_proto::SessionName, rmux_proto::SessionId)>,
    key_table_generation: Option<u64>,
    reset_repeat: bool,
}

#[derive(Clone, Copy, Debug)]
enum AttachedKeyTableTimer {
    Prefix {
        key_table_set_at: Instant,
        key_table_generation: u64,
    },
    Repeat {
        repeat_deadline: Instant,
        key_table_generation: u64,
    },
}

impl AttachedKeyTableTimer {
    fn is_current(self, active: &super::ActiveAttach) -> bool {
        match self {
            Self::Prefix {
                key_table_set_at,
                key_table_generation,
            } => {
                active.key_table_generation == key_table_generation
                    && active.key_table_name.as_deref() == Some("prefix")
                    && active.key_table_set_at == Some(key_table_set_at)
                    && !active.repeat_active
            }
            Self::Repeat {
                repeat_deadline,
                key_table_generation,
            } => {
                active.key_table_generation == key_table_generation
                    && active.repeat_deadline == Some(repeat_deadline)
            }
        }
    }
}

impl RequestHandler {
    #[cfg(test)]
    pub(crate) async fn set_attached_key_table_for_test(
        &self,
        attach_pid: u32,
        key_table_name: Option<String>,
    ) -> Result<(), rmux_proto::RmuxError> {
        self.set_attached_key_table(attach_pid, key_table_name, Some(Instant::now()))
            .await
    }

    #[cfg(test)]
    pub(in crate::handler) async fn set_attached_key_table(
        &self,
        attach_pid: u32,
        key_table_name: Option<String>,
        key_table_set_at: Option<Instant>,
    ) -> Result<(), rmux_proto::RmuxError> {
        self.set_attached_key_table_with_expected_identity(
            AttachedKeyTableExpectation {
                attach_pid,
                attach_id: None,
                session: None,
                key_table_generation: None,
                reset_repeat: false,
            },
            key_table_name,
            key_table_set_at,
        )
        .await
        .map(|_| ())
    }

    pub(in crate::handler) async fn set_attached_key_table_for_client_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        key_table_name: Option<String>,
        key_table_set_at: Option<Instant>,
    ) -> Result<(), rmux_proto::RmuxError> {
        self.set_attached_key_table_with_expected_identity(
            AttachedKeyTableExpectation {
                attach_pid,
                attach_id: Some(expected_attach_id),
                session: None,
                key_table_generation: None,
                reset_repeat: false,
            },
            key_table_name,
            key_table_set_at,
        )
        .await
        .map(|_| ())
    }

    #[cfg(test)]
    pub(in crate::handler) async fn set_attached_key_table_for_client_session_identity(
        &self,
        identity: super::ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        key_table_name: Option<String>,
        key_table_set_at: Option<Instant>,
    ) -> Result<(), rmux_proto::RmuxError> {
        self.set_attached_key_table_with_expected_identity(
            AttachedKeyTableExpectation {
                attach_pid: identity.attach_pid(),
                attach_id: Some(identity.attach_id()),
                session: Some((session_name, session_id)),
                key_table_generation: None,
                reset_repeat: false,
            },
            key_table_name,
            key_table_set_at,
        )
        .await
        .map(|_| ())
    }

    pub(in crate::handler) async fn set_attached_key_table_for_client_session_identity_if_generation(
        &self,
        identity: super::ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        expected_key_table_generation: u64,
        key_table_name: Option<String>,
        key_table_set_at: Option<Instant>,
    ) -> Result<Option<AttachedKeyTableCommit>, rmux_proto::RmuxError> {
        self.set_attached_key_table_with_expected_identity(
            AttachedKeyTableExpectation {
                attach_pid: identity.attach_pid(),
                attach_id: Some(identity.attach_id()),
                session: Some((session_name, session_id)),
                key_table_generation: Some(expected_key_table_generation),
                reset_repeat: false,
            },
            key_table_name,
            key_table_set_at,
        )
        .await
    }

    pub(in crate::handler) async fn set_attached_key_table_for_client_session_identity_and_reset_repeat(
        &self,
        identity: super::ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        key_table_name: Option<String>,
        key_table_set_at: Option<Instant>,
    ) -> Result<AttachedKeyTableCommit, rmux_proto::RmuxError> {
        self.set_attached_key_table_with_expected_identity(
            AttachedKeyTableExpectation {
                attach_pid: identity.attach_pid(),
                attach_id: Some(identity.attach_id()),
                session: Some((session_name, identity.session_id())),
                key_table_generation: None,
                reset_repeat: true,
            },
            key_table_name,
            key_table_set_at,
        )
        .await?
        .ok_or_else(|| {
            rmux_proto::RmuxError::Server("attached key table commit disappeared".to_owned())
        })
    }

    async fn set_attached_key_table_with_expected_identity(
        &self,
        expectation: AttachedKeyTableExpectation<'_>,
        key_table_name: Option<String>,
        key_table_set_at: Option<Instant>,
    ) -> Result<Option<AttachedKeyTableCommit>, rmux_proto::RmuxError> {
        let (commit, table_changed) = {
            // The table selection and its store references are one transaction.
            // Keeping the established state -> active_attach order also serializes
            // concurrent setters before either transition becomes observable.
            let mut state = self.state.lock().await;
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&expectation.attach_pid)
                .ok_or_else(|| {
                    rmux_proto::RmuxError::Server("attached client disappeared".to_owned())
                })?;
            if expectation
                .attach_id
                .is_some_and(|expected| active.id != expected)
            {
                return Err(rmux_proto::RmuxError::Server(
                    "attached client disappeared".to_owned(),
                ));
            }
            if expectation
                .session
                .is_some_and(|(session_name, session_id)| {
                    &active.session_name != session_name || active.session_id != session_id
                })
            {
                return Err(rmux_proto::RmuxError::Server(
                    "attached client changed session".to_owned(),
                ));
            }
            if expectation
                .key_table_generation
                .is_some_and(|expected| active.key_table_generation != expected)
            {
                return Ok(None);
            }

            active.key_table_generation = active.key_table_generation.wrapping_add(1);
            let key_table_generation = active.key_table_generation;
            let table_changed = if active.key_table_name == key_table_name {
                active.key_table_set_at = key_table_set_at.filter(|_| key_table_name.is_some());
                false
            } else {
                let previous_key_table = active.key_table_name.clone();
                active.key_table_name = key_table_name.clone();
                active.key_table_set_at = key_table_set_at.filter(|_| key_table_name.is_some());
                if let Some(table_name) = key_table_name.as_deref() {
                    let _ = state.key_bindings.get_table(table_name, true);
                }
                if let Some(table_name) = previous_key_table {
                    state.key_bindings.unref_table(&table_name);
                }
                #[cfg(test)]
                self.pause_attached_key_table_transition_commit();
                true
            };
            if expectation.reset_repeat {
                active.repeat_active = false;
                active.repeat_deadline = None;
                active.last_key = None;
            }
            let session_name = active.session_name.clone();
            let prefix_timeout_ms = if key_table_name.as_deref() == Some("prefix") {
                state
                    .options
                    .resolve(Some(&session_name), rmux_proto::OptionName::PrefixTimeout)
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(0)
            } else {
                0
            };
            (
                AttachedKeyTableCommit {
                    identity: active.identity(expectation.attach_pid),
                    session_name,
                    key_table_generation,
                    prefix_timeout_ms,
                },
                table_changed,
            )
        };

        if !table_changed {
            return Ok(Some(commit));
        }

        // The key table just changed (e.g. entering or leaving the "prefix"
        // table), so repaint the status bar immediately. Otherwise
        // #{client_prefix} -- and any prefix-pressed indicator built on it --
        // only refreshes on the next key event, which is too late to show the
        // prefix while it is actually being held.
        let _ = match expectation.attach_id {
            Some(attach_id) => {
                self.refresh_attached_client_status_for_identity(
                    expectation.attach_pid,
                    attach_id,
                    &commit.session_name,
                )
                .await
            }
            None => {
                self.refresh_attached_client_status(expectation.attach_pid, &commit.session_name)
                    .await
            }
        };
        Ok(Some(commit))
    }

    pub(in crate::handler) fn schedule_attached_prefix_timeout_for_identity(
        &self,
        identity: super::ActiveAttachIdentity,
        key_table_set_at: Instant,
        key_table_generation: u64,
        timeout_ms: u64,
    ) {
        drop(self.spawn_attached_key_table_timer(
            identity,
            Duration::from_millis(timeout_ms),
            AttachedKeyTableTimer::Prefix {
                key_table_set_at,
                key_table_generation,
            },
        ));
    }

    pub(in crate::handler) fn schedule_attached_repeat_timeout_for_identity(
        &self,
        identity: super::ActiveAttachIdentity,
        repeat_deadline: Instant,
        key_table_generation: u64,
    ) {
        let delay = repeat_deadline.saturating_duration_since(Instant::now());
        drop(self.spawn_attached_key_table_timer(
            identity,
            delay,
            AttachedKeyTableTimer::Repeat {
                repeat_deadline,
                key_table_generation,
            },
        ));
    }

    fn spawn_attached_key_table_timer(
        &self,
        identity: super::ActiveAttachIdentity,
        delay: Duration,
        timer: AttachedKeyTableTimer,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let admission = self
            .lifecycle_producers
            .try_register_in_lane(LifecycleProducerLane::Normal)?;
        let weak_handler = self.downgrade();
        Some(self.spawn_pre_admitted_lifecycle_producer_task_handle(
            ATTACHED_KEY_TABLE_TIMER_TASK,
            admission,
            async move {
                sleep(delay).await;
                let Some(handler) = weak_handler.upgrade() else {
                    return;
                };
                #[cfg(test)]
                handler.pause_attached_key_table_timer_expiry().await;
                handler
                    .expire_attached_key_table_timer(identity, timer)
                    .await;
            },
        ))
    }

    async fn expire_attached_key_table_timer(
        &self,
        identity: super::ActiveAttachIdentity,
        timer: AttachedKeyTableTimer,
    ) {
        // The established atomic lock order is state -> active_attach. Acquire and
        // revalidate both before starting the no-await lifecycle mutation scope.
        let mut state = self.state.lock().await;
        let mut active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach.by_pid.get_mut(&identity.attach_pid()) else {
            return;
        };
        let session_name = active.session_name.clone();
        let session_is_current = state
            .sessions
            .session(&session_name)
            .is_some_and(|session| {
                session.id() == identity.session_id() && session.id() == active.session_id
            });
        if !session_is_current || active.id != identity.attach_id() || !timer.is_current(active) {
            return;
        }

        let Some(mutation) = begin_current_lifecycle_mutation() else {
            return;
        };
        #[cfg(test)]
        self.pause_attached_key_table_timer_mutation();

        let previous_key_table = active.key_table_name.take();
        active.key_table_set_at = None;
        active.key_table_generation = active.key_table_generation.wrapping_add(1);
        active.repeat_deadline = None;
        active.repeat_active = false;
        active.last_key = None;
        if let Some(table_name) = previous_key_table {
            state.key_bindings.unref_table(&table_name);
        }
        drop(mutation);
        drop(active_attach);
        drop(state);

        #[cfg(test)]
        self.pause_attached_key_table_timer_refresh().await;
        // Prefix/repeat expiry only owns the local state transaction above. Status
        // rendering and output remain cancellable during shutdown.
        let _ = self
            .refresh_attached_client_status_for_identity(
                identity.attach_pid(),
                identity.attach_id(),
                &session_name,
            )
            .await;
    }

    #[cfg(test)]
    pub(in crate::handler) fn schedule_attached_prefix_timeout_for_test(
        &self,
        identity: super::ActiveAttachIdentity,
        key_table_set_at: Instant,
        key_table_generation: u64,
        delay: Duration,
    ) -> Option<tokio::task::JoinHandle<()>> {
        self.spawn_attached_key_table_timer(
            identity,
            delay,
            AttachedKeyTableTimer::Prefix {
                key_table_set_at,
                key_table_generation,
            },
        )
    }

    #[cfg(test)]
    pub(in crate::handler) fn schedule_attached_repeat_timeout_for_test(
        &self,
        identity: super::ActiveAttachIdentity,
        repeat_deadline: Instant,
        key_table_generation: u64,
        delay: Duration,
    ) -> Option<tokio::task::JoinHandle<()>> {
        self.spawn_attached_key_table_timer(
            identity,
            delay,
            AttachedKeyTableTimer::Repeat {
                repeat_deadline,
                key_table_generation,
            },
        )
    }
}
