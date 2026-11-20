use clap::{ArgAction, ArgGroup, Args};

use super::{parse_target_spec, TargetSpec};

#[derive(Debug, Clone, Args)]
pub(crate) struct RefreshClientArgs {
    #[arg(short = 'c', action = ArgAction::SetTrue)]
    pub(crate) clear_pan: bool,
    #[arg(short = 'D', action = ArgAction::SetTrue)]
    pub(crate) pan_down: bool,
    #[arg(short = 'L', action = ArgAction::SetTrue)]
    pub(crate) pan_left: bool,
    #[arg(short = 'R', action = ArgAction::SetTrue)]
    pub(crate) pan_right: bool,
    #[arg(short = 'U', action = ArgAction::SetTrue)]
    pub(crate) pan_up: bool,
    #[arg(value_name = "adjustment")]
    pub(crate) adjustment: Option<String>,
    #[arg(short = 'A', action = ArgAction::Append, allow_hyphen_values = true)]
    pub(crate) subscriptions: Vec<String>,
    #[arg(short = 'B', action = ArgAction::Append, allow_hyphen_values = true)]
    pub(crate) subscriptions_format: Vec<String>,
    #[arg(short = 'r', allow_hyphen_values = true)]
    pub(crate) colour_report: Option<String>,
    /// Global WxH or W,H; window @id:WxH; @id: removes its size.
    #[arg(short = 'C', value_name = "size")]
    pub(crate) control_size: Option<String>,
    #[arg(short = 'f')]
    pub(crate) flags: Option<String>,
    #[arg(short = 'F')]
    pub(crate) flags_alias: Option<String>,
    #[arg(short = 'l', action = ArgAction::SetTrue)]
    pub(crate) clipboard_query: bool,
    #[arg(short = 'S', action = ArgAction::SetTrue)]
    pub(crate) status_only: bool,
    #[arg(short = 't', allow_hyphen_values = true)]
    pub(crate) target_client: Option<String>,
}

impl RefreshClientArgs {
    pub(super) fn validate(self) -> Result<Self, clap::Error> {
        if self.clear_pan || self.pan_left || self.pan_right || self.pan_up || self.pan_down {
            return Ok(self);
        }
        if let Some(size) = self.control_size.as_deref() {
            rmux_core::command_parser::ControlClientSize::parse(size).map_err(|error| {
                clap::Error::raw(
                    clap::error::ErrorKind::ValueValidation,
                    format!("invalid value '{size}' for '-C <size>': {error}"),
                )
            })?;
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ListClientsArgs {
    #[arg(short = 'F', conflicts_with = "json")]
    pub(crate) format: Option<String>,
    #[arg(short = 'f')]
    pub(crate) filter: Option<String>,
    #[arg(long = "json", action = ArgAction::SetTrue)]
    pub(crate) json: bool,
    #[arg(short = 'O')]
    pub(crate) sort_order: Option<String>,
    #[arg(short = 'r', action = ArgAction::SetTrue, hide = true)]
    pub(crate) reversed: bool,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target_session: Option<TargetSpec>,
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("selector")
        .required(false)
        .multiple(false)
        .args(["target", "last_session", "next_session", "previous_session"])
))]
pub(crate) struct SwitchClientArgs {
    #[arg(short = 'c', allow_hyphen_values = true)]
    pub(crate) target_client: Option<String>,
    #[arg(short = 'E', action = ArgAction::SetTrue)]
    pub(crate) skip_environment_update: bool,
    #[arg(short = 'l', action = ArgAction::SetTrue, group = "selector")]
    pub(crate) last_session: bool,
    #[arg(short = 'n', action = ArgAction::SetTrue, group = "selector")]
    pub(crate) next_session: bool,
    #[arg(short = 'O')]
    pub(crate) sort_order: Option<String>,
    #[arg(short = 'p', action = ArgAction::SetTrue, group = "selector")]
    pub(crate) previous_session: bool,
    #[arg(short = 'r', action = ArgAction::SetTrue)]
    pub(crate) toggle_read_only: bool,
    #[arg(short = 'T')]
    pub(crate) key_table: Option<String>,
    #[arg(short = 't', allow_hyphen_values = true, group = "selector")]
    pub(crate) target: Option<String>,
    #[arg(short = 'Z', action = ArgAction::SetTrue)]
    pub(crate) zoom: bool,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct DetachClientArgs {
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) all_other_clients: bool,
    #[arg(short = 'E', allow_hyphen_values = true)]
    pub(crate) exec_command: Option<String>,
    #[arg(short = 'P', action = ArgAction::SetTrue)]
    pub(crate) kill_on_detach: bool,
    #[arg(short = 's', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target_session: Option<TargetSpec>,
    #[arg(short = 't', allow_hyphen_values = true)]
    pub(crate) target_client: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct SuspendClientArgs {
    #[arg(short = 't', allow_hyphen_values = true)]
    pub(crate) target_client: Option<String>,
}
