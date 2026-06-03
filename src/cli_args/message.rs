use clap::{ArgAction, Args};

use super::QueuedCommand;

#[derive(Debug, Clone, Args)]
pub(crate) struct DisplayMessageArgs {
    #[arg(short = 'c', allow_hyphen_values = true)]
    pub(crate) target_client: Option<String>,
    #[arg(short = 't', allow_hyphen_values = true)]
    pub(crate) target: Option<String>,
    #[arg(short = 'p', action = ArgAction::SetTrue)]
    pub(crate) print: bool,
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) message: Vec<String>,
    #[arg(skip = String::new())]
    pub(crate) queue_command: String,
}

impl QueuedCommand for DisplayMessageArgs {
    fn set_queue_command(&mut self, queue_command: String) {
        self.queue_command = queue_command;
    }
}
