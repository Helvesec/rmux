//! The action a queued command leaves for the queue: output, error and inserted batches.

use rmux_core::command_parser::ParsedCommands;
use rmux_proto::{CommandOutput, ErrorResponse, Response, RmuxError};

use super::QueueExecutionContext;

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
    pub(in crate::handler::scripting_support) fn without_output(self) -> Self {
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

pub(in crate::handler::scripting_support) fn queue_action_from_response(
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

pub(in crate::handler::scripting_support) fn prompt_queue_action_from_result(
    result: super::super::super::prompt_support::PromptQueueResult,
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
