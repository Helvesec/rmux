use super::{
    command_has_independent_target, parse_queue_invocation, queue_target_find_context,
    requester_environment_context, ActiveAttachIdentity, ParsedCommands, QueueExecutionContext,
    QueueInvocation, QueueTargetFindContextInput, RequestHandler, RmuxError,
};

impl RequestHandler {
    pub(in crate::handler) async fn start_attached_prompt_binding_commands(
        &self,
        requester_pid: u32,
        commands: &ParsedCommands,
        context: &QueueExecutionContext,
    ) -> Result<bool, RmuxError> {
        self.start_attached_prompt_binding_commands_with_identity(
            requester_pid,
            None,
            commands,
            context,
        )
        .await
    }

    pub(in crate::handler) async fn start_attached_prompt_binding_commands_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        session_name: rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        requester_pid: u32,
        commands: &ParsedCommands,
        context: &QueueExecutionContext,
    ) -> Result<bool, RmuxError> {
        self.start_attached_prompt_binding_commands_with_identity(
            requester_pid,
            Some((identity, session_name, session_id)),
            commands,
            context,
        )
        .await
    }

    pub(super) async fn start_attached_prompt_binding_commands_with_identity(
        &self,
        requester_pid: u32,
        identity: Option<(
            ActiveAttachIdentity,
            rmux_proto::SessionName,
            rmux_proto::SessionId,
        )>,
        commands: &ParsedCommands,
        context: &QueueExecutionContext,
    ) -> Result<bool, RmuxError> {
        if commands.commands().len() != 1 {
            return Ok(false);
        }

        if identity
            .as_ref()
            .is_some_and(|(identity, _, _)| identity.attach_pid() != requester_pid)
        {
            return Err(RmuxError::Server(
                "attached prompt identity changed client".to_owned(),
            ));
        }

        self.apply_parse_time_assignments(requester_pid, commands, None)
            .await?;
        let command = commands
            .commands()
            .first()
            .expect("single command checked")
            .clone();
        let attached_session = match identity.as_ref() {
            Some((identity, session_name, _)) => {
                if !self.current_live_attach_input(*identity).await {
                    return Err(RmuxError::Server("attached client disappeared".to_owned()));
                }
                Some(session_name.clone())
            }
            None => self.current_session_candidate(requester_pid).await,
        };
        let socket_path = self.socket_path();
        let requester_environment = if context.current_target.is_none()
            && !command_has_independent_target(&command, None)
        {
            requester_environment_context(requester_pid, &socket_path)
        } else {
            Default::default()
        };
        let invocation = {
            let state = self.state.lock().await;
            if identity
                .as_ref()
                .is_some_and(|(_, session_name, session_id)| {
                    state
                        .sessions
                        .session(session_name)
                        .is_none_or(|session| session.id() != *session_id)
                })
            {
                return Err(RmuxError::Server("attached session disappeared".to_owned()));
            }
            let marked_target = state.marked_pane_target();
            let requester_environment =
                if command_has_independent_target(&command, marked_target.as_ref()) {
                    Default::default()
                } else {
                    requester_environment
                };
            let find_context = queue_target_find_context(QueueTargetFindContextInput {
                sessions: &state.sessions,
                options: &state.options,
                requester_environment,
                attached_session: attached_session.as_ref(),
                requester_owns_attached_session: identity.is_some(),
                current_target: context.current_target.as_ref(),
                current_target_follows_client: context.current_target_follows_client(),
                missing_current_target_fallback: context.missing_current_target_fallback(),
                mouse_target: context.mouse_target.as_ref(),
                marked_target: marked_target.as_ref(),
            })?;
            let run_shell_canfail_fallback_target = context.run_shell_canfail_fallback_target();
            parse_queue_invocation(
                command,
                context.caller_cwd.as_deref(),
                &state.sessions,
                &state.options,
                &find_context,
                context.canfail_fallback_target(),
                run_shell_canfail_fallback_target,
            )
        }?;

        match invocation {
            QueueInvocation::CommandPrompt(command) => {
                match identity {
                    Some((identity, session_name, session_id)) => {
                        self.start_attached_command_prompt_binding_for_identity(
                            identity,
                            session_name,
                            session_id,
                            requester_pid,
                            command,
                            context,
                        )
                        .await?;
                    }
                    None => {
                        self.start_attached_command_prompt_binding(requester_pid, command, context)
                            .await?;
                    }
                }
                Ok(true)
            }
            QueueInvocation::ConfirmBefore(command) => {
                match identity {
                    Some((identity, session_name, session_id)) => {
                        self.start_attached_confirm_before_binding_for_identity(
                            identity,
                            session_name,
                            session_id,
                            requester_pid,
                            command,
                            context,
                        )
                        .await?;
                    }
                    None => {
                        self.start_attached_confirm_before_binding(requester_pid, command, context)
                            .await?;
                    }
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}
