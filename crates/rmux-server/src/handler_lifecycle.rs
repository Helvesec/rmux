use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use rmux_core::{
    command_parser::{CommandArgument, ParsedCommand},
    HookDispatch, LifecycleEvent,
};
use rmux_proto::{HookName, PaneTarget, Request, Response, ScopeSelector, Target, WindowTarget};
use tokio::sync::{broadcast, watch};
use tracing::warn;

use crate::hook_runtime::{
    hooks_disabled, queue_inline_hook, with_hook_execution, PendingInlineHook,
    PendingInlineHookFormat,
};

use super::{
    active_session_target, active_window_target, fallback_current_target,
    target_for_request_response, target_to_scope, RequestHandler,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueuedLifecycleEvent {
    pub(in crate::handler) event: LifecycleEvent,
    pub(in crate::handler) hook_name: HookName,
    pub(in crate::handler) hooks: Vec<HookDispatch>,
    pub(in crate::handler) formats: Vec<(String, String)>,
    pub(in crate::handler) current_target: Option<Target>,
}

impl RequestHandler {
    pub(crate) fn subscribe_lifecycle_events(&self) -> broadcast::Receiver<QueuedLifecycleEvent> {
        self.hook_events.subscribe()
    }

    pub(crate) async fn consume_lifecycle_hooks(
        &self,
        mut events: broadcast::Receiver<QueuedLifecycleEvent>,
        mut shutdown: watch::Receiver<()>,
    ) {
        loop {
            tokio::select! {
                result = events.recv() => {
                    match result {
                        Ok(event) => self.dispatch_lifecycle_hook(event).await,
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(skipped, "lifecycle hook consumer lagged; dropping events");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                result = shutdown.changed() => {
                    let _ = result;
                    self.shutdown_wait_for();
                    break;
                }
            }
        }
    }

    pub(in crate::handler) async fn emit(&self, event: LifecycleEvent) {
        if let LifecycleEvent::PaneModeChanged { target } = &event {
            self.refresh_automatic_window_name_for_pane_target(target)
                .await;
        }
        if hooks_disabled() {
            return;
        }
        let queued = {
            let mut state = self.state.lock().await;
            prepare_lifecycle_event(&mut state, &event)
        };
        self.emit_prepared(queued);
    }

    pub(in crate::handler) async fn emit_without_attached_refresh(&self, event: LifecycleEvent) {
        if hooks_disabled() {
            return;
        }
        let queued = {
            let mut state = self.state.lock().await;
            prepare_lifecycle_event(&mut state, &event)
        };
        self.emit_prepared(queued);
    }

    pub(in crate::handler) fn emit_prepared(&self, event: QueuedLifecycleEvent) {
        if hooks_disabled() {
            return;
        }
        let _ = self.hook_events.send(event);
    }

    pub(in crate::handler) fn shutdown_wait_for(&self) {
        if let Ok(mut wait_for) = self.wait_for.lock() {
            wait_for.shutdown();
        }
    }

    pub(crate) async fn emit_client_attached(
        &self,
        requester_pid: u32,
        session_name: rmux_proto::SessionName,
    ) {
        self.emit(LifecycleEvent::ClientAttached {
            session_name,
            client_name: Some(requester_pid.to_string()),
        })
        .await;
    }

    pub(in crate::handler) async fn dispatch_lifecycle_hook(&self, event: QueuedLifecycleEvent) {
        self.dispatch_control_notifications(&event.event).await;
        self.refresh_control_sessions_for_event(&event.event).await;

        if event.hooks.is_empty() {
            return;
        }
        let current_target = self.lifecycle_dispatch_current_target(&event).await;

        self.execute_hook_dispatches(
            std::process::id(),
            event.hooks,
            current_target,
            event.formats,
            event.hook_name,
            "lifecycle",
        )
        .await;
    }

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
            let formats = match pending.format_mode {
                PendingInlineHookFormat::HookOnly => hook_only_format_values(pending.hook),
                PendingInlineHookFormat::AfterCommand => {
                    after_hook_format_values(pending.hook, parsed_command)
                }
            };
            self.run_built_in_hook_dispatch(
                requester_pid,
                pending.hook,
                pending.scope,
                pending.current_target,
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
                after_hook_format_values(HookName::CommandError, parsed_command),
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
            after_hook_format_values(hook, parsed_command),
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
            after_hook_format_values(HookName::CommandError, Some(command)),
            "command-error",
        )
        .await;
    }

    async fn run_built_in_hook_dispatch(
        &self,
        requester_pid: u32,
        hook_name: HookName,
        scope: ScopeSelector,
        current_target: Option<Target>,
        formats: Vec<(String, String)>,
        source: &'static str,
    ) {
        if hooks_disabled() {
            return;
        }

        let hooks = {
            let mut state = self.state.lock().await;
            state.hooks.dispatch(&scope, hook_name)
        };
        if hooks.is_empty() {
            return;
        }

        self.execute_hook_dispatches(
            requester_pid,
            hooks,
            current_target,
            formats,
            hook_name,
            source,
        )
        .await;
    }

    fn execute_hook_dispatches(
        &self,
        requester_pid: u32,
        hooks: Vec<HookDispatch>,
        current_target: Option<Target>,
        formats: Vec<(String, String)>,
        hook_name: HookName,
        source: &'static str,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            with_hook_execution(formats, async {
                for hook in hooks {
                    let command_current_target =
                        self.valid_hook_command_target(current_target.clone()).await;
                    if let Err(error) = self
                        .execute_hook_command_with_context(
                            requester_pid,
                            hook.command(),
                            command_current_target,
                        )
                        .await
                    {
                        warn!(hook = ?hook_name, source, "failed to execute hook command: {error}");
                    }
                }
            })
            .await;
        })
    }

    async fn current_target_for_request_response(
        &self,
        requester_pid: u32,
        request: &Request,
        response: &Response,
    ) -> Option<Target> {
        let attached_session = self.current_session_candidate(requester_pid).await;
        let state = self.state.lock().await;
        target_for_request_response(&state, request, response, attached_session.as_ref())
    }

    async fn fallback_target_from_current_hook_formats(&self) -> Option<Target> {
        let formats = crate::hook_runtime::current_hook_formats();
        let state = self.state.lock().await;
        hook_format_session_target(&state, &formats)
    }

    async fn valid_hook_command_target(&self, target: Option<Target>) -> Option<Target> {
        if let Some(target) = target {
            let state = self.state.lock().await;
            if target_exists(&state, &target) {
                return pane_target_for_existing_target(&state, &target).or(Some(target));
            }
        }
        self.fallback_target_from_current_hook_formats().await
    }

    async fn lifecycle_dispatch_current_target(
        &self,
        event: &QueuedLifecycleEvent,
    ) -> Option<Target> {
        let state = self.state.lock().await;
        if let Some(pane_target) = event.event.pane_target().filter(|_| {
            matches!(
                event.event,
                LifecycleEvent::PaneExited { .. } | LifecycleEvent::PaneDied { .. }
            )
        }) {
            if let Some(Target::Pane(target)) = event
                .current_target
                .as_ref()
                .filter(|target| target_exists(&state, target))
            {
                if target != pane_target {
                    return Some(Target::Pane(target.clone()));
                }
            }
            return fallback_session_pane_target_after_exit(&state, pane_target).or_else(|| {
                event
                    .event
                    .session_name()
                    .and_then(|session_name| first_session_pane_target(&state, session_name))
                    .or_else(|| hook_format_session_target(&state, &event.formats))
            });
        }
        if let Some(target) = event
            .current_target
            .as_ref()
            .filter(|target| target_exists(&state, target))
        {
            return Some(target.clone());
        }
        if let Some(pane_target) = event.event.pane_target() {
            return fallback_session_pane_target_after_exit(&state, pane_target);
        }
        event
            .event
            .session_name()
            .and_then(|session_name| {
                active_session_target(&state.sessions, session_name)
                    .or_else(|| first_session_pane_target(&state, session_name))
            })
            .or_else(|| hook_format_session_target(&state, &event.formats))
    }
}

pub(in crate::handler) fn prepare_lifecycle_event(
    state: &mut crate::pane_terminals::HandlerState,
    event: &LifecycleEvent,
) -> QueuedLifecycleEvent {
    let hook_name = event.hook_name();
    let current_target = lifecycle_hook_current_target(state, event);
    let dispatch_scope = lifecycle_hook_dispatch_scope(event, current_target.as_ref());
    QueuedLifecycleEvent {
        event: event.clone(),
        hook_name,
        hooks: state.hooks.dispatch(&dispatch_scope, hook_name),
        formats: lifecycle_hook_formats(state, event),
        current_target,
    }
}

fn lifecycle_hook_dispatch_scope(
    event: &LifecycleEvent,
    current_target: Option<&Target>,
) -> ScopeSelector {
    if matches!(
        event,
        LifecycleEvent::PaneExited { .. } | LifecycleEvent::PaneDied { .. }
    ) {
        if let Some(current_target) = current_target {
            return target_to_scope(current_target);
        }
    }
    event.scope()
}

fn hook_only_format_values(hook: HookName) -> Vec<(String, String)> {
    vec![("hook".to_owned(), hook.to_string())]
}

pub(in crate::handler) fn after_hook_format_values(
    hook: HookName,
    parsed_command: Option<&ParsedCommand>,
) -> Vec<(String, String)> {
    let mut formats = hook_only_format_values(hook);
    let Some(parsed_command) = parsed_command else {
        return formats;
    };

    let arguments = parsed_command
        .arguments()
        .iter()
        .map(CommandArgument::to_tmux_string)
        .collect::<Vec<_>>();
    formats.push(("hook_arguments".to_owned(), arguments.join(" ")));
    for (index, argument) in arguments.iter().enumerate() {
        formats.push((format!("hook_argument_{index}"), argument.clone()));
    }

    let scalar_arguments = parsed_command
        .arguments()
        .iter()
        .filter_map(CommandArgument::as_string)
        .collect::<Vec<_>>();
    let mut flag_values = BTreeMap::<char, Vec<String>>::new();
    let mut index = 0;
    while index < scalar_arguments.len() {
        let token = scalar_arguments[index];
        if token == "--" {
            break;
        }
        let Some(flags) = token.strip_prefix('-') else {
            index += 1;
            continue;
        };
        if flags.is_empty()
            || token.starts_with("--")
            || !flags.chars().all(|flag| flag.is_ascii_alphabetic())
        {
            index += 1;
            continue;
        }

        if flags.len() == 1 {
            let flag = flags.chars().next().expect("single-char flag");
            if let Some(value) = scalar_arguments.get(index + 1).copied() {
                if !value.starts_with('-') {
                    flag_values.entry(flag).or_default().push(value.to_owned());
                    index += 2;
                    continue;
                }
            }
        }

        for flag in flags.chars() {
            let _ = flag_values.entry(flag).or_default();
        }
        index += 1;
    }

    for (flag, values) in flag_values {
        if let Some(value) = values.last() {
            formats.push((format!("hook_flag_{flag}"), value.clone()));
            for (index, value) in values.into_iter().enumerate() {
                formats.push((format!("hook_flag_{flag}_{index}"), value));
            }
        } else {
            formats.push((format!("hook_flag_{flag}"), "1".to_owned()));
        }
    }

    formats
}

fn lifecycle_hook_formats(
    state: &crate::pane_terminals::HandlerState,
    event: &LifecycleEvent,
) -> Vec<(String, String)> {
    let mut formats = hook_only_format_values(event.hook_name());
    if let Some(client_name) = event.client_name() {
        formats.push(("hook_client".to_owned(), client_name.to_owned()));
    }
    if let Some(session_name) = event.session_name() {
        if let Some(session) = state.sessions.session(session_name) {
            formats.push(("hook_session".to_owned(), session.id().to_string()));
            formats.push(("hook_session_name".to_owned(), session.name().to_string()));
        } else {
            if let Some(session_id) = event.session_id() {
                formats.push(("hook_session".to_owned(), format!("${session_id}")));
            }
            formats.push(("hook_session_name".to_owned(), session_name.to_string()));
        }
    }
    if let Some(window_target) = event.window_target() {
        let mut resolved_window = false;
        if let Some(session) = state.sessions.session(window_target.session_name()) {
            if let Some(window) = session.window_at(window_target.window_index()) {
                formats.push(("hook_window".to_owned(), window.id().to_string()));
                formats.push((
                    "hook_window_name".to_owned(),
                    window.name().unwrap_or_default().to_owned(),
                ));
                resolved_window = true;
            }
        }
        if !resolved_window {
            if let Some(window_id) = event.window_id() {
                formats.push(("hook_window".to_owned(), format!("@{window_id}")));
                if let Some(window_name) = event.window_name_snapshot() {
                    formats.push(("hook_window_name".to_owned(), window_name.to_owned()));
                }
            }
        }
    }
    if let Some(pane_target) = event.pane_target() {
        let mut resolved_pane = false;
        if let Some(session) = state.sessions.session(pane_target.session_name()) {
            if let Some(window) = session.window_at(pane_target.window_index()) {
                if let Some(pane) = window.pane(pane_target.pane_index()) {
                    formats.push(("hook_pane".to_owned(), format!("%{}", pane.id().as_u32())));
                    resolved_pane = true;
                }
            }
        }
        if !resolved_pane {
            if let Some(pane_id) = event.pane_id() {
                formats.push(("hook_pane".to_owned(), format!("%{pane_id}")));
            }
        }
    }
    if matches!(
        event,
        LifecycleEvent::PaneExited { .. } | LifecycleEvent::PaneDied { .. }
    ) {
        match lifecycle_hook_current_target(state, event) {
            Some(Target::Pane(target)) => append_pane_format_values(state, &target, &mut formats),
            _ => {
                if let Some(session_name) = event.session_name() {
                    if let Some(Target::Pane(target)) =
                        first_session_pane_target(state, session_name)
                    {
                        append_pane_format_values(state, &target, &mut formats);
                    }
                }
            }
        }
    }
    formats
}

fn append_pane_format_values(
    state: &crate::pane_terminals::HandlerState,
    target: &PaneTarget,
    formats: &mut Vec<(String, String)>,
) {
    let Some(session) = state.sessions.session(target.session_name()) else {
        return;
    };
    let Some(window) = session.window_at(target.window_index()) else {
        return;
    };
    let Some(pane) = window.pane(target.pane_index()) else {
        return;
    };
    formats.push(("window_index".to_owned(), target.window_index().to_string()));
    formats.push(("pane_index".to_owned(), target.pane_index().to_string()));
    formats.push(("pane_id".to_owned(), pane.id().to_string()));
}

fn lifecycle_hook_current_target(
    state: &crate::pane_terminals::HandlerState,
    event: &LifecycleEvent,
) -> Option<Target> {
    match event.current_target() {
        Some(Target::Session(session_name)) => {
            active_session_target(&state.sessions, &session_name)
        }
        Some(Target::Window(target)) => active_window_target(&state.sessions, &target)
            .or_else(|| active_session_target(&state.sessions, target.session_name())),
        Some(Target::Pane(target)) => {
            if matches!(
                event,
                LifecycleEvent::PaneExited { .. } | LifecycleEvent::PaneDied { .. }
            ) {
                return surviving_pane_event_target(state, &target);
            }
            let window_target =
                WindowTarget::with_window(target.session_name().clone(), target.window_index());
            let pane_exists = state
                .sessions
                .session(target.session_name())
                .and_then(|session| session.window_at(target.window_index()))
                .and_then(|window| window.pane(target.pane_index()))
                .is_some();
            if pane_exists {
                Some(Target::Pane(target))
            } else {
                active_window_target(&state.sessions, &window_target)
                    .or_else(|| active_session_target(&state.sessions, target.session_name()))
            }
        }
        None => None,
    }
}

fn surviving_pane_event_target(
    state: &crate::pane_terminals::HandlerState,
    exiting_target: &PaneTarget,
) -> Option<Target> {
    let session = state.sessions.session(exiting_target.session_name())?;
    let Some(window) = session.window_at(exiting_target.window_index()) else {
        return fallback_session_pane_target_after_exit(state, exiting_target);
    };
    if let Some(active_pane) = window.active_pane() {
        if active_pane.index() != exiting_target.pane_index() {
            return Some(Target::Pane(PaneTarget::with_window(
                exiting_target.session_name().clone(),
                exiting_target.window_index(),
                active_pane.index(),
            )));
        }
    }
    window
        .panes()
        .iter()
        .find(|pane| pane.index() != exiting_target.pane_index())
        .map(|pane| {
            Target::Pane(PaneTarget::with_window(
                exiting_target.session_name().clone(),
                exiting_target.window_index(),
                pane.index(),
            ))
        })
        .or_else(|| fallback_session_pane_target_after_exit(state, exiting_target))
}

fn fallback_session_pane_target_after_exit(
    state: &crate::pane_terminals::HandlerState,
    exiting_target: &PaneTarget,
) -> Option<Target> {
    if let Some(Target::Pane(target)) =
        active_session_target(&state.sessions, exiting_target.session_name())
    {
        if &target != exiting_target {
            return Some(Target::Pane(target));
        }
    }

    let session = state.sessions.session(exiting_target.session_name())?;
    session.windows().iter().find_map(|(window_index, window)| {
        let target_for_pane = |pane_index| {
            Target::Pane(PaneTarget::with_window(
                exiting_target.session_name().clone(),
                *window_index,
                pane_index,
            ))
        };
        if let Some(active_pane) = window.active_pane() {
            if *window_index != exiting_target.window_index()
                || active_pane.index() != exiting_target.pane_index()
            {
                return Some(target_for_pane(active_pane.index()));
            }
        }
        window.panes().iter().find_map(|pane| {
            (*window_index != exiting_target.window_index()
                || pane.index() != exiting_target.pane_index())
            .then(|| target_for_pane(pane.index()))
        })
    })
}

fn first_session_pane_target(
    state: &crate::pane_terminals::HandlerState,
    session_name: &rmux_proto::SessionName,
) -> Option<Target> {
    let session = state.sessions.session(session_name)?;
    session.windows().iter().find_map(|(window_index, window)| {
        window
            .active_pane()
            .or_else(|| window.panes().first())
            .map(|pane| {
                Target::Pane(PaneTarget::with_window(
                    session_name.clone(),
                    *window_index,
                    pane.index(),
                ))
            })
    })
}

fn hook_format_session_target(
    state: &crate::pane_terminals::HandlerState,
    formats: &[(String, String)],
) -> Option<Target> {
    let session_name = formats
        .iter()
        .rev()
        .find(|(name, _)| name == "hook_session_name")
        .and_then(|(_, value)| rmux_proto::SessionName::new(value.clone()).ok())?;
    first_session_pane_target(state, &session_name)
}

fn target_exists(state: &crate::pane_terminals::HandlerState, target: &Target) -> bool {
    match target {
        Target::Session(session_name) => state.sessions.session(session_name).is_some(),
        Target::Window(target) => state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .is_some(),
        Target::Pane(target) => state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .and_then(|window| window.pane(target.pane_index()))
            .is_some(),
    }
}

fn pane_target_for_existing_target(
    state: &crate::pane_terminals::HandlerState,
    target: &Target,
) -> Option<Target> {
    match target {
        Target::Pane(target) => Some(Target::Pane(target.clone())),
        Target::Window(target) => {
            let session = state.sessions.session(target.session_name())?;
            let window = session.window_at(target.window_index())?;
            let pane = window.active_pane().or_else(|| window.panes().first())?;
            Some(Target::Pane(PaneTarget::with_window(
                target.session_name().clone(),
                target.window_index(),
                pane.index(),
            )))
        }
        Target::Session(session_name) => first_session_pane_target(state, session_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_proto::{
        HookLifecycle, NewSessionRequest, NewWindowRequest, ScopeSelector, SplitDirection,
        SplitWindowRequest, SplitWindowTarget, TerminalSize,
    };

    fn session_name(value: &str) -> rmux_proto::SessionName {
        rmux_proto::SessionName::new(value).expect("valid session name")
    }

    #[tokio::test]
    async fn pane_exit_hook_current_target_skips_exiting_active_pane() {
        let handler = RequestHandler::new();
        let session = session_name("hook-current-target");
        let response = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        assert!(matches!(response, Response::NewSession(_)));
        let response = handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await;
        assert!(matches!(response, Response::SplitWindow(_)));

        let exiting = PaneTarget::with_window(session.clone(), 0, 1);
        let event = LifecycleEvent::PaneExited {
            target: exiting,
            pane_id: Some(1),
            window_id: Some(1),
            window_name: Some("hook-current-target".to_owned()),
        };
        let state = handler.state.lock().await;

        assert_eq!(
            lifecycle_hook_current_target(&state, &event),
            Some(Target::Pane(PaneTarget::with_window(session, 0, 0)))
        );
    }

    #[tokio::test]
    async fn pane_exit_hook_current_target_falls_back_when_window_closed() {
        let handler = RequestHandler::new();
        let session = session_name("hook-closed-window");
        let response = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        assert!(matches!(response, Response::NewSession(_)));

        let event = LifecycleEvent::PaneExited {
            target: PaneTarget::with_window(session.clone(), 1, 0),
            pane_id: Some(2),
            window_id: Some(2),
            window_name: Some("gone".to_owned()),
        };
        let state = handler.state.lock().await;

        assert_eq!(
            lifecycle_hook_current_target(&state, &event),
            Some(Target::Pane(PaneTarget::with_window(session, 0, 0)))
        );
    }

    #[tokio::test]
    async fn pane_exit_hook_current_target_falls_back_when_last_pane_closes_window() {
        let handler = RequestHandler::new();
        let session = session_name("hook-last-pane-window");
        let response = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        assert!(matches!(response, Response::NewSession(_)));
        let response = handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: session.clone(),
                name: None,
                detached: false,
                start_directory: None,
                environment: None,
                command: None,
                process_command: None,
                target_window_index: None,
                insert_at_target: false,
            })))
            .await;
        assert!(matches!(response, Response::NewWindow(_)));

        let event = LifecycleEvent::PaneExited {
            target: PaneTarget::with_window(session.clone(), 1, 0),
            pane_id: Some(2),
            window_id: Some(2),
            window_name: Some("gone".to_owned()),
        };
        let state = handler.state.lock().await;

        assert_eq!(
            lifecycle_hook_current_target(&state, &event),
            Some(Target::Pane(PaneTarget::with_window(session, 0, 0)))
        );
    }

    #[tokio::test]
    async fn pane_exit_dispatches_pane_hook_from_surviving_current_target() {
        let handler = RequestHandler::new();
        let session = session_name("hook-pane-scope");
        let response = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        assert!(matches!(response, Response::NewSession(_)));
        let response = handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await;
        assert!(matches!(response, Response::SplitWindow(_)));

        let survivor = PaneTarget::with_window(session.clone(), 0, 0);
        let exiting = PaneTarget::with_window(session.clone(), 0, 1);
        let event = LifecycleEvent::PaneExited {
            target: exiting,
            pane_id: Some(2),
            window_id: Some(1),
            window_name: Some("hook-pane-scope".to_owned()),
        };
        let mut state = handler.state.lock().await;
        state
            .hooks
            .set(
                ScopeSelector::Pane(survivor.clone()),
                HookName::PaneExited,
                "set-option -g @pt0 yes".to_owned(),
                HookLifecycle::Persistent,
            )
            .expect("pane hook set succeeds");

        let queued = prepare_lifecycle_event(&mut state, &event);

        assert_eq!(queued.current_target, Some(Target::Pane(survivor)));
        assert_eq!(queued.hooks.len(), 1);
        assert_eq!(queued.hooks[0].command(), "set-option -g @pt0 yes");
    }
}
