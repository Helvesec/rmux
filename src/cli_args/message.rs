use clap::{ArgAction, Args};

use super::QueuedCommand;

#[derive(Debug, Clone, Args)]
pub(crate) struct DisplayMessageArgs {
    #[arg(short = 'c', allow_hyphen_values = true)]
    pub(crate) target_client: Option<String>,
    #[arg(short = 't', allow_hyphen_values = true)]
    pub(crate) target: Option<String>,
    /// Display the message for this many milliseconds; zero waits for input.
    #[arg(short = 'd', allow_hyphen_values = true)]
    pub(crate) delay: Option<String>,
    #[arg(short = 'F', allow_hyphen_values = true)]
    pub(crate) format: Option<String>,
    #[arg(short = 'a', action = ArgAction::SetTrue, conflicts_with = "json")]
    pub(crate) all_formats: bool,
    /// tmux's `-C` keeps pane updates flowing while the message is displayed.
    /// RMUX does not freeze pane output for status messages, so this flag is
    /// accepted for compatibility and has no additional runtime effect.
    #[arg(short = 'C', action = ArgAction::SetTrue)]
    pub(crate) no_freeze: bool,
    #[arg(short = 'I', action = ArgAction::SetTrue, conflicts_with = "json")]
    pub(crate) stdin: bool,
    #[arg(short = 'l', action = ArgAction::SetTrue, conflicts_with = "json")]
    pub(crate) literal: bool,
    /// Ignore key presses until a positive display delay expires.
    #[arg(short = 'N', action = ArgAction::SetTrue)]
    pub(crate) ignore_input: bool,
    #[arg(short = 'p', action = ArgAction::SetTrue, conflicts_with = "json")]
    pub(crate) print: bool,
    #[arg(short = 'v', action = ArgAction::SetTrue, conflicts_with = "json")]
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
        if let Some(delay) = self.delay.as_deref() {
            delay
                .parse::<rmux_proto::DisplayMessageDurationMillis>()
                .map_err(|error| {
                    clap::Error::raw(clap::error::ErrorKind::InvalidValue, error.to_string())
                })?;
        }
        if self.message.len() > 1 {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::TooManyValues,
                "command display-message: too many arguments (need at most 1)",
            ));
        }
        Ok(self)
    }
}
