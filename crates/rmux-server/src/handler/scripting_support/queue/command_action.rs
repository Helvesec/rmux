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
    let (output, exit_status) = match response {
        Response::Error(ErrorResponse { error }) => return Err(error),
        Response::RunShell(response) => (response.output, response.exit_status),
        Response::CapturePane(response) => (response.output, None),
        Response::DisplayMessage(response) => (response.output, None),
        response => (response.command_output().cloned(), None),
    };
    Ok(QueueCommandAction::Normal {
        output: output.filter(|output| !output.stdout().is_empty()),
        error: None,
        source_file_error: None,
        exit_status,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_proto::{CapturePaneResponse, DisplayMessageResponse, RunShellResponse};

    #[test]
    fn consumed_hot_response_preserves_allocation_bytes_and_status() {
        for kind in 0..3 {
            let output = CommandOutput::from_stdout(vec![b'x'; 210_000]);
            let pointer = output.stdout.as_ptr();
            let response = match kind {
                0 => Response::CapturePane(CapturePaneResponse::from_output(output)),
                1 => Response::DisplayMessage(DisplayMessageResponse::from_output(output)),
                _ => Response::RunShell(RunShellResponse::from_output_and_exit_status(output, 7)),
            };
            let QueueCommandAction::Normal {
                output: Some(output),
                exit_status,
                error,
                source_file_error,
            } = queue_action_from_response(response).unwrap()
            else {
                panic!("missing output")
            };
            assert_eq!(output.stdout.as_ptr(), pointer);
            assert_eq!(output.stdout, vec![b'x'; 210_000]);
            assert_eq!(exit_status, (kind == 2).then_some(7));
            assert!(error.is_none() && source_file_error.is_none());
        }
    }

    #[test]
    fn consumed_response_preserves_empty_output_and_errors() {
        let response = Response::RunShell(RunShellResponse::from_output_and_exit_status(
            CommandOutput::from_stdout(Vec::new()),
            9,
        ));
        assert!(matches!(
            queue_action_from_response(response).unwrap(),
            QueueCommandAction::Normal {
                output: None,
                exit_status: Some(9),
                ..
            }
        ));
        let error = RmuxError::Server("queue error".into());
        assert_eq!(
            queue_action_from_response(Response::Error(ErrorResponse {
                error: error.clone()
            }))
            .unwrap_err(),
            error
        );
        assert!(matches!(
            queue_action_from_response(Response::DisplayMessage(
                DisplayMessageResponse::no_output()
            ))
            .unwrap(),
            QueueCommandAction::Normal {
                output: None,
                exit_status: None,
                ..
            }
        ));
    }
}
