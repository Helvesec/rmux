use clap::{ArgAction, ArgGroup, Args};
use rmux_proto::SessionName;

use super::{parse_session_name, parse_target_spec, TargetSpec};

#[derive(Debug, Clone, Args)]
pub(crate) struct NewSessionArgs {
    #[arg(short = 'A', action = ArgAction::SetTrue)]
    pub(crate) attach_if_exists: bool,
    #[arg(short = 'c', allow_hyphen_values = true)]
    pub(crate) working_directory: Option<String>,
    #[arg(short = 'D', action = ArgAction::SetTrue)]
    pub(crate) detach_other_clients: bool,
    #[arg(short = 'd', action = ArgAction::SetTrue)]
    pub(crate) detached: bool,
    #[arg(short = 'e')]
    pub(crate) environment: Vec<String>,
    #[arg(short = 'f')]
    pub(crate) flags: Vec<String>,
    #[arg(short = 'F', allow_hyphen_values = true)]
    pub(crate) print_format: Option<String>,
    #[arg(short = 'n')]
    pub(crate) window_name: Option<String>,
    #[arg(short = 'P', action = ArgAction::SetTrue)]
    pub(crate) print_session_info: bool,
    /// Create the session in passthrough mode. The host terminal stays in
    /// the main screen buffer (no alt-screen toggle), inner-PTY bytes
    /// flow verbatim to the client, and pane operations are rejected.
    /// Window operations (`new-window`, `select-window`, …) still work.
    #[arg(long = "passthrough", action = ArgAction::SetTrue)]
    pub(crate) passthrough: bool,
    #[arg(short = 's', value_parser = parse_session_name)]
    pub(crate) session_name: Option<SessionName>,
    #[arg(short = 't', value_parser = parse_session_name)]
    pub(crate) group_target: Option<SessionName>,
    #[arg(short = 'X', action = ArgAction::SetTrue)]
    pub(crate) kill_other_clients: bool,
    #[arg(short = 'x')]
    pub(crate) cols: Option<u16>,
    #[arg(short = 'y')]
    pub(crate) rows: Option<u16>,
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) command: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct AttachSessionArgs {
    #[arg(short = 'c', allow_hyphen_values = true)]
    pub(crate) working_directory: Option<String>,
    #[arg(short = 'd', action = ArgAction::SetTrue)]
    pub(crate) detach_other_clients: bool,
    #[arg(short = 'E', action = ArgAction::SetTrue)]
    pub(crate) skip_environment_update: bool,
    #[arg(short = 'f')]
    pub(crate) flags: Vec<String>,
    #[arg(short = 'r', action = ArgAction::SetTrue)]
    pub(crate) read_only: bool,
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(short = 'x', action = ArgAction::SetTrue)]
    pub(crate) kill_other_clients: bool,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ClientTargetArgs {
    #[arg(short = 't', allow_hyphen_values = true)]
    pub(crate) target: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct SessionTargetArgs {
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct KillSessionArgs {
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) kill_all_except_target: bool,
    #[arg(short = 'C', action = ArgAction::SetTrue)]
    pub(crate) clear_alerts: bool,
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("access_action")
        .required(false)
        .multiple(false)
        .args(["add", "deny", "list"])
), group(
    ArgGroup::new("access_mode")
        .required(false)
        .multiple(false)
        .args(["read_only", "write"])
))]
pub(crate) struct ServerAccessArgs {
    #[arg(short = 'a', action = ArgAction::SetTrue, group = "access_action")]
    pub(crate) add: bool,
    #[arg(short = 'd', action = ArgAction::SetTrue, group = "access_action")]
    pub(crate) deny: bool,
    #[arg(short = 'l', action = ArgAction::SetTrue, group = "access_action")]
    pub(crate) list: bool,
    #[arg(short = 'r', action = ArgAction::SetTrue, group = "access_mode")]
    pub(crate) read_only: bool,
    #[arg(short = 'w', action = ArgAction::SetTrue, group = "access_mode")]
    pub(crate) write: bool,
    #[arg(allow_hyphen_values = true)]
    pub(crate) user: Option<String>,
}

impl ServerAccessArgs {
    pub(super) fn validate(self) -> Result<Self, clap::Error> {
        if self.list {
            if self.user.is_some() {
                return Err(clap::Error::raw(
                    clap::error::ErrorKind::TooManyValues,
                    "server-access -l does not accept a user argument",
                ));
            }
            return Ok(self);
        }

        if self.user.is_none() {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::MissingRequiredArgument,
                "missing user argument",
            ));
        }

        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct AlertSessionTargetArgs {
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) alerts_only: bool,
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ShowMessagesArgs {
    #[arg(short = 'J', action = ArgAction::SetTrue)]
    pub(crate) jobs: bool,
    #[arg(short = 'T', action = ArgAction::SetTrue)]
    pub(crate) terminals: bool,
    #[arg(short = 't')]
    pub(crate) target_client: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ListSessionsArgs {
    #[arg(short = 'F')]
    pub(crate) format: Option<String>,
    #[arg(short = 'f')]
    pub(crate) filter: Option<String>,
    #[arg(short = 'O')]
    pub(crate) sort_order: Option<String>,
    #[arg(short = 'r', action = ArgAction::SetTrue)]
    pub(crate) reversed: bool,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct RenameSessionArgs {
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(value_parser = parse_session_name, allow_hyphen_values = true)]
    pub(crate) new_name: SessionName,
}
