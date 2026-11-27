use crate::control::QueueExecutionError;

use super::{
    aggregate_rmux_errors, command_has_independent_target, control_command_frame_outcome,
    control_command_response_has_individual_guards, control_command_response_stream_is_active,
    control_queue_eof_action, expected_attach_follows_registration, parsed_command_count,
    remove_group_contexts, send_control_command_response_event, validate_expected_attach_identity,
    with_control_queue_identity, CommandQueue, ControlClientIdentity, ControlCommandResponseEvent,
    ControlCommandResult, ControlQueueCommandOrigin, ControlQueueEofAction, ParsedCommands,
    QueueCommandAction, QueueExecutionContext, QueueMode, RequestHandler, RmuxError, Target,
    VecDeque, CONTROL_QUEUE_INSERTED_COMMAND_LIMIT,
};

impl RequestHandler {
    #[async_recursion::async_recursion]
    pub(super) async fn execute_command_queue(
        &self,
        requester_pid: u32,
        commands: ParsedCommands,
        context: &mut QueueExecutionContext,
        mode: QueueMode,
        expected_control_id: Option<u64>,
    ) -> ControlCommandResult {
        if let Err(error) = validate_expected_attach_identity(self, requester_pid).await {
            return ControlCommandResult {
                stdout: Vec::new(),
                error: Some(error.clone()),
                source_file_error: None,
                execution_errors: vec![QueueExecutionError::Runtime(error)],
                exit_status: Some(1),
                server_shutdown_started: false,
            };
        }
        let control_identity = expected_control_id
            .map(|control_id| ControlClientIdentity::new(requester_pid, control_id));
        let response_stream_is_active =
            control_identity.is_some_and(control_command_response_stream_is_active);
        if control_queue_eof_action(control_identity) == ControlQueueEofAction::StopFrame {
            return ControlCommandResult {
                stdout: Vec::new(),
                error: None,
                source_file_error: None,
                execution_errors: Vec::new(),
                exit_status: None,
                server_shutdown_started: false,
            };
        }
        if let Err(error) = self
            .apply_parse_time_assignments(requester_pid, &commands, expected_control_id)
            .await
        {
            return ControlCommandResult {
                stdout: Vec::new(),
                error: Some(error.clone()),
                source_file_error: None,
                execution_errors: vec![QueueExecutionError::Runtime(error)],
                exit_status: Some(1),
                server_shutdown_started: false,
            };
        }
        let mut queue = CommandQueue::from_parsed(commands);
        let mut contexts = VecDeque::from(vec![context.clone(); queue.len()]);
        let mut stdout = Vec::new();
        let mut errors = Vec::new();
        let mut source_file_errors = Vec::new();
        let mut execution_errors = Vec::new();
        let mut exit_status = None;
        let mut inserted_command_count = 0_usize;
        let mut server_shutdown_started = false;
        let mut child_guard_stream_started = false;

        'command_queue: while !queue.is_empty() {
            let expected_attach = match validate_expected_attach_identity(self, requester_pid).await
            {
                Ok(identity) => identity,
                Err(error) => {
                    execution_errors.push(QueueExecutionError::Runtime(error.clone()));
                    errors.push(error);
                    exit_status = Some(1);
                    break 'command_queue;
                }
            };
            if expected_attach_follows_registration()
                && contexts
                    .iter()
                    .any(QueueExecutionContext::follows_attached_session)
            {
                let Some(identity) = expected_attach else {
                    let error = RmuxError::Server(
                        "background command lost its attached client identity".to_owned(),
                    );
                    execution_errors.push(QueueExecutionError::Runtime(error.clone()));
                    errors.push(error);
                    exit_status = Some(1);
                    break 'command_queue;
                };
                let target = match self.attached_queue_target_for_registration(identity).await {
                    Ok(target) => target,
                    Err(error) => {
                        execution_errors.push(QueueExecutionError::Runtime(error.clone()));
                        errors.push(error);
                        exit_status = Some(1);
                        break 'command_queue;
                    }
                };
                for context in &mut contexts {
                    if context.follows_attached_session() {
                        context.rebase_current_target_after_attached_switch(target.clone());
                    }
                }
            }
            if control_queue_eof_action(control_identity) == ControlQueueEofAction::StopFrame {
                break 'command_queue;
            }
            let item = queue.pop_front().expect("non-empty command queue");
            let item_context = contexts
                .pop_front()
                .expect("queue item context must stay aligned")
                .with_client_loop_snapshots(self.client_loop_snapshots().await);
            if response_stream_is_active && child_guard_stream_started {
                let origin = item_context
                    .control_queue_origin()
                    .unwrap_or(ControlQueueCommandOrigin::QueueContinuation);
                if !send_control_command_response_event(
                    control_identity.expect("response streams have a control identity"),
                    ControlCommandResponseEvent::FrameStarted { origin },
                ) {
                    break 'command_queue;
                }
            }
            let command_execution = self.execute_queued_command(
                requester_pid,
                item.command().clone(),
                &item_context,
                mode,
                expected_control_id,
            );
            let execute_and_check_eof = async {
                let command_action = command_execution.await;
                let eof_action = control_queue_eof_action(control_identity);
                (command_action, eof_action)
            };
            let (command_action, eof_action) = match expected_control_id {
                Some(_) => {
                    with_control_queue_identity(
                        control_identity.expect("control queues capture a client identity"),
                        execute_and_check_eof,
                    )
                    .await
                }
                None => execute_and_check_eof.await,
            };
            if eof_action == ControlQueueEofAction::StopFrame {
                break 'command_queue;
            }
            let response_frame_completed = command_action
                .as_ref()
                .is_ok_and(|execution| execution.response_frame_completed);
            let stdout_after_frame = command_action
                .as_ref()
                .is_ok_and(|execution| execution.stdout_after_frame);
            let error_after_frame = command_action
                .as_ref()
                .is_ok_and(|execution| execution.error_after_frame);
            child_guard_stream_started |= response_frame_completed;
            let mut command_action = command_action.map(|execution| {
                if let Some(target) = execution.attached_switch_target {
                    for context in &mut contexts {
                        context.rebase_current_target_after_attached_switch(target.clone());
                    }
                }
                if let Some(rename) = execution.session_rename {
                    rename.apply(context);
                    for context in &mut contexts {
                        rename.apply(context);
                    }
                }
                if let Some(transition) = execution.current_session_transition {
                    transition.apply(context);
                    for context in &mut contexts {
                        transition.apply(context);
                    }
                }
                execution.action
            });
            // Admit insertions and output before publishing bytes or closing
            // the owner guard; a refusal must terminate this entire queue.
            let insertion_error = if let Ok(QueueCommandAction::InsertAfter { batches, .. }) =
                &command_action
            {
                inserted_command_count = batches
                    .iter()
                    .fold(inserted_command_count, |count, (commands, _)| {
                        count.saturating_add(parsed_command_count(commands))
                    });
                (mode == QueueMode::Control
                    && inserted_command_count > CONTROL_QUEUE_INSERTED_COMMAND_LIMIT)
                    .then(|| RmuxError::Server(format!(
                        "control command queue inserted too many commands: {inserted_command_count} (maximum {CONTROL_QUEUE_INSERTED_COMMAND_LIMIT})"
                    )))
            } else {
                None
            };
            let queue_error = insertion_error.or_else(|| match &mut command_action {
                Ok(
                    QueueCommandAction::Normal { output, .. }
                    | QueueCommandAction::InsertAfter { output, .. },
                ) => output.as_mut().and_then(|output| {
                    super::diagnostics::aggregate_queue_output(&mut stdout, output, mode).err()
                }),
                Err(_) => None,
            });
            if let Some(error) = &queue_error {
                command_action = Err(error.clone());
            }
            if response_stream_is_active
                && !response_frame_completed
                && (stdout_after_frame
                    || error_after_frame
                    || child_guard_stream_started
                    || (!queue.is_empty() && control_command_response_has_individual_guards())
                    || command_action.as_ref().is_ok_and(|action| {
                        let QueueCommandAction::InsertAfter { batches, .. } = action else {
                            return false;
                        };
                        batches
                            .iter()
                            .any(|(_, context)| context.control_queue_origin().is_some())
                    }))
            {
                let (mut frame_stdout, mut error) = control_command_frame_outcome(&command_action);
                // Keep admission errors inside their guard, and apply the same
                // stdout bound before publishing either part of the response.
                let trailing_stdout = if error_after_frame && queue_error.is_none() {
                    error.take().map_or_else(Vec::new, |error| {
                        format!("{}\n", error.tmux_message()).into_bytes()
                    })
                } else if stdout_after_frame && error.is_none() {
                    std::mem::take(&mut frame_stdout)
                } else {
                    Vec::new()
                };
                if stdout_after_frame && !child_guard_stream_started && error.is_none() {
                    // An internal initial batch can already have accumulated
                    // ordinary output without starting individual child guards.
                    frame_stdout = stdout[..stdout.len() - trailing_stdout.len()].to_vec();
                }
                if error_after_frame && !child_guard_stream_started && queue_error.is_none() {
                    frame_stdout = stdout.clone();
                }
                if !send_control_command_response_event(
                    control_identity.expect("response streams have a control identity"),
                    ControlCommandResponseEvent::FrameCompleted {
                        stdout: frame_stdout,
                        error,
                        trailing_stdout,
                    },
                ) {
                    break 'command_queue;
                }
                child_guard_stream_started = true;
            }
            if let Some(error) = queue_error {
                execution_errors.push(QueueExecutionError::Runtime(error.clone()));
                errors.push(error);
                exit_status = Some(1);
                break 'command_queue;
            }
            match command_action {
                Ok(QueueCommandAction::Normal {
                    error,
                    source_file_error,
                    exit_status: action_exit_status,
                    ..
                }) => {
                    if let Some(status) = action_exit_status {
                        exit_status = Some(status);
                    }
                    if let Some(error) = source_file_error {
                        source_file_errors.push(error.clone());
                        errors.push(error);
                    }
                    if let Some(error) = error {
                        execution_errors.push(if error_after_frame {
                            QueueExecutionError::CallbackParse(error.clone())
                        } else {
                            QueueExecutionError::Runtime(error.clone())
                        });
                        errors.push(error);
                    }
                }
                Ok(QueueCommandAction::InsertAfter {
                    batches,
                    error,
                    source_file_error,
                    exit_status: action_exit_status,
                    ..
                }) => {
                    if let Err(error) = validate_expected_attach_identity(self, requester_pid).await
                    {
                        execution_errors.push(QueueExecutionError::Runtime(error.clone()));
                        errors.push(error);
                        exit_status = Some(1);
                        break 'command_queue;
                    }
                    if let Some(status) = action_exit_status {
                        exit_status = Some(status);
                    }
                    if let Some(error) = source_file_error {
                        source_file_errors.push(error.clone());
                        errors.push(error);
                    }
                    if let Some(error) = error {
                        execution_errors.push(QueueExecutionError::Runtime(error.clone()));
                        errors.push(error);
                    }
                    let mut insertable_batches = Vec::with_capacity(batches.len());
                    for (commands, context) in batches {
                        if let Err(error) = self
                            .apply_parse_time_assignments(
                                requester_pid,
                                &commands,
                                expected_control_id,
                            )
                            .await
                        {
                            execution_errors.push(QueueExecutionError::Runtime(error.clone()));
                            errors.push(error);
                            exit_status = Some(1);
                            continue;
                        }
                        insertable_batches.push((commands, context));
                    }
                    for (commands, context) in insertable_batches.into_iter().rev() {
                        let inserted = commands.commands().len();
                        queue.insert_after_current(commands);
                        for _ in 0..inserted {
                            contexts.push_front(context.clone());
                        }
                    }
                }
                Err(error) => {
                    execution_errors.push(QueueExecutionError::Runtime(error.clone()));
                    errors.push(error);
                    remove_group_contexts(&queue, &mut contexts, item.group());
                    queue.remove_group(item.group());
                }
            }
            // A popup wait that ended for a shutdown stops its request: tmux runs nothing after it.
            if crate::listener::shutdown_answer::request_stopping_for_shutdown() {
                break 'command_queue;
            }
            let next_consumes_implicit_source_target = {
                let state = self.state.lock().await;
                let marked_target = state.marked_pane_target();
                queue
                    .items()
                    .front()
                    .zip(contexts.front())
                    .is_some_and(|(next, context)| {
                        context.can_refresh_implicit_source_target()
                            && !command_has_independent_target(
                                next.command(),
                                marked_target.as_ref(),
                            )
                    })
            };
            if next_consumes_implicit_source_target {
                let refreshed_target = match control_identity {
                    Some(identity) => {
                        with_control_queue_identity(
                            identity,
                            self.implicit_source_file_target(requester_pid),
                        )
                        .await
                    }
                    None => self.implicit_source_file_target(requester_pid).await,
                };
                let updated_target = match refreshed_target {
                    Ok(target) => target.map(Target::Pane),
                    Err(error) => {
                        execution_errors.push(QueueExecutionError::Runtime(error.clone()));
                        errors.push(error);
                        exit_status = Some(1);
                        break 'command_queue;
                    }
                };
                let next_context = contexts
                    .front_mut()
                    .expect("next queue item must retain its source context");
                *next_context = next_context
                    .clone()
                    .refresh_implicit_current_target(updated_target);
            }
            if self.request_shutdown_if_pending() {
                server_shutdown_started = true;
                break 'command_queue;
            }
        }

        ControlCommandResult {
            stdout,
            error: aggregate_rmux_errors(errors),
            source_file_error: aggregate_rmux_errors(source_file_errors),
            execution_errors,
            exit_status,
            server_shutdown_started,
        }
    }
}
