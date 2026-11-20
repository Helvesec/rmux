use super::*;

pub(super) struct Delegation {
    pub(super) help: clap::Command,
    policy: ParsePolicy,
}

// Queue parsing precedes execution. Local delegates have distinct help rules.
enum ParsePolicy {
    Unsupported,
    HelpOnly,
    Capabilities,
}

pub(super) fn negotiated(name: &'static str) -> Delegation {
    Delegation {
        help: super::super::negotiated_pane::command(name),
        policy: ParsePolicy::HelpOnly,
    }
}

pub(super) fn capabilities(name: &'static str) -> Delegation {
    Delegation {
        help: empty_command(name)
            .arg(
                clap::Arg::new("human")
                    .long("human")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                clap::Arg::new("json")
                    .long("json")
                    .help(JSON_OUTPUT_HELP)
                    .action(ArgAction::SetTrue),
            ),
        policy: ParsePolicy::Capabilities,
    }
}

pub(super) fn claude(name: &'static str) -> Delegation {
    Delegation {
        help: empty_command(name).arg(
            clap::Arg::new("claude-args")
                .num_args(0..)
                .allow_hyphen_values(true)
                .trailing_var_arg(true),
        ),
        policy: ParsePolicy::Unsupported,
    }
}

pub(super) fn doctor(name: &'static str) -> Delegation {
    Delegation {
        help: empty_command(name).arg(
            clap::Arg::new("check")
                .value_name("tmux-dropin")
                .value_parser(["tmux-dropin"]),
        ),
        policy: ParsePolicy::Unsupported,
    }
}

pub(super) fn setup(name: &'static str) -> Delegation {
    Delegation {
        help: empty_command(name).arg(
            clap::Arg::new("action")
                .value_name("tmux-shim")
                .value_parser(["tmux-shim"]),
        ),
        policy: ParsePolicy::Unsupported,
    }
}

pub(super) fn parse(
    name: String,
    arguments: Vec<String>,
    delegation: Delegation,
) -> Result<Command, clap::Error> {
    match delegation.policy {
        ParsePolicy::Capabilities if arguments.iter().any(|arg| arg == "--help") => {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::DisplayHelp,
                "usage: rmux capabilities [--human|--json]\n",
            ));
        }
        ParsePolicy::HelpOnly if arguments == ["--help"] => {
            return Err(delegation
                .help
                .try_get_matches_from(["--help"])
                .expect_err("help request"));
        }
        _ => {}
    }
    Ok(Command::Unsupported(UnsupportedCommandArgs {
        name,
        arguments,
    }))
}
