use std::time::{Duration, Instant};

use rmux_core::key_code_lookup_bits;
use rmux_proto::{OptionName, PaneTarget, RmuxError, Target};
use tracing::warn;

use super::super::copy_mode_support::key_binding::direct_copy_mode_command;
use super::super::RequestHandler;
use super::{attached_status_message_for_error, display_time, AttachedKeyDispatch};
use crate::key_table::{
    default_key_table_name, lookup_attached_key_table_binding, lookup_key_table_binding,
    matches_prefix_key, session_option_key, session_option_u64, should_drop_unbound_prefix_key,
    COPY_MODE_TABLE, COPY_MODE_VI_TABLE, PREFIX_TABLE,
};

mod commands;

#[cfg(test)]
#[path = "../../handler_pane/attached_key_dispatch/test_support.rs"]
mod test_support;

use commands::{execute_attached_binding_commands, AttachedBindingCommandContext};

#[derive(Clone, Copy)]
struct AttachedKeyTableCommitContext<'a> {
    identity: super::super::attach_support::ActiveAttachIdentity,
    session_name: &'a rmux_proto::SessionName,
    expected_generation: u64,
}

impl RequestHandler {
    #[async_recursion::async_recursion]
    pub(super) async fn dispatch_attached_key(
        &self,
        attach_pid: u32,
        requester_pid: u32,
        target: &PaneTarget,
        key: rmux_core::KeyCode,
    ) -> Result<(), RmuxError> {
        let handled = Box::pin(self.dispatch_attached_key_inner(
            target,
            AttachedKeyDispatch {
                attach_pid,
                live_identity: None,
                live_session_id: None,
                requester_pid,
                current_target: Some(Target::Pane(target.clone())),
                mouse_target: None,
                mouse_event: None,
                key,
                attached_live_input: false,
            },
        ))
        .await?;
        if !handled {
            let response = self
                .handle_send_keys(rmux_proto::SendKeysRequest {
                    target: target.clone(),
                    keys: vec![rmux_core::key_string_lookup_key(key, true)],
                })
                .await;
            if let rmux_proto::Response::Error(error) = response {
                return Err(error.error);
            }
        }
        Ok(())
    }

    pub(super) async fn dispatch_attached_key_inner(
        &self,
        target: &PaneTarget,
        dispatch: AttachedKeyDispatch,
    ) -> Result<bool, RmuxError> {
        self.dispatch_attached_key_with_primary(target, dispatch, None)
            .await
    }

    #[async_recursion::async_recursion]
    pub(super) async fn dispatch_attached_key_with_primary(
        &self,
        target: &PaneTarget,
        dispatch: AttachedKeyDispatch,
        primary_key: Option<rmux_core::KeyCode>,
    ) -> Result<bool, RmuxError> {
        let AttachedKeyDispatch {
            attach_pid,
            live_identity,
            live_session_id,
            requester_pid,
            current_target,
            mouse_target,
            mouse_event,
            key,
            attached_live_input,
        } = dispatch;

        let exited_clock_mode = match (live_identity, live_session_id) {
            (Some(identity), Some(session_id)) => {
                self.exit_clock_mode_for_attached_identity(target, identity, session_id)
                    .await?
            }
            (Some(_), None) => {
                return Err(RmuxError::Server(
                    "attached client session identity missing".to_owned(),
                ));
            }
            (None, _) => self.exit_clock_mode(target).await?,
        };
        if exited_clock_mode {
            return Ok(true);
        }

        let now = Instant::now();
        let snapshot = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| {
                    live_identity.is_none_or(|identity| {
                        identity.matches_active(active)
                            && live_session_id.is_some_and(|expected| {
                                active.session_id == expected
                                    && active.session_name == *target.session_name()
                            })
                            && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    })
                })
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            (
                active.identity(attach_pid),
                active.session_name.clone(),
                active.session_id,
                active.client_name.clone(),
                active.key_table_name.clone(),
                active.key_table_set_at,
                active.key_table_generation,
                active.repeat_deadline,
                active.repeat_active,
                active.last_key,
            )
        };

        let lookup_key = key_code_lookup_bits(key);
        let primary_key = primary_key.map(key_code_lookup_bits);
        let (
            key_table_identity,
            session_name,
            session_id,
            client_name,
            current_table_name,
            key_table_set_at,
            key_table_generation,
            repeat_deadline,
            repeat_active,
            last_key,
        ) = snapshot;
        let (
            default_table,
            prefix_key,
            prefix2_key,
            prefix_timeout_ms,
            repeat_time_ms,
            initial_repeat_time_ms,
            binding,
            should_enter_prefix,
            should_clear_before_dispatch,
        ) = {
            let state = self.state.lock().await;
            if live_session_id.is_some()
                && state
                    .sessions
                    .session(target.session_name())
                    .is_none_or(|session| session.id() != session_id)
            {
                return Ok(true);
            }
            let default_table = default_key_table_name(&state, target);
            let prefix_key = session_option_key(&state, &session_name, OptionName::Prefix);
            let prefix2_key = session_option_key(&state, &session_name, OptionName::Prefix2);
            let prefix_timeout_ms =
                session_option_u64(&state, &session_name, OptionName::PrefixTimeout);
            let repeat_time_ms = session_option_u64(&state, &session_name, OptionName::RepeatTime);
            let initial_repeat_time_ms =
                session_option_u64(&state, &session_name, OptionName::InitialRepeatTime);

            let mut table_name = current_table_name
                .clone()
                .unwrap_or_else(|| default_table.clone());
            let mut should_clear = false;

            if repeat_deadline.is_some_and(|deadline| now > deadline) {
                table_name = default_table.clone();
                should_clear = true;
            }
            if current_table_name.as_deref() == Some(PREFIX_TABLE)
                && prefix_timeout_ms != 0
                && !repeat_active
                && key_table_set_at.is_some_and(|set_at| {
                    now.duration_since(set_at).as_millis() > u128::from(prefix_timeout_ms)
                })
            {
                table_name = default_table.clone();
                should_clear = true;
            }

            let prefix_match = matches_prefix_key(lookup_key, prefix_key, prefix2_key)
                || primary_key.is_some_and(|key| matches_prefix_key(key, prefix_key, prefix2_key));
            // tmux gives prefix/prefix2 precedence over every current table.
            // Keep an already-active prefix table so its send-prefix binding runs.
            if table_name != PREFIX_TABLE && prefix_match {
                (
                    default_table,
                    prefix_key,
                    prefix2_key,
                    prefix_timeout_ms,
                    repeat_time_ms,
                    initial_repeat_time_ms,
                    None,
                    true,
                    should_clear,
                )
            } else {
                let lookup = if attached_live_input {
                    lookup_attached_key_table_binding
                } else {
                    lookup_key_table_binding
                };
                // An acquired alternate supplements the explicit primary chord.
                // Resolve both in the same table snapshot, before Any and before
                // any dispatch side effect (not two successive key dispatches).
                let lookup_binding = |table_name: &str| {
                    primary_key
                        .and_then(|key| lookup(&state, table_name, key))
                        .filter(|binding| binding.key() != rmux_core::KEYC_ANY)
                        .or_else(|| lookup(&state, table_name, lookup_key))
                };
                let mut binding = lookup_binding(&table_name);
                if repeat_active
                    && table_name != default_table
                    && binding.as_ref().is_some_and(|binding| !binding.repeat())
                {
                    table_name = default_table.clone();
                    binding = lookup_binding(&table_name);
                    should_clear = true;
                }

                (
                    default_table,
                    prefix_key,
                    prefix2_key,
                    prefix_timeout_ms,
                    repeat_time_ms,
                    initial_repeat_time_ms,
                    binding,
                    false,
                    should_clear,
                )
            }
        };

        #[cfg(test)]
        self.pause_attached_key_dispatch_after_lookup(attach_pid)
            .await;

        let key_table_commit = AttachedKeyTableCommitContext {
            identity: key_table_identity,
            session_name: &session_name,
            expected_generation: key_table_generation,
        };
        let _ = (prefix_key, prefix2_key);

        if should_enter_prefix {
            let Some(commit) = self
                .set_attached_key_table_for_dispatch(
                    key_table_commit,
                    Some(PREFIX_TABLE.to_owned()),
                    Some(now),
                )
                .await?
            else {
                return Ok(true);
            };
            let timer_identity = {
                let mut active_attach = self.active_attach.lock().await;
                let active = active_attach
                    .by_pid
                    .get_mut(&attach_pid)
                    .filter(|active| {
                        key_table_identity.matches_active_session(active, &session_name, session_id)
                    })
                    .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
                if active.key_table_generation != commit.key_table_generation {
                    return Ok(true);
                }
                active.repeat_active = false;
                active.repeat_deadline = None;
                active.last_key = None;
                (active.identity(attach_pid), active.key_table_generation)
            };
            if prefix_timeout_ms != 0 {
                self.schedule_attached_prefix_timeout_for_identity(
                    timer_identity.0,
                    now,
                    timer_identity.1,
                    prefix_timeout_ms,
                );
            }
            return Ok(true);
        }

        let Some(binding) = binding else {
            if current_table_name
                .as_deref()
                .is_some_and(|table_name| should_drop_unbound_prefix_key(table_name, lookup_key))
            {
                let commit = self
                    .set_attached_key_table_for_dispatch(key_table_commit, None, None)
                    .await?;
                let Some(commit) = commit else {
                    return Ok(true);
                };
                let mut active_attach = self.active_attach.lock().await;
                if let Some(active) = active_attach
                    .by_pid
                    .get_mut(&attach_pid)
                    .filter(|active| {
                        key_table_identity.matches_active_session(active, &session_name, session_id)
                    })
                    .filter(|active| active.key_table_generation == commit.key_table_generation)
                {
                    active.repeat_active = false;
                    active.repeat_deadline = None;
                    active.last_key = None;
                }
                return Ok(true);
            }
            if should_clear_before_dispatch
                || current_table_name
                    .as_deref()
                    .is_some_and(|table_name| table_name != default_table.as_str())
            {
                let commit = self
                    .set_attached_key_table_for_dispatch(key_table_commit, None, None)
                    .await?;
                let Some(commit) = commit else {
                    return Ok(true);
                };
                let mut active_attach = self.active_attach.lock().await;
                if let Some(active) = active_attach
                    .by_pid
                    .get_mut(&attach_pid)
                    .filter(|active| {
                        key_table_identity.matches_active_session(active, &session_name, session_id)
                    })
                    .filter(|active| active.key_table_generation == commit.key_table_generation)
                {
                    active.repeat_active = false;
                    active.repeat_deadline = None;
                    active.last_key = None;
                }
            }
            if matches!(default_table.as_str(), COPY_MODE_TABLE | COPY_MODE_VI_TABLE) {
                return Ok(true);
            }
            return Ok(false);
        };

        let first_repeat = !repeat_active || last_key != Some(binding.key());
        let repeat_window_ms = if binding.repeat() {
            if first_repeat && initial_repeat_time_ms != 0 {
                initial_repeat_time_ms
            } else {
                repeat_time_ms
            }
        } else {
            0
        };
        let repeat_deadline = binding
            .repeat()
            .then_some(now + Duration::from_millis(repeat_window_ms.max(1)));
        let should_return_to_default = current_table_name
            .as_deref()
            .is_some_and(|table_name| table_name != default_table)
            && !binding.repeat();

        // Binding lookup happened before awaits. Treat its table generation as a
        // CAS token so an older dispatch cannot install repeat state on a newer
        // table selection.
        let expected_repeat_generation = if should_return_to_default || should_clear_before_dispatch
        {
            self.set_attached_key_table_for_dispatch(key_table_commit, None, None)
                .await?
                .map(|commit| commit.key_table_generation)
        } else {
            Some(key_table_generation)
        };
        let timer_identity = if let Some(expected_repeat_generation) = expected_repeat_generation {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .filter(|active| {
                    key_table_identity.matches_active_session(active, &session_name, session_id)
                })
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            if active.key_table_generation != expected_repeat_generation {
                None
            } else {
                if binding.repeat() {
                    active.repeat_active = true;
                    active.repeat_deadline = repeat_deadline;
                    active.last_key = Some(binding.key());
                } else {
                    active.repeat_active = false;
                    active.repeat_deadline = None;
                    active.last_key = Some(binding.key());
                }
                Some((active.identity(attach_pid), active.key_table_generation))
            }
        } else {
            None
        };
        if let (Some(timer_identity), Some(repeat_deadline)) = (timer_identity, repeat_deadline) {
            self.schedule_attached_repeat_timeout_for_identity(
                timer_identity.0,
                repeat_deadline,
                timer_identity.1,
            );
        }

        if let Some(command) = direct_copy_mode_command(binding.commands()) {
            Box::pin(self.execute_direct_copy_mode_binding(
                requester_pid,
                live_identity,
                target.clone(),
                &command,
                mouse_event,
            ))
            .await?;
            return Ok(true);
        }

        let dispatch_target = current_target.unwrap_or_else(|| Target::Pane(target.clone()));
        Box::pin(execute_attached_binding_commands(
            self,
            AttachedBindingCommandContext {
                attach_pid,
                live_identity,
                requester_pid,
                session_name: session_name.clone(),
                session_id,
                client_name,
                attached_live_input,
                dispatch_target,
                mouse_target,
                mouse_event,
                commands: binding.commands().clone(),
            },
        ))
        .await?;
        Ok(true)
    }

    async fn set_attached_key_table_for_dispatch(
        &self,
        context: AttachedKeyTableCommitContext<'_>,
        key_table_name: Option<String>,
        key_table_set_at: Option<Instant>,
    ) -> Result<Option<super::super::attach_support::AttachedKeyTableCommit>, RmuxError> {
        self.set_attached_key_table_for_client_session_identity_if_generation(
            context.identity,
            context.session_name,
            context.identity.session_id(),
            context.expected_generation,
            key_table_name,
            key_table_set_at,
        )
        .await
    }

    pub(in crate::handler) async fn report_attached_command_error(
        &self,
        session_name: &rmux_proto::SessionName,
        attach_pid: u32,
        error: &RmuxError,
    ) {
        warn!(
            attach_pid,
            session = %session_name,
            "attached input command failed: {error}"
        );

        let message = attached_status_message_for_error(error);
        let duration = {
            let mut state = self.state.lock().await;
            state.add_message(message.clone());
            let Some(session) = state.sessions.session(session_name) else {
                return;
            };
            let _ = session;
            display_time(&state.options, session_name)
        };

        let _ = self
            .send_attached_overlay(
                session_name,
                message,
                (!duration.is_zero()).then_some(duration),
                crate::handler::attach_support::TransientMessageInputPolicy::DismissAndForward,
            )
            .await;
    }
}
