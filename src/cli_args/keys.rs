use clap::{ArgAction, Args};

use super::{parse_target_spec, TargetSpec};

#[derive(Debug, Clone, Args)]
pub(crate) struct SendKeysArgs {
    #[arg(short = 'F', action = ArgAction::SetTrue)]
    pub(crate) expand_formats: bool,
    #[arg(short = 'H', action = ArgAction::SetTrue)]
    pub(crate) hex: bool,
    #[arg(short = 'l', action = ArgAction::SetTrue)]
    pub(crate) literal: bool,
    #[arg(short = 'K', action = ArgAction::SetTrue)]
    pub(crate) key_table: bool,
    #[arg(short = 'M', action = ArgAction::SetTrue)]
    pub(crate) mouse: bool,
    #[arg(short = 'N')]
    pub(crate) repeat_count: Option<usize>,
    #[arg(short = 'p', action = ArgAction::SetTrue, hide = true)]
    pub(crate) unsupported_prefix: bool,
    #[arg(short = 'R', action = ArgAction::SetTrue)]
    pub(crate) reset_terminal: bool,
    #[arg(short = 'X', action = ArgAction::SetTrue)]
    pub(crate) copy_mode: bool,
    #[arg(short = 'c', allow_hyphen_values = true)]
    pub(crate) client_target: Option<String>,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) keys: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct BindKeyArgs {
    #[arg(short = 'n', action = ArgAction::SetTrue)]
    pub(crate) root_table: bool,
    #[arg(short = 'r', action = ArgAction::SetTrue)]
    pub(crate) repeat: bool,
    #[arg(short = 'N')]
    pub(crate) note: Option<String>,
    #[arg(short = 'T')]
    pub(crate) table_name: Option<String>,
    #[arg(allow_hyphen_values = true)]
    pub(crate) key: String,
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) command: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct UnbindKeyArgs {
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) all: bool,
    #[arg(short = 'n', action = ArgAction::SetTrue)]
    pub(crate) root_table: bool,
    #[arg(short = 'q', action = ArgAction::SetTrue)]
    pub(crate) quiet: bool,
    #[arg(short = 'T')]
    pub(crate) table_name: Option<String>,
    #[arg(allow_hyphen_values = true)]
    pub(crate) key: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ListKeysArgs {
    #[arg(short = '1', action = ArgAction::SetTrue)]
    pub(crate) first_only: bool,
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) include_unnoted: bool,
    #[arg(short = 'N', action = ArgAction::SetTrue)]
    pub(crate) notes: bool,
    #[arg(short = 'r', action = ArgAction::SetTrue, hide = true)]
    pub(crate) reversed: bool,
    #[arg(short = 'F', hide = true)]
    pub(crate) format: Option<String>,
    #[arg(short = 'O', hide = true)]
    pub(crate) sort_order: Option<String>,
    #[arg(short = 'P')]
    pub(crate) prefix: Option<String>,
    #[arg(short = 'T')]
    pub(crate) table_name: Option<String>,
    #[arg(allow_hyphen_values = true)]
    pub(crate) key: Option<String>,
}

impl ListKeysArgs {
    pub(crate) fn validate(self) -> Result<Self, clap::Error> {
        if self.reversed {
            return Err(unknown_flag_error("list-keys", "-r"));
        }
        if self.sort_order.is_some() {
            return Err(unknown_flag_error("list-keys", "-O"));
        }
        if self.format.is_some() {
            return Err(unknown_flag_error("list-keys", "-F"));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct SendPrefixArgs {
    #[arg(short = '2', action = ArgAction::SetTrue)]
    pub(crate) secondary: bool,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
}

impl BindKeyArgs {
    pub(crate) fn table_name(&self) -> String {
        if let Some(table_name) = &self.table_name {
            table_name.clone()
        } else if self.root_table {
            "root".to_owned()
        } else {
            "prefix".to_owned()
        }
    }
}

impl UnbindKeyArgs {
    pub(crate) fn table_name(&self) -> String {
        if let Some(table_name) = &self.table_name {
            table_name.clone()
        } else if self.root_table {
            "root".to_owned()
        } else {
            "prefix".to_owned()
        }
    }
}

fn unknown_flag_error(command_name: &str, flag: &str) -> clap::Error {
    clap::Error::raw(
        clap::error::ErrorKind::UnknownArgument,
        format!("command {command_name}: unknown flag {flag}"),
    )
}
