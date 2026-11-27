use super::command_args::command_arguments_as_strings;
use super::session_parse::parse_attach_session_options;
use super::tokens::{
    normalize_compact_short_options, normalize_repeated_target_options, CommandTokens,
};
use super::{
    command_has_independent_target, current_control_queue_identity, new_window_runtime,
    parse_queue_invocation, queue_special_target, queue_target_find_context,
    queue_transfer_identity, requester_environment_context, ParsedCommand, QueueExactTargetCapture,
    QueueExecutionContext, QueueInvocation, QueueLifecycleTargetCapture, QueueLifecycleTargetPlan,
    QueueMode, QueueSpecialTargetPlan, QueueTargetFindContextInput, QueuedSessionRename, Request,
    RequestHandler, RmuxError, Target,
};

pub(super) struct QueuedCommandAdmission {
    pub(super) invocation: QueueInvocation,
    pub(super) send_keys_client: Option<crate::handler::attach_support::ActiveAttachIdentity>,
    pub(super) target_identities: Vec<crate::handler::StableTargetIdentity>,
    pub(super) retained_lifecycle_target:
        Option<std::sync::Arc<crate::handler::lifecycle_support::LifecycleTargetLease>>,
    pub(super) retained_lifecycle_identity: Option<crate::handler::StableTargetIdentity>,
    pub(super) special_target_binding: Option<Box<queue_special_target::QueueSpecialTargetBinding>>,
    pub(super) session_rename: Option<QueuedSessionRename>,
}

impl RequestHandler {
    pub(super) async fn admit_queued_command(
        &self,
        requester_pid: u32,
        command_and_hooks: (ParsedCommand, &ParsedCommand),
        attached_session: Option<&rmux_proto::SessionName>,
        context: &QueueExecutionContext,
        mode: QueueMode,
        stdin_payload: Option<Vec<u8>>,
    ) -> Result<QueuedCommandAdmission, RmuxError> {
        let (command, command_for_hooks) = command_and_hooks;
        let requester_owns_attached_session =
            current_control_queue_identity(requester_pid).is_some();
        // tmux takes send-keys' default target from the queue item's own
        // client; `-c` only chooses the client the keys are judged for.
        let send_keys_session = if command_for_hooks.name() == "send-keys" {
            self.requester_current_session(requester_pid).await
        } else {
            None
        };
        let socket_path = self.socket_path();
        let requester_environment = if context.current_target.is_none()
            && !command_has_independent_target(command_for_hooks, None)
        {
            requester_environment_context(requester_pid, &socket_path)
        } else {
            Default::default()
        };
        {
            let mut state = self.state.lock().await;
            let active_attach = if command_for_hooks.name() == "send-keys" {
                Some(self.active_attach.lock().await)
            } else {
                None
            };
            (|| -> Result<_, RmuxError> {
                context.require_pinned_current_target(&state)?;
                // Match direct attach: after syntax validation, an empty server
                // takes precedence over target resolution and terminal checks.
                if command.name() == "attach-session" && state.sessions.is_empty() {
                    let command = normalize_repeated_target_options(command);
                    let arguments =
                        command_arguments_as_strings(command.name(), command.arguments())?;
                    let arguments = normalize_compact_short_options(command.name(), arguments);
                    parse_attach_session_options(CommandTokens::new(arguments))?;
                    return Err(RmuxError::Server("no sessions".to_owned()));
                }
                let retained_target = context.retained_lifecycle_target().cloned();
                let lifecycle_plan = retained_target
                    .as_ref()
                    .map(|_| QueueLifecycleTargetPlan::for_command(command_for_hooks))
                    .transpose()?
                    .flatten();
                let special_target_plan = QueueSpecialTargetPlan::for_command(command_for_hooks)?;
                let allow_historical_retired_work = context.allows_historical_retired_target_work();
                let retained_parse_target = match (&lifecycle_plan, retained_target.as_ref()) {
                    (Some(plan), Some(retained_target)) => {
                        plan.resolve_parse_target(retained_target, &state)?
                    }
                    _ => None,
                };
                let special_retained_parse_target = match special_target_plan.as_ref() {
                    Some(plan) => plan.resolve_parse_target(
                        retained_target.as_ref(),
                        &state,
                        allow_historical_retired_work,
                    )?,
                    None => None,
                };
                let current_target = if lifecycle_plan.is_some() {
                    retained_parse_target.as_ref()
                } else if special_retained_parse_target.is_some() {
                    special_retained_parse_target.as_ref()
                } else {
                    context.current_target.as_ref()
                };
                let marked_target = state.marked_pane_target();
                let requester_environment =
                    if command_has_independent_target(command_for_hooks, marked_target.as_ref()) {
                        Default::default()
                    } else {
                        requester_environment
                    };
                let historical_retired = allow_historical_retired_work
                    && retained_target.as_ref().is_some_and(|target| {
                        matches!(
                            target.resolve(&state),
                            super::super::lifecycle_support::LeaseResolution::Retired(_)
                        )
                    });
                let (command, find_context) = if historical_retired {
                    super::targets::admit_historical_target_roles(command, &state, context)?
                } else {
                    (
                        command,
                        queue_target_find_context(QueueTargetFindContextInput {
                            sessions: &state.sessions,
                            options: &state.options,
                            requester_environment,
                            attached_session,
                            requester_owns_attached_session,
                            current_target,
                            current_target_follows_client: context.current_target_follows_client(),
                            missing_current_target_fallback: context
                                .missing_current_target_fallback(),
                            mouse_target: context.mouse_target.as_ref(),
                            marked_target: marked_target.as_ref(),
                        })?,
                    )
                };
                let run_shell_canfail_fallback_target =
                    context.run_shell_canfail_fallback_target().or_else(|| {
                        (mode == QueueMode::Control)
                            .then(|| find_context.current())
                            .flatten()
                    });
                let queue_current_target = if lifecycle_plan.is_some() {
                    retained_parse_target
                        .as_ref()
                        .or(special_retained_parse_target.as_ref())
                } else {
                    special_retained_parse_target
                        .as_ref()
                        .or(retained_parse_target.as_ref())
                        .or(context.canfail_fallback_target())
                };
                let mut invocation = parse_queue_invocation(
                    command,
                    context.caller_cwd.as_deref(),
                    &state.sessions,
                    &state.options,
                    &find_context,
                    queue_current_target,
                    run_shell_canfail_fallback_target,
                )?;
                if let Some(payload) = stdin_payload {
                    // Installed under the lock that admitted the command and
                    // before any identity is captured, so the commit still
                    // revalidates everything it validated before. Each offer
                    // reaches only its own command: the split offer admits
                    // nothing but `split-window`, the new-pane input offer
                    // nothing but `new-pane`.
                    match &mut invocation {
                        QueueInvocation::SplitWindow(command) => {
                            command.install_stdin_payload(payload)?;
                        }
                        QueueInvocation::NewPane(command) => {
                            command.install_stdin_payload(payload)?;
                        }
                        _ => {
                            return Err(RmuxError::Server(
                                "input requires a single split or new-pane command".to_owned(),
                            ))
                        }
                    }
                }
                if let Some(plan) = lifecycle_plan.as_ref() {
                    plan.bind_implicit_target(&mut invocation, &state.sessions, &find_context)?;
                }
                let special_target_pending = match special_target_plan {
                    Some(plan) => plan.bind(
                        &mut invocation,
                        &state,
                        &find_context,
                        retained_target.clone(),
                        allow_historical_retired_work,
                    )?,
                    None => None,
                };
                if context.uses_explicit_current_target()
                    && !crate::hook_runtime::lifecycle_hook_lacks_notification_state()
                {
                    QueueExactTargetCapture::bind_queue_current(
                        &mut invocation,
                        &state.sessions,
                        &find_context,
                    )?;
                }
                drop(find_context);
                let send_keys_client = active_attach
                    .as_ref()
                    .map(|active_attach| {
                        super::queued_send_keys_client::bind_client(
                            &mut invocation,
                            active_attach,
                            requester_pid,
                        )
                    })
                    .transpose()?
                    .flatten();
                QueueExactTargetCapture::bind(&mut invocation, &state, send_keys_session.as_ref())?;
                let special_target_binding = special_target_pending
                    .map(|pending| pending.capture(&mut state))
                    .transpose()?
                    .map(Box::new);
                if let QueueInvocation::NewWindow(command) = &mut invocation {
                    let witness = new_window_runtime::QueuedNewWindowTargetWitness::capture(
                        &mut state, command,
                    )?;
                    command.target_witness = Some(Box::new(witness));
                }
                if let QueueInvocation::SetPanePlacement(command) = &mut invocation {
                    command.capture_witness(&state)?;
                }
                let session_rename = QueuedSessionRename::capture(&invocation, &state)?;
                let exact_target =
                    QueueExactTargetCapture::capture(command_for_hooks, &invocation, &mut state)
                        .into_identity()?;
                let lifecycle_target = match (lifecycle_plan, retained_target) {
                    (Some(plan), Some(retained_target)) => {
                        plan.capture(&invocation, &mut state, retained_target)?
                    }
                    _ => QueueLifecycleTargetCapture::default(),
                };
                let (mut target_identities, retained_lifecycle_target, retained_lifecycle_identity) =
                    lifecycle_target.into_parts();
                if let Some(exact_target) = exact_target {
                    target_identities.push(exact_target);
                }
                // `join-pane`/`move-pane` name two panes and then wait. Capture
                // both identities here, under the lock that resolved them, for
                // every queue and whatever the flags were: an ordinary queue
                // holds no lifecycle lease, and that is the path the race
                // reaches. The handlers refuse under the lock just before the
                // mutation if either slot was replaced meanwhile.
                if let Some((source, target)) =
                    queue_transfer_identity::queue_transfer_targets(&invocation)
                {
                    for pane_target in [source.clone(), target.clone()] {
                        let identity = crate::handler::StableTargetIdentity::capture(
                            &mut state,
                            Target::Pane(pane_target),
                        )?;
                        if !target_identities.contains(&identity) {
                            target_identities.push(identity);
                        }
                    }
                }
                if let QueueInvocation::MoveFloatingPane(command) = &invocation {
                    target_identities.push(crate::handler::StableTargetIdentity::capture(
                        &mut state,
                        Target::Pane(command.target.clone()),
                    )?);
                }
                if let QueueInvocation::SetPanePlacement(command) = &invocation {
                    for target in command.stable_targets() {
                        target_identities.push(crate::handler::StableTargetIdentity::capture(
                            &mut state, target,
                        )?);
                    }
                }
                if let QueueInvocation::SplitWindow(split) = &invocation {
                    if split.exit_policy
                        == Some(crate::pane_terminals::SplitExitPolicyOverride::Key)
                    {
                        if let Request::SplitWindowExt(request) = &split.request {
                            let target = match &request.target {
                                rmux_proto::SplitWindowTarget::Pane(target) => {
                                    Target::Pane(target.clone())
                                }
                                rmux_proto::SplitWindowTarget::Session(session) => {
                                    Target::Session(session.clone())
                                }
                            };
                            target_identities.push(crate::handler::StableTargetIdentity::capture(
                                &mut state, target,
                            )?);
                        }
                    }
                }
                target_identities.dedup();
                Ok(QueuedCommandAdmission {
                    invocation,
                    send_keys_client,
                    target_identities,
                    retained_lifecycle_target,
                    retained_lifecycle_identity,
                    special_target_binding,
                    session_rename,
                })
            })()
        }
    }
}
