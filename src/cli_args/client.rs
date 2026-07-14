use clap::{ArgAction, ArgGroup, Args};

use super::{parse_target_spec, TargetSpec};

#[derive(Debug, Clone, Args)]
pub(crate) struct RefreshClientArgs {
    #[arg(short = 'c', action = ArgAction::SetTrue)]
    pub(crate) clear_pan: bool,
    #[arg(short = 'C')]
    pub(crate) control_size: Option<String>,
    #[arg(short = 'D', action = ArgAction::SetTrue)]
    pub(crate) pan_down: bool,
    #[arg(short = 'f')]
    pub(crate) flags: Option<String>,
    #[arg(short = 'F')]
    pub(crate) flags_alias: Option<String>,
    #[arg(short = 'l', action = ArgAction::SetTrue)]
    pub(crate) clipboard_query: bool,
    #[arg(short = 'L', action = ArgAction::SetTrue)]
    pub(crate) pan_left: bool,
    #[arg(short = 'R', action = ArgAction::SetTrue)]
    pub(crate) pan_right: bool,
    #[arg(short = 'S', action = ArgAction::SetTrue)]
    pub(crate) status_only: bool,
    #[arg(short = 't', allow_hyphen_values = true)]
    pub(crate) target_client: Option<String>,
    #[arg(short = 'U', action = ArgAction::SetTrue)]
    pub(crate) pan_up: bool,
    #[arg(allow_hyphen_values = true)]
    pub(crate) adjustment: Option<u32>,
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
