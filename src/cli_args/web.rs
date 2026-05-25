use clap::{ArgAction, ArgGroup, Args};

use super::{parse_target_spec, TargetSpec};

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("mode")
        .required(false)
        .multiple(false)
        .args(["list", "stop", "stop_all", "lookup", "config"])
))]
pub(crate) struct WebShareArgs {
    #[arg(short = 'l', action = ArgAction::SetTrue, group = "mode")]
    pub(crate) list: bool,
    #[arg(short = 'K', value_name = "share-id", group = "mode")]
    pub(crate) stop: Option<String>,
    #[arg(short = 'X', action = ArgAction::SetTrue, group = "mode")]
    pub(crate) stop_all: bool,
    #[arg(long = "lookup", value_name = "share-id", group = "mode")]
    pub(crate) lookup: Option<String>,
    #[arg(long = "config", action = ArgAction::SetTrue, group = "mode")]
    pub(crate) config: bool,
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(short = 'w', long = "writable", action = ArgAction::SetTrue)]
    pub(crate) writable: bool,
    #[arg(long = "ttl", value_name = "seconds")]
    pub(crate) ttl_seconds: Option<u64>,
    #[arg(long = "max-viewers", value_name = "count")]
    pub(crate) max_viewers: Option<u16>,
    #[arg(long = "public-url", value_name = "url")]
    pub(crate) public_base_url: Option<String>,
}
