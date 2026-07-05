use std::collections::VecDeque;
use std::path::PathBuf;

use rmux_core::{
    command_parser::ParsedCommands,
    command_queue::{CommandGroup, CommandQueue},
};
use rmux_proto::{CommandOutput, ErrorResponse, Request, Response, RmuxError, Target};

use crate::mouse::AttachedMouseEvent;

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
