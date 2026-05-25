use clap::{ArgAction, ArgGroup, Args, ValueEnum};

use super::{parse_command_args, parse_target_spec, TargetSpec};

pub(crate) fn parse_web_share_args(arguments: Vec<String>) -> Result<WebShareArgs, clap::Error> {
    parse_command_args("web-share", normalize_web_share_args(arguments))
}

#[derive(Debug, Clone, Args)]
#[command(
    after_help = "Local web-share mode opens https://share.rmux.io/ against ws://127.0.0.1:<port>/share. -t accepts a pane target or a session name; pane targets expose one pane, session targets expose the attached session view. Pass -w for operator input. Pass --controls with -w on a session target to allow whitelisted rmux commands. Use `web-share disconnect <share-id>` to revoke one active share without stopping the rmux daemon. Pass --tunnel-url for a bring-your-own public endpoint. Pass --frontend-url to use a self-hosted frontend. Pass --theme user|light|dark to choose the initial browser terminal palette. Pass --pin to require an out-of-band pairing code. Chromium-based browsers may require allowing Local Network access for local mode. In-app webviews are not guaranteed."
)]
#[command(group(
    ArgGroup::new("mode")
        .required(false)
        .multiple(false)
        .args(["list", "stop", "disconnect", "stop_all", "lookup", "config"])
))]
pub(crate) struct WebShareArgs {
    #[arg(short = 'l', action = ArgAction::SetTrue, group = "mode")]
    pub(crate) list: bool,
    #[arg(short = 'K', value_name = "share-id", group = "mode")]
    pub(crate) stop: Option<String>,
    #[arg(long = "disconnect", value_name = "share-id", group = "mode")]
    pub(crate) disconnect: Option<String>,
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
    #[arg(long = "controls", action = ArgAction::SetTrue)]
    pub(crate) controls: bool,
    #[arg(long = "ttl", value_name = "seconds")]
    pub(crate) ttl_seconds: Option<u64>,
    #[arg(long = "max-readers", value_name = "count")]
    pub(crate) max_readers: Option<u16>,
    #[arg(long = "frontend-url", alias = "web-frontend", value_name = "url")]
    pub(crate) frontend_url: Option<String>,
    #[arg(long = "tunnel-url", alias = "public-url", value_name = "url")]
    pub(crate) public_base_url: Option<String>,
    #[arg(long = "no-navbar", action = ArgAction::SetTrue)]
    pub(crate) no_navbar: bool,
    #[arg(long = "no-disclaimer", action = ArgAction::SetTrue)]
    pub(crate) no_disclaimer: bool,
    #[arg(
        long = "theme",
        alias = "terminal-theme",
        value_enum,
        value_name = "user|light|dark"
    )]
    pub(crate) terminal_theme: Option<WebShareTerminalThemeArg>,
    #[arg(long = "pin", alias = "pairing-code", action = ArgAction::SetTrue)]
    pub(crate) require_pin: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum WebShareTerminalThemeArg {
    User,
    Light,
    Dark,
}

fn normalize_web_share_args(arguments: Vec<String>) -> Vec<String> {
    let Some((command, rest)) = arguments.split_first() else {
        return arguments;
    };
    match command.as_str() {
        "list" => prefixed("-l", rest),
        "stop" => normalize_stop(rest),
        "disconnect" => normalize_disconnect(rest),
        "off" => prefixed("-X", rest),
        "config" => prefixed("--config", rest),
        "lookup" => prefixed("--lookup", rest),
        _ => arguments,
    }
}

fn normalize_disconnect(rest: &[String]) -> Vec<String> {
    match rest.split_first() {
        Some((target, tail)) => {
            let mut normalized = vec!["--disconnect".to_owned(), target.clone()];
            normalized.extend_from_slice(tail);
            normalized
        }
        None => vec!["--disconnect".to_owned()],
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
