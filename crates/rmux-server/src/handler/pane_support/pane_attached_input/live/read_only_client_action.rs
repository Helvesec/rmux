use std::io;
use std::time::Instant;

use rmux_core::{key_code_lookup_bits, KeyCode, KEYC_ANY};
use rmux_proto::{OptionName, Response};

use super::super::bracketed_paste::{
    decode_bracketed_paste_after_append, take_incomplete_bracketed_paste_segment,
    BracketedPasteDecode,
};
use super::super::retain_partial_attached_control_input;
use super::super::retained::MAX_RETAINED_ATTACHED_CONTROL_INPUT;
use super::{decode_live_attached_key, AttachedKeyDecode};
use crate::handler::attach_support::ActiveAttachIdentity;
use crate::handler::scripting_support::{read_only_client_action, ReadOnlyClientAction};
use crate::handler::RequestHandler;
use crate::key_table::{
    effective_client_key_table_name, lookup_attached_key_table_binding, matches_prefix_key,
    session_option_key, PREFIX_TABLE,
};

struct ReadOnlyClientBinding {
    key: KeyCode,
    action: Option<ReadOnlyClientAction>,
}

struct ReadOnlyClientTable {
    name: String,
    bindings: Vec<ReadOnlyClientBinding>,
}

struct ReadOnlyClientActionSnapshot {
    session_name: rmux_proto::SessionName,
    session_id: rmux_proto::SessionId,
    default_table_name: String,
    key_table_name: Option<String>,
    key_table_generation: u64,
    prefix: Option<KeyCode>,
    prefix2: Option<KeyCode>,
    tables: Vec<ReadOnlyClientTable>,
}

impl ReadOnlyClientActionSnapshot {
    fn binding_action(
        &self,
        table_name: &str,
        key: KeyCode,
    ) -> Option<&Option<ReadOnlyClientAction>> {
        let table = self.tables.iter().find(|table| table.name == table_name)?;
        table
            .bindings
            .iter()
            .find(|binding| binding.key == key)
            .or_else(|| {
                table
                    .bindings
                    .iter()
                    .find(|binding| binding.key == KEYC_ANY)
            })
            .map(|binding| &binding.action)
    }
}

impl RequestHandler {
    pub(super) async fn handle_read_only_client_action_input(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
        backspace: Option<u8>,
        mut key_override: Option<KeyCode>,
    ) -> io::Result<()> {
        let Some(snapshot) = self.read_only_client_action_snapshot(identity).await? else {
            pending_input.clear();
            return Ok(());
        };
        let initial_table_name = snapshot.key_table_name.clone();
        let mut next_table_name = initial_table_name.clone();
        let new_input_at = pending_input.len();
        pending_input.extend_from_slice(bytes);
        let mut offset = 0;
        let mut matched_binding = false;
        let mut action = None;

        let user_keys = self.configured_user_keys().await;
        while offset < pending_input.len() {
            let slice = &pending_input[offset..];
            let slice_new_input_at = new_input_at.saturating_sub(offset);
            match decode_bracketed_paste_after_append(slice, slice_new_input_at) {
                BracketedPasteDecode::Matched { size, .. } => {
                    next_table_name = None;
                    offset += size;
                    continue;
                }
                BracketedPasteDecode::Partial => {
                    pending_input.drain(..offset);
                    let _ = take_incomplete_bracketed_paste_segment(
                        pending_input,
                        MAX_RETAINED_ATTACHED_CONTROL_INPUT,
                    );
                    retain_partial_attached_control_input(
                        "read-only client action bracketed paste",
                        pending_input,
                    )?;
                    self.commit_read_only_key_table_state(
                        identity,
                        &snapshot,
                        &initial_table_name,
                        next_table_name,
                        false,
                    )
                    .await?;
                    return Ok(());
                }
                BracketedPasteDecode::NotPaste => {}
            }

            let (size, key) = match super::super::user_key_input::decode_user_key(slice, &user_keys)
                .unwrap_or_else(|| decode_live_attached_key(slice, backspace))
            {
                AttachedKeyDecode::Matched { size, key } => (size, key),
                AttachedKeyDecode::Partial => {
                    pending_input.drain(..offset);
                    retain_partial_attached_control_input(
                        "read-only client action key",
                        pending_input,
                    )?;
                    self.commit_read_only_key_table_state(
                        identity,
                        &snapshot,
                        &initial_table_name,
                        next_table_name,
                        false,
                    )
                    .await?;
                    return Ok(());
                }
                AttachedKeyDecode::Invalid => {
                    pending_input.clear();
                    self.commit_read_only_key_table_state(
                        identity,
                        &snapshot,
                        &initial_table_name,
                        None,
                        false,
                    )
                    .await?;
                    return Ok(());
                }
            };
            if size == 0 {
                pending_input.clear();
                next_table_name = None;
                break;
            }
            offset += size;
            let key = key_override.take().unwrap_or(key);
            let lookup_key = key_code_lookup_bits(key);
            if next_table_name.as_deref() == Some(PREFIX_TABLE) {
                next_table_name = None;
                if let Some(binding_action) = snapshot.binding_action(PREFIX_TABLE, lookup_key) {
                    matched_binding = true;
                    action = binding_action.clone();
                    break;
                }
            } else if matches_prefix_key(lookup_key, snapshot.prefix, snapshot.prefix2) {
                next_table_name = Some(PREFIX_TABLE.to_owned());
            } else {
                let table_name = next_table_name
                    .as_deref()
                    .unwrap_or(&snapshot.default_table_name);
                if let Some(binding_action) = snapshot.binding_action(table_name, lookup_key) {
                    matched_binding = true;
                    action = binding_action.clone();
                    next_table_name = None;
                    break;
                }
                if next_table_name.is_some() {
                    next_table_name = None;
                }
            }
        }

        pending_input.clear();
        if !self
            .commit_read_only_key_table_state(
                identity,
                &snapshot,
                &initial_table_name,
                next_table_name,
                matched_binding,
            )
            .await?
        {
            return Ok(());
        }

        if let Some(action) = action {
            self.execute_read_only_client_action(identity, &snapshot.session_name, action)
                .await?;
        }
        Ok(())
    }

    async fn read_only_client_action_snapshot(
        &self,
        identity: ActiveAttachIdentity,
    ) -> io::Result<Option<ReadOnlyClientActionSnapshot>> {
        let state = self.state.lock().await;
        let active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .ok_or_else(|| io::Error::other("attached client disappeared"))?;
        let Some(session) = state.sessions.session(&active.session_name) else {
            return Ok(None);
        };
        if session.id() != active.session_id {
            return Ok(None);
        }

        let default_table_name = effective_client_key_table_name(&state, session, None);
        let current_table_name =
            effective_client_key_table_name(&state, session, active.key_table_name.as_deref());
        let mut table_names = vec![current_table_name];
        if !table_names.iter().any(|name| name == PREFIX_TABLE) {
            table_names.push(PREFIX_TABLE.to_owned());
        }
        let tables = table_names
            .into_iter()
            .map(|name| {
                let bindings = state
                    .key_bindings
                    .table(&name)
                    .into_iter()
                    .flat_map(|table| table.active().keys())
                    .filter_map(|key| {
                        let key = key_code_lookup_bits(*key);
                        lookup_attached_key_table_binding(&state, &name, key).map(|binding| {
                            ReadOnlyClientBinding {
                                key,
                                action: read_only_client_action(binding.commands()),
                            }
                        })
                    })
                    .collect();
                ReadOnlyClientTable { name, bindings }
            })
            .collect();

        Ok(Some(ReadOnlyClientActionSnapshot {
            session_name: active.session_name.clone(),
            session_id: active.session_id,
            default_table_name,
            key_table_name: active.key_table_name.clone(),
            key_table_generation: active.key_table_generation,
            prefix: session_option_key(&state, &active.session_name, OptionName::Prefix),
            prefix2: session_option_key(&state, &active.session_name, OptionName::Prefix2),
            tables,
        }))
    }

    async fn commit_read_only_key_table_state(
        &self,
        identity: ActiveAttachIdentity,
        snapshot: &ReadOnlyClientActionSnapshot,
        initial_table_name: &Option<String>,
        next_table_name: Option<String>,
        force_generation_check: bool,
    ) -> io::Result<bool> {
        if !force_generation_check && initial_table_name == &next_table_name {
            return Ok(true);
        }
        let set_at = (next_table_name.as_deref() == Some(PREFIX_TABLE)).then(Instant::now);
        let commit = self
            .set_attached_key_table_for_read_only_input(
                identity,
                &snapshot.session_name,
                snapshot.session_id,
                snapshot.key_table_generation,
                next_table_name,
                set_at,
            )
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        let Some(commit) = commit else {
            return Ok(false);
        };
        if let Some(set_at) = set_at {
            if commit.prefix_timeout_ms != 0 {
                self.schedule_attached_prefix_timeout_for_identity(
                    commit.identity,
                    set_at,
                    commit.key_table_generation,
                    commit.prefix_timeout_ms,
                );
            }
        }
        Ok(true)
    }

    async fn execute_read_only_client_action(
        &self,
        identity: ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        action: ReadOnlyClientAction,
    ) -> io::Result<()> {
        if !self.current_live_attach_input(identity).await {
            return Ok(());
        }
        let response = match action {
            ReadOnlyClientAction::DetachSelf => {
                self.handle_detach_client_for_identity(identity).await
            }
            ReadOnlyClientAction::SwitchSelf(request) => {
                self.handle_switch_client_ext3_for_attach_identity(identity, request)
                    .await
            }
        };
        if let Response::Error(error) = response {
            if self.current_live_attach_input(identity).await {
                self.report_attached_command_error(
                    session_name,
                    identity.attach_pid(),
                    &error.error,
                )
                .await;
            }
        }
        Ok(())
    }

    pub(super) async fn dispatch_immediate_prefix_detach(
        &self,
        identity: ActiveAttachIdentity,
        target: &rmux_proto::PaneTarget,
        bytes: &[u8],
        backspace: Option<u8>,
    ) -> io::Result<bool> {
        if self.decode_configured_user_key(bytes).await.is_some() {
            return Ok(false);
        }
        let AttachedKeyDecode::Matched {
            size: prefix_size,
            key: prefix_key,
        } = decode_live_attached_key(bytes, backspace)
        else {
            return Ok(false);
        };
        if prefix_size == 0 || prefix_size >= bytes.len() {
            return Ok(false);
        }

        let AttachedKeyDecode::Matched {
            size: command_size,
            key: command_key,
        } = decode_live_attached_key(&bytes[prefix_size..], backspace)
        else {
            return Ok(false);
        };
        if prefix_size.saturating_add(command_size) != bytes.len() {
            return Ok(false);
        }

        let is_bare_detach_binding = {
            let state = self.state.lock().await;
            let prefix = session_option_key(
                &state,
                target.session_name(),
                rmux_proto::OptionName::Prefix,
            );
            let prefix2 = session_option_key(
                &state,
                target.session_name(),
                rmux_proto::OptionName::Prefix2,
            );
            if !matches_prefix_key(prefix_key, prefix, prefix2) {
                return Ok(false);
            }
            lookup_attached_key_table_binding(
                &state,
                PREFIX_TABLE,
                key_code_lookup_bits(command_key),
            )
            .is_some_and(|binding| {
                let commands = binding.commands().commands();
                commands.len() == 1
                    && commands[0].name() == "detach-client"
                    && commands[0].arguments().is_empty()
            })
        };
        if !is_bare_detach_binding {
            return Ok(false);
        }

        if !self.current_live_attach_input(identity).await {
            return Ok(false);
        }
        match self.handle_detach_client_for_identity(identity).await {
            Response::Error(error) => Err(io::Error::other(error.error.to_string())),
            _ => Ok(true),
        }
    }
}
