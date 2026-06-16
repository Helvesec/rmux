use clap::{ArgAction, Args};

use super::QueuedCommand;

/// Arguments for `display-menu` / alias `menu`.
#[derive(Debug, Clone, Args)]
pub(crate) struct DisplayMenuArgs {
    #[arg(short = 'M', action = ArgAction::SetTrue)]
    pub(crate) mouse: bool,
    #[arg(short = 'O', action = ArgAction::SetTrue)]
    pub(crate) select_open: bool,
    #[arg(short = 'b', allow_hyphen_values = true)]
    pub(crate) border_lines: Option<String>,
    #[arg(short = 'c', allow_hyphen_values = true)]
    pub(crate) target_client: Option<String>,
    #[arg(short = 'C', allow_hyphen_values = true)]
    pub(crate) starting_choice: Option<String>,
    #[arg(short = 'H', allow_hyphen_values = true)]
    pub(crate) selected_style: Option<String>,
    #[arg(short = 's', allow_hyphen_values = true)]
    pub(crate) style: Option<String>,
    #[arg(short = 'S', allow_hyphen_values = true)]
    pub(crate) border_style: Option<String>,
    #[arg(short = 't', allow_hyphen_values = true)]
    pub(crate) target: Option<String>,
    #[arg(short = 'T', allow_hyphen_values = true)]
    pub(crate) title: Option<String>,
    #[arg(short = 'x', allow_hyphen_values = true)]
    pub(crate) x: Option<String>,
    #[arg(short = 'y', allow_hyphen_values = true)]
    pub(crate) y: Option<String>,
    /// Raw menu item triplets `(label, key, command)` supplied positionally.
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) items: Vec<String>,
    #[arg(skip = String::new())]
    pub(crate) queue_command: String,
}

/// Arguments for `display-popup` / alias `popup`.
#[derive(Debug, Clone, Args)]
pub(crate) struct DisplayPopupArgs {
    #[arg(short = 'B', action = ArgAction::SetTrue)]
    pub(crate) no_border: bool,
    #[arg(short = 'C', action = ArgAction::SetTrue)]
    pub(crate) close_all: bool,
    #[arg(short = 'E', action = ArgAction::Count)]
    pub(crate) close_on_exit: u8,
    #[arg(short = 'k', action = ArgAction::SetTrue)]
    pub(crate) close_on_key: bool,
    #[arg(short = 'N', action = ArgAction::SetTrue)]
    pub(crate) no_title_border: bool,
    #[arg(short = 'b', allow_hyphen_values = true)]
    pub(crate) border_lines: Option<String>,
    #[arg(short = 'c', allow_hyphen_values = true)]
    pub(crate) target_client: Option<String>,
    #[arg(short = 'd', allow_hyphen_values = true)]
    pub(crate) start_directory: Option<String>,
    #[arg(short = 'e', allow_hyphen_values = true)]
    pub(crate) environment: Vec<String>,
    #[arg(short = 'h', allow_hyphen_values = true)]
    pub(crate) height: Option<String>,
    #[arg(short = 's', allow_hyphen_values = true)]
    pub(crate) style: Option<String>,
    #[arg(short = 'S', allow_hyphen_values = true)]
    pub(crate) border_style: Option<String>,
    #[arg(short = 't', allow_hyphen_values = true)]
    pub(crate) target: Option<String>,
    #[arg(short = 'T', allow_hyphen_values = true)]
    pub(crate) title: Option<String>,
    #[arg(short = 'w', allow_hyphen_values = true)]
    pub(crate) width: Option<String>,
    #[arg(short = 'x', allow_hyphen_values = true)]
    pub(crate) x: Option<String>,
    #[arg(short = 'y', allow_hyphen_values = true)]
    pub(crate) y: Option<String>,
    /// Optional shell command and arguments for the popup body.
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) shell_command: Vec<String>,
    #[arg(skip = String::new())]
    pub(crate) queue_command: String,
}

impl QueuedCommand for DisplayMenuArgs {
    fn set_queue_command(&mut self, queue_command: String) {
        self.queue_command = queue_command;
    }
}

impl QueuedCommand for DisplayPopupArgs {
    fn set_queue_command(&mut self, queue_command: String) {
        self.queue_command = queue_command;
    }
}
