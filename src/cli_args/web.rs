use clap::{ArgAction, ArgGroup, Args, ValueEnum};

use super::{parse_command_args, parse_target_spec, TargetSpec};

pub(crate) const WEB_SHARE_TUNNEL_PROVIDERS: &[&str] = &[
    "localhost-run",
    "sandhole",
    "serveo",
    "srv-us",
    "tailscale-funnel",
    "tailscale-serve",
];

pub(crate) fn parse_web_share_args(arguments: Vec<String>) -> Result<WebShareArgs, clap::Error> {
    parse_command_args("web-share", normalize_web_share_args(arguments))
}

#[derive(Debug, Clone, Args)]
#[command(
    after_help = WEB_SHARE_AFTER_HELP
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
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(long = "operator-only", action = ArgAction::SetTrue, conflicts_with = "spectator_only")]
    pub(crate) operator_only: bool,
    #[arg(long = "spectator-only", action = ArgAction::SetTrue, conflicts_with = "operator_only")]
    pub(crate) spectator_only: bool,
    #[arg(long = "ttl", value_name = "seconds")]
    pub(crate) ttl_seconds: Option<u64>,
    #[arg(long = "expires-at", value_name = "RFC3339")]
    pub(crate) expires_at: Option<String>,
    #[arg(long = "kill-session-on-expire", action = ArgAction::SetTrue)]
    pub(crate) kill_session_on_expire: bool,
    #[arg(long = "max-spectators", value_name = "count")]
    pub(crate) max_spectators: Option<u16>,
    #[arg(long = "max-operators", value_name = "count")]
    pub(crate) max_operators: Option<u16>,
    #[arg(long = "frontend-url", alias = "web-frontend", value_name = "url")]
    pub(crate) frontend_url: Option<String>,
    #[arg(
        long = "tunnel-url",
        alias = "public-url",
        value_name = "url",
        conflicts_with = "tunnel_provider"
    )]
    pub(crate) public_base_url: Option<String>,
    #[arg(
        long = "tunnel-provider",
        value_name = "provider",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with = "public_base_url"
    )]
    pub(crate) tunnel_provider: Option<String>,
    #[arg(long = "no-navbar", action = ArgAction::SetTrue)]
    pub(crate) no_navbar: bool,
    #[arg(long = "no-disclaimer", action = ArgAction::SetTrue)]
    pub(crate) no_disclaimer: bool,
    #[arg(long = "hide-viewers", alias = "hide-viewer-count", action = ArgAction::SetTrue)]
    pub(crate) hide_viewers: bool,
    #[arg(
        long = "show-viewers",
        alias = "show-viewer-count",
        action = ArgAction::SetTrue,
        hide = true,
        conflicts_with = "hide_viewers"
    )]
    pub(crate) show_viewers: bool,
    #[arg(
        long = "theme",
        alias = "terminal-theme",
        value_enum,
        value_name = "user|light|dark"
    )]
    pub(crate) terminal_theme: Option<WebShareTerminalThemeArg>,
    #[arg(long = "no-pin", action = ArgAction::SetTrue)]
    pub(crate) no_pin: bool,
    #[arg(
        long = "pin-operator",
        value_name = "PIN",
        conflicts_with_all = ["no_pin", "spectator_only"]
    )]
    pub(crate) pin_operator: Option<String>,
    #[arg(
        long = "pin-spectator",
        value_name = "PIN",
        conflicts_with_all = ["no_pin", "operator_only"]
    )]
    pub(crate) pin_spectator: Option<String>,
    #[arg(
        long = "pin",
        alias = "pairing-code",
        action = ArgAction::SetTrue,
        hide = true,
        conflicts_with = "no_pin"
    )]
    pub(crate) pin: bool,
}

const WEB_SHARE_AFTER_HELP: &str = "\
Notes:
  -t accepts a pane target or a session name.
  Pane targets expose one pane; session targets expose the attached session view.
  By default rmux mints a private operator URL and a spectator URL.
  Use --operator-only or --spectator-only to restrict roles.
  Pairing PINs are required by default; use --no-pin to disable them.
  Use --pin-operator PIN and --pin-spectator PIN to supply 6-digit role PINs.
  Use web-share stop <share-id> to revoke an active share.
  Use --tunnel-provider NAME for internet access, or --tunnel-url for your own endpoint.
  Available tunnel providers: localhost-run, sandhole, serveo, srv-us, tailscale-funnel, tailscale-serve.
  Use --frontend-url to host your own static frontend.
";

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
