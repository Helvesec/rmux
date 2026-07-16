use clap::{ArgAction, Args};
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
    #[arg(short = 'E', action = ArgAction::SetTrue)]
    pub(crate) skip_environment_update: bool,
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
    #[arg(short = 's', value_parser = parse_session_name)]
    pub(crate) session_name: Option<SessionName>,
    #[arg(short = 't', value_parser = parse_session_name)]
    pub(crate) group_target: Option<SessionName>,
    #[arg(short = 'X', action = ArgAction::SetTrue)]
    pub(crate) kill_other_clients: bool,
    #[arg(short = 'x', value_parser = parse_new_session_cols)]
    pub(crate) cols: Option<u16>,
    #[arg(short = 'y', value_parser = parse_new_session_rows)]
    pub(crate) rows: Option<u16>,
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) command: Vec<String>,
}

fn parse_new_session_cols(value: &str) -> Result<u16, String> {
    parse_new_session_dimension(value, "width")
}

fn parse_new_session_rows(value: &str) -> Result<u16, String> {
    parse_new_session_dimension(value, "height")
}

fn parse_new_session_dimension(value: &str, label: &str) -> Result<u16, String> {
    let parsed = value
        .parse::<i64>()
        .map_err(|_| format!("{label} invalid"))?;
    if parsed < 0 {
        return Err(format!("{label} too small"));
    }
    if parsed > i64::from(u16::MAX) {
        return Err(format!("{label} too large"));
    }
    u16::try_from(parsed).map_err(|_| format!("{label} invalid"))
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
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
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
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct KillSessionArgs {
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) kill_all_except_target: bool,
    #[arg(short = 'C', action = ArgAction::SetTrue)]
    pub(crate) clear_alerts: bool,
    #[arg(short = 'g', action = ArgAction::SetTrue)]
    pub(crate) kill_group: bool,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ServerAccessArgs {
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) add: bool,
    #[arg(short = 'd', action = ArgAction::SetTrue)]
    pub(crate) deny: bool,
    #[arg(short = 'l', action = ArgAction::SetTrue)]
    pub(crate) list: bool,
    #[arg(short = 'r', action = ArgAction::SetTrue)]
    pub(crate) read_only: bool,
    #[arg(short = 'w', action = ArgAction::SetTrue)]
    pub(crate) write: bool,
    pub(crate) user: Option<String>,
}

impl ServerAccessArgs {
    pub(super) fn validate(self) -> Result<Self, clap::Error> {
        if !self.list && self.add && self.deny {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "-a and -d cannot be used together",
            ));
        }
        if !self.list && self.read_only && self.write {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "-r and -w cannot be used together",
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct AlertSessionTargetArgs {
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) alerts_only: bool,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
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
    #[arg(short = 'F', conflicts_with = "json")]
    pub(crate) format: Option<String>,
    #[arg(short = 'f')]
    pub(crate) filter: Option<String>,
    #[arg(long = "json", action = ArgAction::SetTrue)]
    pub(crate) json: bool,
    #[arg(short = 'O', allow_hyphen_values = true)]
    pub(crate) sort_order: Option<String>,
    #[arg(short = 'r', action = ArgAction::SetTrue)]
    pub(crate) reversed: bool,
}

impl ListSessionsArgs {
    pub(crate) fn validate(self) -> Result<Self, clap::Error> {
        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct RenameSessionArgs {
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(value_parser = parse_session_name, allow_hyphen_values = true)]
    pub(crate) new_name: SessionName,
}
