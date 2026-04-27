use std::time::{Duration, Instant};

use rmux_core::{
    command_parser::{CommandArgument, ParsedCommands},
    key_code_lookup_bits,
};
use rmux_proto::{ErrorResponse, OptionName, PaneTarget, Response, RmuxError, Target};
use tracing::warn;

use super::super::{
    scripting_support::{spawn_background_async, QueueExecutionContext},
    RequestHandler,
};
use super::{attached_status_message_for_error, display_time, AttachedKeyDispatch};
use crate::key_table::{
    default_key_table_name, lookup_attached_key_table_binding, lookup_key_table_binding,
    matches_prefix_key, session_option_key, session_option_u64, should_drop_unbound_prefix_key,
    step03_prefix_binding, Step03PrefixBinding, COPY_MODE_TABLE, COPY_MODE_VI_TABLE, PREFIX_TABLE,
};
use crate::pane_terminals::session_not_found;
use crate::renderer;

struct DirectCopyModeCommand {
    command: String,
    args: Vec<String>,
    repeat_count: usize,
}

fn direct_copy_mode_command(commands: &ParsedCommands) -> Option<DirectCopyModeCommand> {
    if !commands.assignments().is_empty() {
        return None;
    }
    let [command] = commands.commands() else {
        return None;
    };
    if !matches!(command.name(), "send" | "send-keys") {
        return None;
    }

    let mut args = command.arguments().iter();
    let mut copy_mode_command = false;
    let mut repeat_count = 1;
    let mut command_args = Vec::new();
    while let Some(argument) = args.next() {
        let value = argument.as_string()?;
        match value {
            "--" => {
                command_args.extend(copy_mode_argument_strings(args)?);
                break;
            }
            "-X" => copy_mode_command = true,
            "-N" => {
                repeat_count = args.next()?.as_string()?.parse::<usize>().ok()?.max(1);
            }
            value if value.starts_with("-N") && value.len() > 2 => {
                repeat_count = value[2..].parse::<usize>().ok()?.max(1);
            }
            value if value.starts_with('-') => return None,
            value => {
                command_args.push(value.to_owned());
                command_args.extend(copy_mode_argument_strings(args)?);
                break;
            }
        }
    }

    if !copy_mode_command {
        return None;
    }
    let command = command_args.first()?.clone();
    Some(DirectCopyModeCommand {
        command,
        args: command_args.into_iter().skip(1).collect(),
        repeat_count,
    })
}

fn copy_mode_argument_strings<'a>(
    args: impl Iterator<Item = &'a CommandArgument>,
) -> Option<Vec<String>> {
    args.map(|argument| argument.as_string().map(str::to_owned))
        .collect()
}

impl RequestHandler {
    pub(super) async fn dispatch_attached_key(
        &self,
        attach_pid: u32,
        requester_pid: u32,
        target: &PaneTarget,
        key: rmux_core::KeyCode,
    ) -> Result<(), RmuxError> {
        let _ = Box::pin(self.dispatch_attached_key_inner(
            target,
            AttachedKeyDispatch {
                attach_pid,
                requester_pid,
                current_target: Some(Target::Pane(target.clone())),
                mouse_target: None,
                key,
                attached_live_input: false,
            },
        ))
        .await?;
        Ok(())
    }

    pub(super) async fn dispatch_attached_key_inner(
        &self,
        target: &PaneTarget,
        dispatch: AttachedKeyDispatch,
    ) -> Result<bool, RmuxError> {
        let AttachedKeyDispatch {
            attach_pid,
            requester_pid,
            current_target,
            mouse_target,
            key,
            attached_live_input,
        } = dispatch;

        if self.exit_clock_mode(target).await? {
            return Ok(true);
        }

        let now = Instant::now();
        let snapshot = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            (
                active.session_name.clone(),
                active.key_table_name.clone(),
                active.key_table_set_at,
                active.repeat_deadline,
                active.repeat_active,
                active.last_key,
            )
        };

        let lookup_key = key_code_lookup_bits(key);
        let (
            session_name,
            current_table_name,
            key_table_set_at,
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
            from_prefix_table,
        ) = {
            let state = self.state.lock().await;
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

            let prefix_match = matches_prefix_key(lookup_key, prefix_key, prefix2_key);
            if table_name == default_table && prefix_match {
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
                    false,
                )
            } else {
                let from_prefix_table = table_name == PREFIX_TABLE;
                let lookup_binding = if attached_live_input {
                    lookup_attached_key_table_binding
                } else {
                    lookup_key_table_binding
                };
                let mut binding = lookup_binding(&state, &table_name, lookup_key);
                if repeat_active
                    && table_name != default_table
                    && binding.as_ref().is_some_and(|binding| !binding.repeat())
                {
                    table_name = default_table.clone();
                    binding = lookup_binding(&state, &table_name, lookup_key);
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
                    from_prefix_table,
                )
            }
        };

        let _ = (prefix_key, prefix2_key);

        if should_enter_prefix {
            self.set_attached_key_table(attach_pid, Some(PREFIX_TABLE.to_owned()), Some(now))
                .await?;
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            active.repeat_active = false;
            active.repeat_deadline = None;
            active.last_key = None;
            drop(active_attach);
            if prefix_timeout_ms != 0 {
                self.schedule_attached_prefix_timeout(attach_pid, now, prefix_timeout_ms);
            }
            return Ok(true);
        }

        let Some(binding) = binding else {
            if current_table_name
                .as_deref()
                .is_some_and(|table_name| should_drop_unbound_prefix_key(table_name, lookup_key))
            {
                self.set_attached_key_table(attach_pid, None, None).await?;
                let mut active_attach = self.active_attach.lock().await;
                if let Some(active) = active_attach.by_pid.get_mut(&attach_pid) {
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
                self.set_attached_key_table(attach_pid, None, None).await?;
                let mut active_attach = self.active_attach.lock().await;
                if let Some(active) = active_attach.by_pid.get_mut(&attach_pid) {
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

        if should_return_to_default || should_clear_before_dispatch {
            self.set_attached_key_table(attach_pid, None, None).await?;
        }
        {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            if binding.repeat() {
                active.repeat_active = true;
                active.repeat_deadline = repeat_deadline;
                active.last_key = Some(binding.key());
            } else {
                active.repeat_active = false;
                active.repeat_deadline = None;
                active.last_key = Some(binding.key());
            }
        }
        if let Some(repeat_deadline) = repeat_deadline {
            self.schedule_attached_repeat_timeout(attach_pid, repeat_deadline);
        }

        if from_prefix_table {
            if let Some(action) = step03_prefix_binding(lookup_key) {
                if let Err(error) = self.dispatch_step03_prefix_action(action, target).await {
                    if attached_live_input {
                        self.report_attached_command_error(&session_name, attach_pid, &error)
                            .await;
                        return Ok(true);
                    }
                    return Err(error);
                }
                return Ok(true);
            }
        }

        if let Some(command) = direct_copy_mode_command(binding.commands()) {
            Box::pin(self.execute_copy_mode_command(
                requester_pid,
                target.clone(),
                &command.command,
                &command.args,
                command.repeat_count,
            ))
            .await?;
            return Ok(true);
        }

        let context = QueueExecutionContext::without_caller_cwd()
            .with_current_target(Some(
                current_target.unwrap_or_else(|| Target::Pane(target.clone())),
            ))
            .with_mouse_target(mouse_target);
        if parsed_commands_block_for_prompt(binding.commands()) {
            let handler = self.clone();
            let commands = binding.commands().clone();
            spawn_background_async("rmux-attached-prompt", move || async move {
                let _ = handler
                    .execute_parsed_commands(requester_pid, commands, context)
                    .await;
            });
        } else {
            if let Err(error) = Box::pin(self.execute_parsed_commands(
                requester_pid,
                binding.commands().clone(),
                context,
            ))
            .await
            {
                if attached_live_input {
                    self.report_attached_command_error(&session_name, attach_pid, &error)
                        .await;
                    return Ok(true);
                }
                return Err(error);
            }
        }
        Ok(true)
    }

    async fn report_attached_command_error(
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
        let (overlay_frame, clear_frame, duration) = {
            let mut state = self.state.lock().await;
            state.add_message(message.clone());
            let Some(session) = state.sessions.session(session_name) else {
                return;
            };
            let mut overlay_frame = renderer::render_display_panes_clear(session, &state.options);
            overlay_frame.extend_from_slice(
                renderer::render_status_message(session, &state.options, &message).as_slice(),
            );
            let clear_frame = renderer::render_display_panes_clear(session, &state.options);
            let duration = display_time(&state.options, session_name);
            (overlay_frame, clear_frame, duration)
        };

        let _ = self
            .send_attached_overlay(session_name, overlay_frame, clear_frame, duration)
            .await;
    }

    async fn dispatch_step03_prefix_action(
        &self,
        action: Step03PrefixBinding,
        target: &PaneTarget,
    ) -> Result<(), RmuxError> {
        match action {
            Step03PrefixBinding::SelectPaneNext | Step03PrefixBinding::SelectPanePrevious => {
                let target = {
                    let state = self.state.lock().await;
                    let session = state
                        .sessions
                        .session(target.session_name())
                        .ok_or_else(|| session_not_found(target.session_name()))?;
                    let window = session.window_at(target.window_index()).ok_or_else(|| {
                        RmuxError::invalid_target(
                            target.to_string(),
                            "window index does not exist in session",
                        )
                    })?;
                    let panes = window.panes();
                    let active = window.active_pane_index();
                    let Some(position) = panes.iter().position(|pane| pane.index() == active)
                    else {
                        return Err(RmuxError::invalid_target(
                            target.to_string(),
                            "active pane index does not exist in window",
                        ));
                    };
                    let selected_position = match action {
                        Step03PrefixBinding::SelectPaneNext => (position + 1) % panes.len(),
                        Step03PrefixBinding::SelectPanePrevious => {
                            (position + panes.len() - 1) % panes.len()
                        }
                        _ => unreachable!("action filtered by outer match"),
                    };
                    PaneTarget::with_window(
                        target.session_name().clone(),
                        target.window_index(),
                        panes[selected_position].index(),
                    )
                };
                let response = self
                    .handle_select_pane(rmux_proto::SelectPaneRequest {
                        target,
                        title: None,
                    })
                    .await;
                match response {
                    Response::SelectPane(_) => Ok(()),
                    Response::Error(ErrorResponse { error }) => Err(error),
                    _ => Err(RmuxError::Server(
                        "select-pane prefix binding returned unexpected response".to_owned(),
                    )),
                }
            }
            Step03PrefixBinding::NextWindow => {
                let response = self
                    .handle_next_window(rmux_proto::NextWindowRequest {
                        target: target.session_name().clone(),
                        alerts_only: false,
                    })
                    .await;
                match response {
                    Response::NextWindow(_) => Ok(()),
                    Response::Error(ErrorResponse { error }) => Err(error),
                    _ => Err(RmuxError::Server(
                        "next-window prefix binding returned unexpected response".to_owned(),
                    )),
                }
            }
            Step03PrefixBinding::PreviousWindow => {
                let response = self
                    .handle_previous_window(rmux_proto::PreviousWindowRequest {
                        target: target.session_name().clone(),
                        alerts_only: false,
                    })
                    .await;
                match response {
                    Response::PreviousWindow(_) => Ok(()),
                    Response::Error(ErrorResponse { error }) => Err(error),
                    _ => Err(RmuxError::Server(
                        "previous-window prefix binding returned unexpected response".to_owned(),
                    )),
                }
            }
        }
    }
}

fn parsed_commands_block_for_prompt(commands: &rmux_core::command_parser::ParsedCommands) -> bool {
    commands
        .commands()
        .iter()
        .any(parsed_command_blocks_for_prompt)
}

fn parsed_command_blocks_for_prompt(command: &rmux_core::command_parser::ParsedCommand) -> bool {
    match command.name() {
        "display-panes" => !command
            .arguments()
            .iter()
            .filter_map(rmux_core::command_parser::CommandArgument::as_string)
            .any(|argument| argument.starts_with('-') && argument.contains('b')),
        "command-prompt" => !command
            .arguments()
            .iter()
            .filter_map(rmux_core::command_parser::CommandArgument::as_string)
            .any(|argument| {
                argument.starts_with('-') && (argument.contains('b') || argument.contains('i'))
            }),
        "confirm-before" => !command
            .arguments()
            .iter()
            .filter_map(rmux_core::command_parser::CommandArgument::as_string)
            .any(|argument| argument.starts_with('-') && argument.contains('b')),
        _ => false,
    }
}

#[cfg(test)]
mod parsed_command_prompt_block_tests {
    use super::*;

    #[test]
    fn block_detection_handles_combined_flags() {
        use rmux_core::command_parser::CommandParser;

        let parsed = CommandParser::new()
            .parse_one_group("command-prompt -bF { display-message hi }")
            .unwrap();
        assert!(!parsed_commands_block_for_prompt(&parsed));

        let parsed = CommandParser::new()
            .parse_one_group("command-prompt -p test { display-message hi }")
            .unwrap();
        assert!(parsed_commands_block_for_prompt(&parsed));

        let parsed = CommandParser::new()
            .parse_one_group("confirm-before -by { kill-window }")
            .unwrap();
        assert!(!parsed_commands_block_for_prompt(&parsed));

        let parsed = CommandParser::new()
            .parse_one_group("display-panes")
            .unwrap();
        assert!(parsed_commands_block_for_prompt(&parsed));

        let parsed = CommandParser::new()
            .parse_one_group("display-panes -b")
            .unwrap();
        assert!(!parsed_commands_block_for_prompt(&parsed));
    }

    #[test]
    fn direct_copy_mode_command_accepts_send_alias() {
        use rmux_core::command_parser::CommandParser;

        let parsed = CommandParser::new()
            .parse_one_group("send -N3 -X cancel")
            .unwrap();
        let command = direct_copy_mode_command(&parsed).unwrap();

        assert_eq!(command.command, "cancel");
        assert_eq!(command.args, Vec::<String>::new());
        assert_eq!(command.repeat_count, 3);
    }
}
