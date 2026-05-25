use clap::{ArgAction, ArgGroup, Args};

use super::{parse_command_args, parse_target_spec, TargetSpec};

pub(crate) fn parse_web_share_args(arguments: Vec<String>) -> Result<WebShareArgs, clap::Error> {
    parse_command_args("web-share", normalize_web_share_args(arguments))
}

#[derive(Debug, Clone, Args)]
#[command(
    after_help = "Local web-share mode opens https://share.rmux.io against ws://127.0.0.1:<port>/share. It requires a browser that treats 127.0.0.1 as a secure context. Tested on: Chrome 130, Firefox 132, Safari 18, Edge 130. In-app webviews are not guaranteed."
)]
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

fn normalize_web_share_args(arguments: Vec<String>) -> Vec<String> {
    let Some((command, rest)) = arguments.split_first() else {
        return arguments;
    };
    match command.as_str() {
        "list" => prefixed("-l", rest),
        "stop" => normalize_stop(rest),
        "config" => prefixed("--config", rest),
        "lookup" => prefixed("--lookup", rest),
        _ => arguments,
    }
}

fn normalize_stop(rest: &[String]) -> Vec<String> {
    match rest.split_first() {
        Some((target, tail)) if target == "all" => prefixed("-X", tail),
        Some((target, tail)) => {
            let mut normalized = vec!["-K".to_owned(), target.clone()];
            normalized.extend_from_slice(tail);
            normalized
        }
        None => vec!["-K".to_owned()],
    }
}

fn prefixed(flag: &str, rest: &[String]) -> Vec<String> {
    let mut normalized = Vec::with_capacity(rest.len() + 1);
    normalized.push(flag.to_owned());
    normalized.extend_from_slice(rest);
    normalized
}
