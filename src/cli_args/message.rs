use clap::{ArgAction, Args};

use super::QueuedCommand;

#[derive(Debug, Clone, Args)]
pub(crate) struct DisplayMessageArgs {
    #[arg(short = 'c', allow_hyphen_values = true)]
    pub(crate) target_client: Option<String>,
    #[arg(short = 't', allow_hyphen_values = true)]
    pub(crate) target: Option<String>,
    #[arg(short = 'd', allow_hyphen_values = true)]
    pub(crate) delay: Option<String>,
    #[arg(short = 'F', allow_hyphen_values = true)]
    pub(crate) format: Option<String>,
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) all_formats: bool,
    #[arg(short = 'I', action = ArgAction::SetTrue)]
    pub(crate) stdin: bool,
    #[arg(short = 'l', action = ArgAction::SetTrue)]
    pub(crate) literal: bool,
    #[arg(short = 'N', action = ArgAction::SetTrue)]
    pub(crate) no_format: bool,
    #[arg(short = 'p', action = ArgAction::SetTrue, conflicts_with = "json")]
    pub(crate) print: bool,
    #[arg(short = 'v', action = ArgAction::SetTrue)]
    pub(crate) verbose: bool,
    #[arg(long = "json", action = ArgAction::SetTrue)]
    pub(crate) json: bool,
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

impl DisplayMessageArgs {
    pub(crate) fn validate(self) -> Result<Self, clap::Error> {
        if self.message.len() > 1 {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::TooManyValues,
                "command display-message: too many arguments (need at most 1)",
            ));
        }
        Ok(self)
    }
}
