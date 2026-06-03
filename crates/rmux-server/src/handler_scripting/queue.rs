use std::collections::VecDeque;
use std::path::PathBuf;

use rmux_core::{
    command_parser::ParsedCommands,
    command_queue::{CommandGroup, CommandQueue},
};
use rmux_proto::{CommandOutput, ErrorResponse, Request, Response, RmuxError, Target};

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
    pub(super) mouse_target: Option<Target>,
}

impl QueueExecutionContext {
    pub(in crate::handler) fn new(caller_cwd: Option<PathBuf>) -> Self {
        Self {
            caller_cwd,
            source_file_depth: 0,
            current_file: None,
            current_target: None,
            mouse_target: None,
        }
    }

    pub(in crate::handler) fn without_caller_cwd() -> Self {
        Self {
            caller_cwd: None,
            source_file_depth: 0,
            current_file: None,
            current_target: None,
            mouse_target: None,
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
            mouse_target: self.mouse_target.clone(),
        }
    }

    pub(in crate::handler) fn with_current_target(
        mut self,
        current_target: Option<Target>,
    ) -> Self {
        self.current_target = current_target;
        self
    }

    pub(in crate::handler) fn with_mouse_target(mut self, mouse_target: Option<Target>) -> Self {
        self.mouse_target = mouse_target;
        self
    }

    pub(in crate::handler) fn current_target(&self) -> Option<&Target> {
        self.current_target.as_ref()
    }
}

#[derive(Debug, Clone)]
pub(in crate::handler) enum QueueCommandAction {
    Normal {
        output: Option<CommandOutput>,
        error: Option<RmuxError>,
    },
    InsertAfter {
        batches: Vec<(ParsedCommands, QueueExecutionContext)>,
        output: Option<CommandOutput>,
        error: Option<RmuxError>,
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
        response => Ok(QueueCommandAction::Normal {
            output: response
                .command_output()
                .filter(|output| !output.stdout().is_empty())
                .cloned(),
            error: None,
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
        },
        None => QueueCommandAction::Normal {
            output: None,
            error: result.error,
        },
    }
}
