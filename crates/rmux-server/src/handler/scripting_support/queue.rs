use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use rmux_core::{
    command_queue::{CommandGroup, CommandQueue},
    MissingCurrentTargetFallback,
};
use rmux_proto::{Request, RmuxError, SessionId, SessionName, Target};

use crate::client_format::ClientLoopSnapshot;
use crate::control::ControlQueueCommandOrigin;
use crate::mouse::AttachedMouseEvent;

use super::super::lifecycle_support::{LeaseResolution, LifecycleTargetLease};
use super::super::{StablePaneOutputIdentity, StableTargetIdentity};
pub(super) use super::invocation::QueueInvocation;

mod command_action;
mod session_rename;

pub(in crate::handler) use self::command_action::QueueCommandAction;
pub(super) use self::command_action::{
    prompt_queue_action_from_result, queue_action_from_response,
};
pub(in crate::handler) use self::session_rename::{
    rename_pane_target_session, rename_target_session, rename_window_target_session,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QueueTerminalClient {
    Unrestricted,
    NonTerminal,
    Clientless,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CurrentSessionTransitionPolicy {
    Stable,
    StartupRoot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) struct QueueExecutionContext {
    pub(super) caller_cwd: Option<PathBuf>,
    pub(super) invocation_input: Option<super::invocation_input::FinishedInvocationInput>,
    pub(super) source_input_queue_status:
        Option<super::source_input_queue_status::SourceInputQueueStatus>,
    expose_hook_arguments: bool,
    pub(super) source_file_depth: usize,
    pub(super) current_file: Option<String>,
    pub(super) current_target: Option<Target>,
    pub(super) current_target_allows_canfail_fallback: bool,
    current_target_follows_client: bool,
    missing_current_target_fallback: MissingCurrentTargetFallback,
    run_shell_canfail_fallback_target: bool,
    pub(super) follows_attached_session: bool,
    pub(super) client_name: Option<String>,
    client_loop_snapshots: Option<Arc<[ClientLoopSnapshot]>>,
    pub(super) mouse_target: Option<Target>,
    pub(super) mouse_event: Option<AttachedMouseEvent>,
    retained_lifecycle_target: Option<Arc<LifecycleTargetLease>>,
    historical_retired_target_work: bool,
    pinned_current_target_identity: Option<Arc<StableTargetIdentity>>,
    pinned_pane_output_identity: Option<Arc<StablePaneOutputIdentity>>,
    rebased_current_session: Option<(SessionId, SessionName)>,
    current_session_transition_policy: CurrentSessionTransitionPolicy,
    run_shell_command_depth: usize,
    pub(super) terminal_client: QueueTerminalClient,
    control_queue_origin: Option<ControlQueueCommandOrigin>,
}

impl QueueExecutionContext {
    pub(super) fn with_invocation_input(mut self, bytes: Vec<u8>) -> Self {
        self.invocation_input = Some(super::invocation_input::FinishedInvocationInput::new(bytes));
        self
    }

    pub(in crate::handler) fn new(caller_cwd: Option<PathBuf>) -> Self {
        Self {
            caller_cwd,
            invocation_input: None,
            source_input_queue_status: None,
            expose_hook_arguments: true,
            source_file_depth: 0,
            current_file: None,
            current_target: None,
            current_target_allows_canfail_fallback: false,
            current_target_follows_client: false,
            missing_current_target_fallback: MissingCurrentTargetFallback::AllowDefaultSession,
            run_shell_canfail_fallback_target: false,
            follows_attached_session: false,
            client_name: None,
            client_loop_snapshots: None,
            mouse_target: None,
            mouse_event: None,
            retained_lifecycle_target: None,
            historical_retired_target_work: false,
            pinned_current_target_identity: None,
            pinned_pane_output_identity: None,
            rebased_current_session: None,
            current_session_transition_policy: CurrentSessionTransitionPolicy::Stable,
            run_shell_command_depth: 0,
            terminal_client: super::queued_command_context()
                .map_or(QueueTerminalClient::Unrestricted, |context| {
                    context.terminal_client
                }),
            control_queue_origin: None,
        }
    }

    pub(in crate::handler) fn without_caller_cwd() -> Self {
        Self::new(None)
    }

    pub(in crate::handler) fn for_sourced_commands(
        &self,
        source_file_depth: usize,
        current_file: Option<String>,
    ) -> Self {
        Self {
            expose_hook_arguments: true,
            source_file_depth,
            current_file,
            current_session_transition_policy: match (
                self.current_session_transition_policy,
                source_file_depth,
            ) {
                (CurrentSessionTransitionPolicy::StartupRoot, 1) => {
                    CurrentSessionTransitionPolicy::StartupRoot
                }
                _ => CurrentSessionTransitionPolicy::Stable,
            },
            control_queue_origin: Some(ControlQueueCommandOrigin::SourceFile {
                depth: source_file_depth,
            }),
            ..self.clone()
        }
    }

    pub(in crate::handler) fn for_if_shell_commands(mut self) -> Self {
        self.expose_hook_arguments = true;
        self.control_queue_origin = Some(ControlQueueCommandOrigin::IfShell);
        self
    }

    pub(super) fn for_startup_config(mut self) -> Self {
        self.current_session_transition_policy = CurrentSessionTransitionPolicy::StartupRoot;
        self
    }

    // Direct CLI requests historically omit parsed argv from hook formats.
    // Child command queues recover their ordinary metadata at their entry seam.
    pub(super) fn for_direct_cli(mut self) -> Self {
        self.expose_hook_arguments = false;
        self
    }

    pub(super) const fn exposes_hook_arguments(&self) -> bool {
        self.expose_hook_arguments
    }

    pub(in crate::handler) fn with_caller_cwd(mut self, caller_cwd: Option<PathBuf>) -> Self {
        self.caller_cwd = caller_cwd;
        self
    }

    pub(in crate::handler) const fn run_shell_command_depth(&self) -> usize {
        self.run_shell_command_depth
    }

    pub(super) fn terminal_attach_is_refused(&self) -> bool {
        self.terminal_client == QueueTerminalClient::NonTerminal
    }

    pub(in crate::handler) const fn control_queue_origin(
        &self,
    ) -> Option<ControlQueueCommandOrigin> {
        self.control_queue_origin
    }

    pub(in crate::handler) fn for_run_shell_commands(
        mut self,
        parent_depth: usize,
        synchronous: bool,
        background: bool,
    ) -> Result<Self, RmuxError> {
        let depth = parent_depth.saturating_add(1);
        if depth > super::RUN_SHELL_COMMAND_NESTING_LIMIT {
            return Err(RmuxError::Server(format!(
                "run-shell -C nesting exceeds safe runtime limit ({})",
                super::RUN_SHELL_COMMAND_NESTING_LIMIT
            )));
        }
        self.expose_hook_arguments = true;
        self.run_shell_command_depth = depth;
        self.terminal_client =
            if background || self.terminal_client == QueueTerminalClient::Clientless {
                QueueTerminalClient::Clientless
            } else {
                QueueTerminalClient::NonTerminal
            };
        self.control_queue_origin =
            synchronous.then_some(ControlQueueCommandOrigin::RunShell { depth });
        Ok(self)
    }

    pub(in crate::handler) fn with_current_target(
        mut self,
        current_target: Option<Target>,
    ) -> Self {
        self.current_target_allows_canfail_fallback = current_target.is_some();
        self.current_target_follows_client = false;
        self.follows_attached_session = false;
        self.current_target = current_target;
        self.pinned_current_target_identity = None;
        self.pinned_pane_output_identity = None;
        self.rebased_current_session = None;
        self
    }

    pub(in crate::handler) fn with_implicit_current_target(
        mut self,
        current_target: Option<Target>,
    ) -> Self {
        self.current_target = current_target;
        self.current_target_allows_canfail_fallback = false;
        self.pinned_current_target_identity = None;
        self.pinned_pane_output_identity = None;
        self.rebased_current_session = None;
        self
    }

    /// Refreshes the implicit current target with the requesting client's
    /// implicit source target, so it follows that client.
    pub(super) fn refresh_implicit_current_target(
        mut self,
        current_target: Option<Target>,
    ) -> Self {
        let Some((_, session_name)) = self.rebased_current_session.as_ref() else {
            return self
                .with_implicit_current_target(current_target)
                .with_current_target_following_client();
        };
        if self.pinned_current_target_identity.is_some() {
            return self;
        }
        if current_target
            .as_ref()
            .is_some_and(|target| target.session_name() == session_name)
        {
            self.current_target = current_target;
            self.current_target_follows_client = true;
            self.pinned_pane_output_identity = None;
        }
        self
    }

    pub(super) fn accepts_explicit_current_session_transition(&self) -> bool {
        self.current_session_transition_policy == CurrentSessionTransitionPolicy::StartupRoot
            && self.source_file_depth == 1
            && self.rebased_current_session.is_some()
    }

    pub(super) fn rebase_after_explicit_current_session_transition(
        &mut self,
        session_id: SessionId,
        session_name: &SessionName,
    ) {
        if !self.accepts_explicit_current_session_transition() {
            return;
        }
        self.current_target = Some(Target::Session(session_name.clone()));
        self.current_target_allows_canfail_fallback = false;
        self.follows_attached_session = false;
        self.pinned_current_target_identity = None;
        self.pinned_pane_output_identity = None;
        self.rebased_current_session = Some((session_id, session_name.clone()));
    }

    pub(in crate::handler) fn forbid_missing_current_target_fallback(mut self) -> Self {
        self.missing_current_target_fallback = MissingCurrentTargetFallback::ForbidDefaultSession;
        self
    }

    pub(super) const fn missing_current_target_fallback(&self) -> MissingCurrentTargetFallback {
        self.missing_current_target_fallback
    }

    pub(in crate::handler) fn with_run_shell_canfail_fallback_target(mut self) -> Self {
        self.run_shell_canfail_fallback_target = self.current_target.is_some();
        self
    }

    pub(in crate::handler) fn following_attached_session(mut self) -> Self {
        if !self.current_target_allows_canfail_fallback {
            self.follows_attached_session = true;
        }
        self
    }

    pub(in crate::handler) fn follows_attached_session(&self) -> bool {
        self.follows_attached_session
    }

    pub(in crate::handler) fn rebase_current_target_after_attached_switch(
        &mut self,
        current_target: Target,
    ) {
        self.current_target = Some(current_target);
        self.pinned_current_target_identity = None;
        self.pinned_pane_output_identity = None;
        self.rebased_current_session = None;
    }

    /// Marks an implicit current target resolved from the requesting client
    /// (its inherited pane), not pinned by a hook, key binding or `-t`; see
    /// `TargetFindContext::following_client`.
    pub(super) fn with_current_target_following_client(mut self) -> Self {
        self.current_target_follows_client = true;
        self
    }

    pub(super) const fn current_target_follows_client(&self) -> bool {
        self.current_target_follows_client
    }

    pub(in crate::handler) fn uses_explicit_current_target(&self) -> bool {
        self.current_target_allows_canfail_fallback
    }

    pub(in crate::handler) fn can_refresh_implicit_source_target(&self) -> bool {
        self.source_file_depth > 0
            && !self.historical_retired_target_work
            && !self.uses_explicit_current_target()
            && !self.follows_attached_session
            && self.pinned_current_target_identity.is_none()
    }

    pub(in crate::handler) fn with_client_name(mut self, client_name: Option<String>) -> Self {
        self.client_name = client_name;
        self
    }

    pub(in crate::handler) fn with_client_loop_snapshots(
        mut self,
        snapshots: Vec<ClientLoopSnapshot>,
    ) -> Self {
        self.client_loop_snapshots = Some(Arc::from(snapshots.into_boxed_slice()));
        self
    }

    pub(in crate::handler) fn client_loop_snapshots(&self) -> Option<&[ClientLoopSnapshot]> {
        self.client_loop_snapshots.as_deref()
    }

    pub(in crate::handler) fn with_mouse_target(mut self, mouse_target: Option<Target>) -> Self {
        self.mouse_target = mouse_target;
        self
    }

    pub(in crate::handler) fn with_mouse_event(
        mut self,
        mouse_event: Option<AttachedMouseEvent>,
    ) -> Self {
        self.mouse_event = mouse_event;
        self
    }

    pub(in crate::handler) fn with_retained_lifecycle_target(
        mut self,
        target: Option<Arc<LifecycleTargetLease>>,
    ) -> Self {
        if target.is_none() {
            return self.without_retained_lifecycle_target();
        }
        self.retained_lifecycle_target = target;
        self
    }

    pub(in crate::handler) fn without_retained_lifecycle_target(mut self) -> Self {
        self.retained_lifecycle_target = None;
        self.historical_retired_target_work = false;
        self
    }

    pub(in crate::handler) fn with_historical_retired_target_work(mut self) -> Self {
        self.historical_retired_target_work = true;
        self
    }

    pub(super) const fn allows_historical_retired_target_work(&self) -> bool {
        self.historical_retired_target_work
    }

    pub(in crate::handler) fn with_pinned_current_target_identity(
        mut self,
        identity: Option<StableTargetIdentity>,
    ) -> Self {
        if let Some(identity) = identity {
            self.current_target = Some(identity.target().clone());
            self.pinned_current_target_identity = Some(Arc::new(identity));
            self.rebased_current_session = None;
        }
        self
    }

    pub(in crate::handler) fn with_pinned_pane_output_identity(
        mut self,
        identity: Option<StablePaneOutputIdentity>,
    ) -> Self {
        self.pinned_pane_output_identity = identity.map(Arc::new);
        self
    }

    pub(super) fn require_pinned_current_target(
        &self,
        state: &crate::pane_terminals::HandlerState,
    ) -> Result<(), RmuxError> {
        match (
            self.pinned_current_target_identity.as_ref(),
            self.current_target.as_ref(),
        ) {
            (Some(identity), Some(target)) => {
                identity.require(state, target, "queued pinned")?;
            }
            (Some(_), None) => Err(RmuxError::Server(
                "queued pinned target was unavailable".to_owned(),
            ))?,
            (None, _) => {}
        }
        if let Some(identity) = self.pinned_pane_output_identity.as_ref() {
            identity.require(state, "queued pinned")?;
        }
        if let Some((session_id, session_name)) = self.rebased_current_session.as_ref() {
            if state
                .sessions
                .session(session_name)
                .is_none_or(|session| session.id() != *session_id)
            {
                return Err(RmuxError::Server(
                    "queued renamed target was replaced before execution".to_owned(),
                ));
            }
        }
        Ok(())
    }

    pub(in crate::handler) fn require_live_retained_lifecycle_target(
        &self,
        state: &crate::pane_terminals::HandlerState,
        operation: &str,
    ) -> Result<(), RmuxError> {
        match self
            .retained_lifecycle_target
            .as_deref()
            .map(|target| target.resolve(state))
        {
            Some(LeaseResolution::Retired(_)) => Err(RmuxError::Server(format!(
                "queued lifecycle target retired before {operation}"
            ))),
            Some(LeaseResolution::Replaced) => Err(RmuxError::Server(format!(
                "queued lifecycle target was replaced before {operation}"
            ))),
            Some(LeaseResolution::Live(_)) | None => Ok(()),
        }
    }

    pub(super) fn retained_lifecycle_target(&self) -> Option<&Arc<LifecycleTargetLease>> {
        self.retained_lifecycle_target.as_ref()
    }

    pub(in crate::handler) fn without_mouse_event(mut self) -> Self {
        self.mouse_event = None;
        self
    }

    pub(in crate::handler) fn without_mouse_origin(mut self) -> Self {
        self.mouse_target = None;
        self.mouse_event = None;
        self
    }

    pub(in crate::handler) fn current_target(&self) -> Option<&Target> {
        self.current_target.as_ref()
    }

    pub(in crate::handler) fn canfail_fallback_target(&self) -> Option<&Target> {
        self.current_target_allows_canfail_fallback
            .then_some(self.current_target.as_ref())
            .flatten()
    }

    pub(in crate::handler) fn run_shell_canfail_fallback_target(&self) -> Option<&Target> {
        self.run_shell_canfail_fallback_target
            .then_some(self.current_target.as_ref())
            .flatten()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QueueMode {
    Detached,
    Control,
}

pub(super) fn remove_group_contexts(
    queue: &CommandQueue,
    contexts: &mut VecDeque<QueueExecutionContext>,
    group: CommandGroup,
) {
    let mut retained = VecDeque::new();
    for (item, context) in queue.items().iter().zip(contexts.drain(..)) {
        if item.group() != group {
            retained.push_back(context);
        }
    }
    *contexts = retained;
}

pub(super) fn captures_attached_client_transition(request: &Request) -> bool {
    matches!(
        request,
        Request::AttachSession(_)
            | Request::AttachSessionExt(_)
            | Request::AttachSessionExt2(_)
            | Request::AttachSessionExt3(_)
            | Request::SwitchClient(_)
            | Request::SwitchClientExt(_)
            | Request::SwitchClientExt2(_)
            | Request::SwitchClientExt3(_)
            | Request::NewSession(_)
            | Request::NewSessionExt(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_proto::request::{
        AttachSessionExt2Request, AttachSessionExt3Request, AttachSessionExtRequest,
        SwitchClientExt2Request, SwitchClientExt3Request, SwitchClientExtRequest,
    };
    use rmux_proto::{AttachSessionRequest, PaneTarget, SessionName, SwitchClientRequest};

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid test session")
    }

    #[tokio::test]
    async fn constructors_inherit_terminal_authority_without_parent_payload() {
        assert_eq!(
            QueueExecutionContext::without_caller_cwd().terminal_client,
            QueueTerminalClient::Unrestricted
        );
        for terminal_client in [
            QueueTerminalClient::Unrestricted,
            QueueTerminalClient::NonTerminal,
            QueueTerminalClient::Clientless,
        ] {
            let mut ambient = QueueExecutionContext::new(Some(PathBuf::from("parent")))
                .with_invocation_input(b"parent-input".to_vec());
            ambient.terminal_client = terminal_client;
            ambient.source_file_depth = 9;
            ambient.current_file = Some("parent.rmux".to_owned());
            ambient.source_input_queue_status =
                Some(super::super::source_input_queue_status::SourceInputQueueStatus::new());
            super::super::QUEUED_COMMAND_CONTEXT
                .scope(ambient, async {
                    let with_cwd = QueueExecutionContext::new(Some(PathBuf::from("child")));
                    let no_cwd = QueueExecutionContext::without_caller_cwd();
                    assert_eq!(with_cwd.caller_cwd, Some(PathBuf::from("child")));
                    assert_eq!(no_cwd.caller_cwd, None);
                    for fresh in [with_cwd, no_cwd] {
                        assert_eq!(fresh.terminal_client, terminal_client);
                        assert!(fresh.invocation_input.is_none());
                        assert!(fresh.source_input_queue_status.is_none());
                        assert_eq!(fresh.source_file_depth, 0);
                        assert!(fresh.current_file.is_none());
                        assert!(fresh.current_target.is_none());
                        assert!(fresh.control_queue_origin.is_none());
                        assert!(fresh.expose_hook_arguments);
                    }
                })
                .await;
        }
    }

    #[test]
    fn sourced_context_preserves_authority_and_resets_entry_metadata() {
        for policy in [
            CurrentSessionTransitionPolicy::Stable,
            CurrentSessionTransitionPolicy::StartupRoot,
        ] {
            for terminal_client in [
                QueueTerminalClient::Unrestricted,
                QueueTerminalClient::NonTerminal,
                QueueTerminalClient::Clientless,
            ] {
                let mut parent = QueueExecutionContext::new(Some(PathBuf::from("caller")))
                    .with_invocation_input(b"binary\0payload".to_vec());
                parent.current_target = Some(Target::Session(session_name("alpha")));
                parent.current_target_allows_canfail_fallback = true;
                parent.run_shell_canfail_fallback_target = true;
                parent.follows_attached_session = true;
                parent.client_name = Some("client".to_owned());
                parent.historical_retired_target_work = true;
                parent.expose_hook_arguments = false;
                parent.run_shell_command_depth = 17;
                parent.source_file_depth = 8;
                parent.current_file = Some("parent.rmux".to_owned());
                parent.current_session_transition_policy = policy;
                parent.terminal_client = terminal_client;
                parent.control_queue_origin = Some(ControlQueueCommandOrigin::IfShell);
                for depth in [0, 1, 2] {
                    let child = parent.for_sourced_commands(depth, Some("child.rmux".to_owned()));
                    assert_eq!(child.caller_cwd, parent.caller_cwd);
                    assert_eq!(child.current_target, parent.current_target);
                    assert_eq!(child.invocation_input, parent.invocation_input);
                    assert!(child.current_target_allows_canfail_fallback);
                    assert!(child.run_shell_canfail_fallback_target);
                    assert!(child.follows_attached_session);
                    assert_eq!(child.client_name.as_deref(), Some("client"));
                    assert!(child.historical_retired_target_work);
                    assert!(!child.can_refresh_implicit_source_target());
                    assert_eq!(child.run_shell_command_depth, 17);
                    assert_eq!(child.terminal_client, terminal_client);
                    assert_eq!(
                        child.terminal_attach_is_refused(),
                        terminal_client == QueueTerminalClient::NonTerminal
                    );
                    assert!(child.expose_hook_arguments);
                    assert_eq!(child.source_file_depth, depth);
                    assert_eq!(child.current_file.as_deref(), Some("child.rmux"));
                    assert_eq!(
                        child.control_queue_origin,
                        Some(ControlQueueCommandOrigin::SourceFile { depth })
                    );
                    let expected_policy =
                        if policy == CurrentSessionTransitionPolicy::StartupRoot && depth == 1 {
                            CurrentSessionTransitionPolicy::StartupRoot
                        } else {
                            CurrentSessionTransitionPolicy::Stable
                        };
                    assert_eq!(child.current_session_transition_policy, expected_policy);
                }
                assert_eq!(parent.current_file.as_deref(), Some("parent.rmux"));
                assert_eq!(parent.source_file_depth, 8);
                assert!(!parent.expose_hook_arguments);
            }
        }
    }

    #[tokio::test]
    async fn sourced_contexts_share_one_input_and_keep_outer_selection_depth() {
        let mut parent = QueueExecutionContext::without_caller_cwd()
            .with_invocation_input(b"one\0input".to_vec());
        parent.source_input_queue_status = Some(
            super::super::source_input_queue_status::SourceInputQueueStatus::new().at_depth(1),
        );
        let child = parent.for_sourced_commands(1, Some("outer.rmux".to_owned()));
        let nested = child.for_sourced_commands(2, Some("inner.rmux".to_owned()));
        assert_eq!(
            nested
                .invocation_input
                .as_ref()
                .unwrap()
                .reserve()
                .await
                .take(),
            Some(b"one\0input".to_vec())
        );
        assert!(parent
            .invocation_input
            .as_ref()
            .unwrap()
            .reserve()
            .await
            .is_none());
        assert!(child
            .invocation_input
            .as_ref()
            .unwrap()
            .reserve()
            .await
            .is_none());
        nested
            .source_input_queue_status
            .as_ref()
            .unwrap()
            .record_selection_failure(2);
        assert!(!parent
            .source_input_queue_status
            .as_ref()
            .unwrap()
            .stop_queue());
        child
            .source_input_queue_status
            .as_ref()
            .unwrap()
            .record_selection_failure(1);
        assert!(parent
            .source_input_queue_status
            .as_ref()
            .unwrap()
            .stop_queue());
    }

    #[test]
    fn attached_transition_capture_recognizes_every_request_generation() {
        let alpha = session_name("alpha");
        assert!(captures_attached_client_transition(
            &Request::AttachSession(AttachSessionRequest {
                target: alpha.clone(),
            })
        ));
        assert!(captures_attached_client_transition(
            &Request::AttachSessionExt(AttachSessionExtRequest {
                target: Some(alpha.clone()),
                detach_other_clients: false,
                kill_other_clients: false,
                read_only: false,
                skip_environment_update: false,
                flags: None,
            })
        ));
        let attach_ext2 = AttachSessionExt2Request {
            target: Some(alpha.clone()),
            target_spec: None,
            detach_other_clients: false,
            kill_other_clients: false,
            read_only: false,
            skip_environment_update: false,
            flags: None,
            working_directory: None,
            client_terminal: Default::default(),
            client_size: None,
        };
        assert!(captures_attached_client_transition(
            &Request::AttachSessionExt2(Box::new(attach_ext2.clone()))
        ));
        assert!(captures_attached_client_transition(
            &Request::AttachSessionExt3(Box::new(AttachSessionExt3Request::from_ext2(
                attach_ext2,
                Vec::new(),
            )))
        ));
        assert!(captures_attached_client_transition(&Request::SwitchClient(
            SwitchClientRequest {
                target: alpha.clone(),
            }
        )));
        assert!(captures_attached_client_transition(
            &Request::SwitchClientExt(SwitchClientExtRequest {
                target: None,
                key_table: Some("root".to_owned()),
            })
        ));
        assert!(captures_attached_client_transition(
            &Request::SwitchClientExt2(Box::new(SwitchClientExt2Request {
                target: None,
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: true,
                toggle_read_only: true,
                flags: None,
                sort_order: None,
                skip_environment_update: false,
            }))
        ));
        assert!(captures_attached_client_transition(
            &Request::SwitchClientExt3(Box::new(SwitchClientExt3Request {
                target_client: Some("other".to_owned()),
                target: Some(alpha.to_string()),
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                sort_order: None,
                skip_environment_update: false,
                zoom: false,
            }))
        ));
        assert!(!captures_attached_client_transition(&Request::KillServer(
            rmux_proto::KillServerRequest
        )));
    }

    #[test]
    fn attached_switch_rebases_every_queue_target() {
        let alpha = Target::Session(session_name("alpha"));
        let beta = Target::Session(session_name("beta"));
        let mut implicit = QueueExecutionContext::without_caller_cwd()
            .with_implicit_current_target(Some(alpha.clone()));
        let mut explicit =
            QueueExecutionContext::without_caller_cwd().with_current_target(Some(alpha));

        implicit.rebase_current_target_after_attached_switch(beta.clone());
        explicit.rebase_current_target_after_attached_switch(beta);

        assert_eq!(
            implicit.current_target(),
            Some(&Target::Session(session_name("beta")))
        );
        assert_eq!(
            explicit.current_target(),
            Some(&Target::Session(session_name("beta")))
        );
    }

    #[test]
    fn only_unpinned_implicit_source_contexts_allow_environment_refresh() {
        let pane = PaneTarget::with_window(session_name("alpha"), 0, 0);
        let target = Some(Target::Pane(pane.clone()));
        let implicit = QueueExecutionContext::without_caller_cwd()
            .with_implicit_current_target(target.clone())
            .for_sourced_commands(1, None);
        let explicit = QueueExecutionContext::without_caller_cwd()
            .with_current_target(target.clone())
            .for_sourced_commands(1, None);
        let pinned = QueueExecutionContext::without_caller_cwd()
            .with_implicit_current_target(target.clone())
            .with_pinned_current_target_identity(Some(StableTargetIdentity::pane_for_test(pane)))
            .for_sourced_commands(1, None);
        let attached = QueueExecutionContext::without_caller_cwd()
            .with_implicit_current_target(target)
            .following_attached_session()
            .for_sourced_commands(1, None);
        let historical = QueueExecutionContext::without_caller_cwd()
            .with_historical_retired_target_work()
            .for_sourced_commands(1, None);

        assert!(implicit.can_refresh_implicit_source_target());
        assert!(!explicit.can_refresh_implicit_source_target());
        assert!(!pinned.can_refresh_implicit_source_target());
        assert!(!attached.can_refresh_implicit_source_target());
        assert!(!historical.can_refresh_implicit_source_target());
    }

    #[test]
    fn session_rename_rebases_only_the_matching_pinned_identity() {
        let alpha = session_name("alpha");
        let beta = session_name("beta");
        let pane = PaneTarget::with_window(alpha.clone(), 0, 0);
        let identity = StableTargetIdentity::pane_for_test(pane);
        let mut matching = QueueExecutionContext::without_caller_cwd()
            .with_pinned_current_target_identity(Some(identity.clone()));
        let mut replacement = QueueExecutionContext::without_caller_cwd()
            .with_pinned_current_target_identity(Some(identity));

        matching.rebase_after_session_rename(SessionId::new(1), &alpha, &beta);
        replacement.rebase_after_session_rename(SessionId::new(2), &alpha, &beta);

        assert_eq!(
            matching.current_target(),
            Some(&Target::Pane(PaneTarget::with_window(beta, 0, 0)))
        );
        assert_eq!(
            replacement.current_target(),
            Some(&Target::Pane(PaneTarget::with_window(alpha, 0, 0)))
        );
    }
}
