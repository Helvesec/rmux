use clap::{ArgAction, Args};

use super::{parse_target_spec, TargetSpec};

#[derive(Debug, Clone, Args)]
pub(crate) struct SetBufferArgs {
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) append: bool,
    #[arg(short = 'b')]
    pub(crate) name: Option<String>,
    #[arg(short = 'n')]
    pub(crate) new_name: Option<String>,
    #[arg(short = 't', allow_hyphen_values = true)]
    pub(crate) target_client: Option<String>,
    #[arg(short = 'w', action = ArgAction::SetTrue)]
    pub(crate) set_clipboard: bool,
    #[arg()]
    pub(crate) content: Option<String>,
}

impl SetBufferArgs {
    pub(crate) fn validate(self) -> Result<Self, clap::Error> {
        if self.content.is_none() && self.new_name.is_none() {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::MissingRequiredArgument,
                "set-buffer requires content or -n new-name",
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ShowBufferArgs {
    #[arg(short = 'b')]
    pub(crate) name: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct PasteBufferArgs {
    #[arg(short = 'b')]
    pub(crate) name: Option<String>,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(short = 'd', action = ArgAction::SetTrue)]
    pub(crate) delete_after: bool,
    #[arg(short = 'p', action = ArgAction::SetTrue)]
    pub(crate) bracketed: bool,
    #[arg(short = 'r', action = ArgAction::SetTrue)]
    pub(crate) linefeed: bool,
    #[arg(short = 'S', action = ArgAction::SetTrue)]
    pub(crate) raw: bool,
    #[arg(short = 's', allow_hyphen_values = true)]
    pub(crate) separator: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct DeleteBufferArgs {
    #[arg(short = 'b')]
    pub(crate) name: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct LoadBufferArgs {
    #[arg(short = 'b')]
    pub(crate) name: Option<String>,
    #[arg(short = 't', allow_hyphen_values = true)]
    pub(crate) target_client: Option<String>,
    #[arg(short = 'w', action = ArgAction::SetTrue)]
    pub(crate) set_clipboard: bool,
    #[arg(allow_hyphen_values = true)]
    pub(crate) path: String,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct SaveBufferArgs {
    #[arg(short = 'b')]
    pub(crate) name: Option<String>,
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) append: bool,
    #[arg(allow_hyphen_values = true)]
    pub(crate) path: String,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ListBuffersArgs {
    #[arg(short = 'F')]
    pub(crate) format: Option<String>,
    #[arg(short = 'f')]
    pub(crate) filter: Option<String>,
    #[arg(short = 'O')]
    pub(crate) sort_order: Option<String>,
    #[arg(short = 'r', action = ArgAction::SetTrue, hide = true)]
    pub(crate) reversed: bool,
}
