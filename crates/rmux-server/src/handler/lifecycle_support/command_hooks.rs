use super::*;

impl RequestHandler {
    pub(in crate::handler) fn queue_inline_hook(
        &self,
        hook: HookName,
        scope: ScopeSelector,
        current_target: Option<Target>,
        format_mode: PendingInlineHookFormat,
    ) {
        queue_inline_hook(PendingInlineHook {
            hook,
            scope,
            current_target,
            exact_pane_target: None,
            exact_session_id: None,
            skip_dispatch: false,
            format_mode,
        });
    }

    pub(in crate::handler) fn queue_exact_pane_inline_hook(
        &self,
        hook: HookName,
        target: PaneTarget,
        identity: ExactPaneHookTarget,
        format_mode: PendingInlineHookFormat,
    ) {
        queue_inline_hook(PendingInlineHook {
            hook,
            scope: ScopeSelector::Pane(target.clone()),
            current_target: Some(Target::Pane(target)),
            exact_pane_target: Some(identity),
            exact_session_id: None,
            skip_dispatch: false,
            format_mode,
        });
    }

    pub(in crate::handler) fn queue_exact_session_inline_hook(
        &self,
        hook: HookName,
        session_name: rmux_proto::SessionName,
        session_id: SessionId,
        current_target: Option<Target>,
        format_mode: PendingInlineHookFormat,
    ) {
        queue_inline_hook(PendingInlineHook {
            hook,
            scope: ScopeSelector::Session(session_name),
            current_target,
            exact_pane_target: None,
            exact_session_id: Some(session_id),
            skip_dispatch: false,
            format_mode,
        });
    }

    pub(in crate::handler) fn queue_missing_target_inline_hook(
        &self,
        hook: HookName,
        format_mode: PendingInlineHookFormat,
    ) {
        self.queue_suppressed_inline_hook(hook, format_mode);
    }

    pub(in crate::handler) fn queue_suppressed_inline_hook(
        &self,
        hook: HookName,
        format_mode: PendingInlineHookFormat,
    ) {
        queue_inline_hook(PendingInlineHook {
            hook,
            scope: ScopeSelector::Global,
            current_target: None,
            exact_pane_target: None,
            exact_session_id: None,
            skip_dispatch: true,
            format_mode,
        });
    }

    pub(in crate::handler) async fn run_inline_hooks(
        &self,
        requester_pid: u32,
        inline_hooks: Vec<PendingInlineHook>,
        parsed_command: Option<&ParsedCommand>,
    ) {
        for pending in inline_hooks {
            if pending.skip_dispatch {
                continue;
            }
            let (hook, format_mode) = (pending.hook, pending.format_mode);
            let formats = move || match format_mode {
                PendingInlineHookFormat::HookOnly => hook_only_format_values(hook),
                PendingInlineHookFormat::AfterCommand => {
                    after_hook_format_values(hook, parsed_command)
                }
            };
            let current_target = match (pending.exact_pane_target, pending.exact_session_id) {
                (Some(identity), None) => HookDispatchCurrentTarget::ExactPane(identity),
                (None, Some(session_id)) => HookDispatchCurrentTarget::ExactSession(session_id),
                (None, None) => HookDispatchCurrentTarget::Dynamic(pending.current_target),
                (Some(_), Some(_)) => {
                    unreachable!("inline hook cannot carry pane and session identities")
                }
            };
            self.run_built_in_hook_dispatch_with_target(
                requester_pid,
                pending.hook,
                pending.scope,
                current_target,
                formats,
                "inline",
            )
            .await;
        }
    }

    pub(in crate::handler) async fn run_request_hooks(
        &self,
        requester_pid: u32,
        request: &Request,
        response: &Response,
        parsed_command: Option<&ParsedCommand>,
        suppressed_success_hooks: &[HookName],
    ) {
        if hooks_disabled() {
            return;
        }

        let current_target = self
            .current_target_for_request_response(requester_pid, request, response)
            .await;
        let scope = current_target
            .as_ref()
            .map(target_to_scope)
            .unwrap_or(ScopeSelector::Global);

        if matches!(response, Response::Error(_)) {
            self.run_built_in_hook_dispatch(
                requester_pid,
                HookName::CommandError,
                scope,
                current_target,
                || after_hook_format_values(HookName::CommandError, parsed_command),
                "command-error",
            )
            .await;
            return;
        }

        let hook_name = format!("after-{}", request.command_name());
        let Some(hook) = HookName::from_str(&hook_name) else {
            return;
        };
        if suppressed_success_hooks.contains(&hook) {
            return;
        }
        self.run_built_in_hook_dispatch(
            requester_pid,
            hook,
            scope,
            current_target,
            || after_hook_format_values(hook, parsed_command),
            "after",
        )
        .await;
    }

    pub(in crate::handler) async fn run_command_error_hook_for_parsed_command(
        &self,
        requester_pid: u32,
        command: &ParsedCommand,
        current_target: Option<Target>,
        attached_session: Option<&rmux_proto::SessionName>,
    ) {
        if hooks_disabled() {
            return;
        }

        let current_target = if current_target.is_some() {
            current_target
        } else {
            let state = self.state.lock().await;
            fallback_current_target(&state, attached_session)
        };
        let scope = current_target
            .as_ref()
            .map(target_to_scope)
            .unwrap_or(ScopeSelector::Global);
        self.run_built_in_hook_dispatch(
            requester_pid,
            HookName::CommandError,
            scope,
            current_target,
            || after_hook_format_values(HookName::CommandError, Some(command)),
            "command-error",
        )
        .await;
    }

    pub(in crate::handler) async fn run_command_success_hook_for_parsed_command(
        &self,
        requester_pid: u32,
        hook: HookName,
        command: &ParsedCommand,
        current_target: Option<Target>,
        attached_session: Option<&rmux_proto::SessionName>,
    ) {
        if hooks_disabled() {
            return;
        }

        let current_target = if current_target.is_some() {
            current_target
        } else {
            let state = self.state.lock().await;
            fallback_current_target(&state, attached_session)
        };
        let scope = current_target
            .as_ref()
            .map(target_to_scope)
            .unwrap_or(ScopeSelector::Global);
        self.run_built_in_hook_dispatch(
            requester_pid,
            hook,
            scope,
            current_target,
            || after_hook_format_values(hook, Some(command)),
            "after",
        )
        .await;
    }

    pub(super) async fn run_built_in_hook_dispatch(
        &self,
        requester_pid: u32,
        hook_name: HookName,
        scope: ScopeSelector,
        current_target: Option<Target>,
        formats: impl FnOnce() -> Vec<(String, String)>,
        source: &'static str,
    ) {
        self.run_built_in_hook_dispatch_with_target(
            requester_pid,
            hook_name,
            scope,
            HookDispatchCurrentTarget::Dynamic(current_target),
            formats,
            source,
        )
        .await;
    }

    pub(super) async fn run_built_in_hook_dispatch_with_target(
        &self,
        requester_pid: u32,
        hook_name: HookName,
        scope: ScopeSelector,
        current_target: HookDispatchCurrentTarget,
        formats: impl FnOnce() -> Vec<(String, String)>,
        source: &'static str,
    ) {
        if hooks_disabled() {
            return;
        }

        let hooks = {
            let mut state = self.state.lock().await;
            let scope = match &current_target {
                HookDispatchCurrentTarget::ExactPane(identity) => {
                    let Some(target) = exact_pane_hook_target(&state, *identity) else {
                        return;
                    };
                    target_to_scope(&target)
                }
                HookDispatchCurrentTarget::ExactSession(session_id) => {
                    let Some(target) = resolve_stable_session_target(&state, *session_id) else {
                        return;
                    };
                    target_to_scope(&target)
                }
                _ => scope,
            };
            match super::super::resolve_hook_scope_identity(&state, &scope) {
                Ok(identity) => state
                    .hooks
                    .dispatch_with_identity_or_scope(&identity, &scope, hook_name),
                Err(_) => state.hooks.dispatch(&scope, hook_name),
            }
        };
        if hooks.is_empty() {
            return;
        }

        // Rendering after-hook arguments escapes every argument; do it only
        // when a hook will read the values.
        self.execute_hook_dispatches(
            requester_pid,
            hooks,
            current_target,
            formats(),
            HookExecutionContext::command(hook_name),
            source,
        )
        .await;
    }
}
