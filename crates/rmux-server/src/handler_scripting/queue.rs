use std::collections::VecDeque;
use std::path::PathBuf;

use rmux_core::{
    command_parser::ParsedCommands,
    command_queue::{CommandGroup, CommandQueue},
};
use rmux_proto::{
    CommandOutput, ErrorResponse, PaneTarget, Request, Response, RmuxError, SessionName, Target,
    WindowTarget,
};

use crate::mouse::AttachedMouseEvent;

use super::list_commands_runtime::ParsedListCommandsCommand;
use super::list_parse::ParsedListPanesAllCommand;
use super::pane_parse::ParsedSplitWindowCommand;
use super::prompt_parse::{
    ParsedCommandPromptCommand, ParsedConfirmBeforeCommand, ParsedPromptHistoryCommand,
};
use super::queue_parse::{ParsedIfShellCommand, ParsedNewWindowCommand};
use super::source_files::ParsedSourceFileCommand;

#[derive(Debug, Clone)]
pub(in crate::handler) struct QueueExecutionContext {
    pub(super) caller_cwd: Option<PathBuf>,
    pub(super) source_file_depth: usize,
    pub(super) current_file: Option<String>,
    pub(super) current_target: Option<Target>,
    pub(super) current_target_allows_canfail_fallback: bool,
    pub(super) client_name: Option<String>,
    pub(super) mouse_target: Option<Target>,
    pub(super) mouse_event: Option<AttachedMouseEvent>,
}

impl QueueExecutionContext {
    pub(in crate::handler) fn new(caller_cwd: Option<PathBuf>) -> Self {
        Self {
            caller_cwd,
            source_file_depth: 0,
            current_file: None,
            current_target: None,
            current_target_allows_canfail_fallback: false,
            client_name: None,
            mouse_target: None,
            mouse_event: None,
        }
    }

    pub(in crate::handler) fn without_caller_cwd() -> Self {
        Self {
            caller_cwd: None,
            source_file_depth: 0,
            current_file: None,
            current_target: None,
            current_target_allows_canfail_fallback: false,
            client_name: None,
            mouse_target: None,
            mouse_event: None,
        }
    }

    pub(in crate::handler) fn for_sourced_commands(
        &self,
        source_file_depth: usize,
        current_file: Option<String>,
    ) -> Self {
        Self {
            caller_cwd: self.caller_cwd.clone(),
            source_file_depth,
            current_file,
            current_target: self.current_target.clone(),
            current_target_allows_canfail_fallback: self.current_target_allows_canfail_fallback,
            client_name: self.client_name.clone(),
            mouse_target: self.mouse_target.clone(),
            mouse_event: self.mouse_event.clone(),
        }
    }

    pub(in crate::handler) fn with_current_target(
        mut self,
        current_target: Option<Target>,
    ) -> Self {
        self.current_target_allows_canfail_fallback = current_target.is_some();
        self.current_target = current_target;
        self
    }

    pub(in crate::handler) fn with_implicit_current_target(
        mut self,
        current_target: Option<Target>,
    ) -> Self {
        self.current_target = current_target;
        self.current_target_allows_canfail_fallback = false;
        self
    }

    pub(in crate::handler) fn rebase_implicit_current_target(&mut self, current_target: Target) {
        if !self.uses_explicit_current_target() {
            self.current_target = Some(current_target);
        }
    }

    pub(in crate::handler) fn uses_explicit_current_target(&self) -> bool {
        self.current_target_allows_canfail_fallback
    }

    pub(in crate::handler) fn with_client_name(mut self, client_name: Option<String>) -> Self {
        self.client_name = client_name;
        self
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

    pub(in crate::handler) fn current_target(&self) -> Option<&Target> {
        self.current_target.as_ref()
    }

    pub(in crate::handler) fn canfail_fallback_target(&self) -> Option<&Target> {
        self.current_target_allows_canfail_fallback
            .then_some(self.current_target.as_ref())
            .flatten()
    }

    pub(in crate::handler) fn rename_session_targets(
        &mut self,
        old_name: &SessionName,
        new_name: &SessionName,
    ) {
        if let Some(target) = self.current_target.as_mut() {
            rename_target_session(target, old_name, new_name);
        }
        if let Some(target) = self.mouse_target.as_mut() {
            rename_target_session(target, old_name, new_name);
        }
        if let Some(event) = self.mouse_event.as_mut() {
            if let Some(target) = event.pane_target.as_mut() {
                rename_pane_target_session(target, old_name, new_name);
            }
        }
    }
}

pub(in crate::handler) fn rename_target_session(
    target: &mut Target,
    old_name: &SessionName,
    new_name: &SessionName,
) {
    match target {
        Target::Session(session_name) if session_name == old_name => {
            *session_name = new_name.clone();
        }
        Target::Window(window) => rename_window_target_session(window, old_name, new_name),
        Target::Pane(pane) => rename_pane_target_session(pane, old_name, new_name),
        Target::Session(_) => {}
    }
}

pub(in crate::handler) fn rename_window_target_session(
    target: &mut WindowTarget,
    old_name: &SessionName,
    new_name: &SessionName,
) {
    if target.session_name() != old_name {
        return;
    }
    *target = WindowTarget::with_window(new_name.clone(), target.window_index());
}

pub(in crate::handler) fn rename_pane_target_session(
    target: &mut PaneTarget,
    old_name: &SessionName,
    new_name: &SessionName,
) {
    if target.session_name() != old_name {
        return;
    }
    *target = PaneTarget::with_window(new_name.clone(), target.window_index(), target.pane_index());
}

#[derive(Debug, Clone)]
pub(in crate::handler) enum QueueCommandAction {
    Normal {
        output: Option<CommandOutput>,
        error: Option<RmuxError>,
        source_file_error: Option<RmuxError>,
        exit_status: Option<i32>,
    },
    InsertAfter {
        batches: Vec<(ParsedCommands, QueueExecutionContext)>,
        output: Option<CommandOutput>,
        error: Option<RmuxError>,
        source_file_error: Option<RmuxError>,
        exit_status: Option<i32>,
    },
}

impl QueueCommandAction {
    pub(super) fn without_output(self) -> Self {
        match self {
            Self::Normal {
                error,
                source_file_error,
                exit_status,
                ..
            } => Self::Normal {
                output: None,
                error,
                source_file_error,
                exit_status,
            },
            Self::InsertAfter {
                batches,
                error,
                source_file_error,
                exit_status,
                ..
            } => Self::InsertAfter {
                batches,
                output: None,
                error,
                source_file_error,
                exit_status,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QueueMode {
    Detached,
    Control,
}

#[derive(Debug, Clone)]
pub(super) enum QueueInvocation {
    Request(Request),
    NoOp,
    StartServer,
    ListCommands(ParsedListCommandsCommand),
    NewWindow(ParsedNewWindowCommand),
    IfShell(ParsedIfShellCommand),
    SourceFile(ParsedSourceFileCommand),
    ListPanesAll(ParsedListPanesAllCommand),
    SplitWindow(ParsedSplitWindowCommand),
    MouseResizePane(rmux_proto::PaneTarget),
    CommandPrompt(ParsedCommandPromptCommand),
    ConfirmBefore(ParsedConfirmBeforeCommand),
    ModeTree(super::super::mode_tree_support::ParsedModeTreeCommand),
    Overlay(super::super::overlay_support::ParsedOverlayCommand),
    PromptHistory(ParsedPromptHistoryCommand),
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
    )
}

pub(super) fn queue_action_from_response(
    response: Response,
) -> Result<QueueCommandAction, RmuxError> {
    match response {
        Response::Error(ErrorResponse { error }) => Err(error),
        Response::RunShell(response) => Ok(QueueCommandAction::Normal {
            output: response
                .command_output()
                .filter(|output| !output.stdout().is_empty())
                .cloned(),
            error: None,
            source_file_error: None,
            exit_status: response.exit_status(),
        }),
        response => Ok(QueueCommandAction::Normal {
            output: response
                .command_output()
                .filter(|output| !output.stdout().is_empty())
                .cloned(),
            error: None,
            source_file_error: None,
            exit_status: None,
        }),
    }
}

pub(super) fn prompt_queue_action_from_result(
    result: super::super::prompt_support::PromptQueueResult,
) -> QueueCommandAction {
    match result.inserted {
        Some((parsed, context)) => QueueCommandAction::InsertAfter {
            batches: vec![(parsed, context)],
            output: None,
            error: result.error,
            source_file_error: None,
            exit_status: None,
        },
        None => QueueCommandAction::Normal {
            output: None,
            error: result.error,
            source_file_error: None,
            exit_status: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_proto::request::{
        AttachSessionExt2Request, AttachSessionExt3Request, AttachSessionExtRequest,
        SwitchClientExt2Request, SwitchClientExt3Request, SwitchClientExtRequest,
    };
    use rmux_proto::{AttachSessionRequest, SessionName, SwitchClientRequest};

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid test session")
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
    fn attached_switch_rebases_only_implicit_queue_targets() {
        let alpha = Target::Session(session_name("alpha"));
        let beta = Target::Session(session_name("beta"));
        let mut implicit = QueueExecutionContext::without_caller_cwd()
            .with_implicit_current_target(Some(alpha.clone()));
        let mut explicit =
            QueueExecutionContext::without_caller_cwd().with_current_target(Some(alpha));

        implicit.rebase_implicit_current_target(beta.clone());
        explicit.rebase_implicit_current_target(beta);

        assert_eq!(
            implicit.current_target(),
            Some(&Target::Session(session_name("beta")))
        );
        assert_eq!(
            explicit.current_target(),
            Some(&Target::Session(session_name("alpha")))
        );
    }
}
