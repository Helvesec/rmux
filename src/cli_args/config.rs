use clap::{ArgAction, ArgGroup, Args};
use rmux_proto::HookName;
#[cfg(test)]
use rmux_proto::{ScopeSelector, SessionName};

use super::{parse_target_spec, TargetSpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetOptionCommandKind {
    SetOption,
    SetWindowOption,
}

impl SetOptionCommandKind {
    pub(crate) const fn command_name(self) -> &'static str {
        match self {
            Self::SetOption => "set-option",
            Self::SetWindowOption => "set-window-option",
        }
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct SetOptionArgs {
    #[arg(short = 'g', action = ArgAction::SetTrue)]
    pub(crate) global: bool,
    #[arg(short = 's', action = ArgAction::SetTrue)]
    pub(crate) server: bool,
    #[arg(short = 'w', action = ArgAction::SetTrue)]
    pub(crate) window: bool,
    #[arg(short = 'p', action = ArgAction::SetTrue)]
    pub(crate) pane: bool,
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) append: bool,
    #[arg(short = 'F', action = ArgAction::SetTrue)]
    pub(crate) format: bool,
    #[arg(short = 'o', action = ArgAction::SetTrue)]
    pub(crate) only_if_unset: bool,
    #[arg(short = 'q', action = ArgAction::SetTrue)]
    pub(crate) quiet: bool,
    #[arg(short = 'u', action = ArgAction::SetTrue)]
    pub(crate) unset: bool,
    #[arg(short = 'U', action = ArgAction::SetTrue)]
    pub(crate) unset_pane_overrides: bool,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    pub(crate) option: String,
    #[arg(allow_hyphen_values = true)]
    pub(crate) value: Option<String>,
}

impl SetOptionArgs {
    pub(crate) fn validate(self, kind: SetOptionCommandKind) -> Result<Self, clap::Error> {
        if matches!(kind, SetOptionCommandKind::SetOption)
            && [self.server, self.window, self.pane]
                .into_iter()
                .filter(|flag| *flag)
                .count()
                > 1
        {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "set-option accepts at most one of -s, -w, or -p",
            ));
        }

        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct SetWindowOptionArgs {
    #[arg(short = 'g', action = ArgAction::SetTrue)]
    global: bool,
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    append: bool,
    #[arg(short = 'F', action = ArgAction::SetTrue)]
    format: bool,
    #[arg(short = 'o', action = ArgAction::SetTrue)]
    only_if_unset: bool,
    #[arg(short = 'q', action = ArgAction::SetTrue)]
    quiet: bool,
    #[arg(short = 'u', action = ArgAction::SetTrue)]
    unset: bool,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    target: Option<TargetSpec>,
    option: String,
    #[arg(allow_hyphen_values = true)]
    value: Option<String>,
}

impl From<SetWindowOptionArgs> for SetOptionArgs {
    fn from(args: SetWindowOptionArgs) -> Self {
        Self {
            global: args.global,
            server: false,
            window: false,
            pane: false,
            append: args.append,
            format: args.format,
            only_if_unset: args.only_if_unset,
            quiet: args.quiet,
            unset: args.unset,
            unset_pane_overrides: false,
            target: args.target,
            option: args.option,
            value: args.value,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShowOptionsCommandKind {
    ShowOptions,
    ShowWindowOptions,
}

impl ShowOptionsCommandKind {
    pub(crate) const fn command_name(self) -> &'static str {
        match self {
            Self::ShowOptions => "show-options",
            Self::ShowWindowOptions => "show-window-options",
        }
    }
}

#[derive(Debug, Clone, Args)]
#[command(
    disable_help_flag = true,
    group(
        ArgGroup::new("scope")
            .required(false)
            .multiple(false)
            .args(["global", "target"])
    )
)]
pub(crate) struct SetEnvironmentArgs {
    #[arg(short = 'g', action = ArgAction::SetTrue, group = "scope")]
    pub(crate) global: bool,
    #[arg(short = 't', value_parser = parse_target_spec, group = "scope")]
    pub(crate) target: Option<TargetSpec>,
    #[arg(short = 'F', action = ArgAction::SetTrue)]
    pub(crate) format: bool,
    #[arg(short = 'h', action = ArgAction::SetTrue)]
    pub(crate) hidden: bool,
    #[arg(short = 'r', action = ArgAction::SetTrue)]
    pub(crate) clear: bool,
    #[arg(short = 'u', action = ArgAction::SetTrue)]
    pub(crate) unset: bool,
    pub(crate) name: String,
    #[arg(allow_hyphen_values = true)]
    pub(crate) value: Option<String>,
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("scope")
        .required(false)
        .multiple(false)
        .args(["server", "window", "pane"])
))]
pub(crate) struct ShowOptionsArgs {
    #[arg(short = 'A', action = ArgAction::SetTrue)]
    pub(crate) include_inherited: bool,
    #[arg(short = 'H', action = ArgAction::SetTrue)]
    pub(crate) include_hooks: bool,
    #[arg(short = 'g', action = ArgAction::SetTrue)]
    pub(crate) global: bool,
    #[arg(short = 's', action = ArgAction::SetTrue, group = "scope")]
    pub(crate) server: bool,
    #[arg(short = 'w', action = ArgAction::SetTrue, group = "scope")]
    pub(crate) window: bool,
    #[arg(short = 'p', action = ArgAction::SetTrue, group = "scope")]
    pub(crate) pane: bool,
    #[arg(short = 'q', action = ArgAction::SetTrue)]
    pub(crate) quiet: bool,
    #[arg(short = 'v', action = ArgAction::SetTrue)]
    pub(crate) value_only: bool,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(allow_hyphen_values = true)]
    pub(crate) name: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ShowWindowOptionsArgs {
    #[arg(short = 'g', action = ArgAction::SetTrue)]
    global: bool,
    #[arg(short = 'v', action = ArgAction::SetTrue)]
    value_only: bool,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    target: Option<TargetSpec>,
    #[arg(allow_hyphen_values = true)]
    name: Option<String>,
}

impl From<ShowWindowOptionsArgs> for ShowOptionsArgs {
    fn from(args: ShowWindowOptionsArgs) -> Self {
        Self {
            include_inherited: false,
            include_hooks: false,
            global: args.global,
            server: false,
            window: false,
            pane: false,
            quiet: false,
            value_only: args.value_only,
            target: args.target,
            name: args.name,
        }
    }
}

#[derive(Debug, Clone, Args)]
#[command(
    disable_help_flag = true,
    group(
        ArgGroup::new("scope")
            .required(false)
            .multiple(false)
            .args(["global", "target"])
    )
)]
pub(crate) struct ShowEnvironmentArgs {
    #[arg(short = 'g', action = ArgAction::SetTrue, group = "scope")]
    pub(crate) global: bool,
    #[arg(short = 't', value_parser = parse_target_spec, group = "scope")]
    pub(crate) target: Option<TargetSpec>,
    #[arg(short = 'h', action = ArgAction::SetTrue)]
    pub(crate) hidden: bool,
    #[arg(short = 's', action = ArgAction::SetTrue)]
    pub(crate) shell_format: bool,
    pub(crate) name: Option<String>,
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("scope")
        .required(false)
        .multiple(true)
        .args(["global", "target"])
))]
pub(crate) struct SetHookArgs {
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) append: bool,
    #[arg(short = 'g', action = ArgAction::SetTrue, group = "scope")]
    pub(crate) global: bool,
    #[arg(short = 'p', action = ArgAction::SetTrue)]
    pub(crate) pane: bool,
    #[arg(short = 'R', action = ArgAction::SetTrue)]
    pub(crate) run_immediately: bool,
    #[arg(short = 't', value_parser = parse_target_spec, group = "scope")]
    pub(crate) target: Option<TargetSpec>,
    #[arg(short = 'u', action = ArgAction::SetTrue)]
    pub(crate) unset: bool,
    #[arg(short = 'w', action = ArgAction::SetTrue)]
    pub(crate) window: bool,
    #[arg(value_parser = parse_hook_spec)]
    pub(crate) hook: ParsedHookSpec,
    pub(crate) command: Option<String>,
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("scope")
        .required(false)
        .multiple(true)
        .args(["global", "target"])
))]
pub(crate) struct ShowHooksArgs {
    #[arg(short = 'g', action = ArgAction::SetTrue, group = "scope")]
    pub(crate) global: bool,
    #[arg(short = 'p', action = ArgAction::SetTrue)]
    pub(crate) pane: bool,
    #[arg(short = 't', value_parser = parse_target_spec, group = "scope")]
    pub(crate) target: Option<TargetSpec>,
    #[arg(short = 'w', action = ArgAction::SetTrue)]
    pub(crate) window: bool,
    #[arg(value_parser = parse_hook_name)]
    pub(crate) hook: Option<HookName>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParsedHookSpec {
    pub(crate) hook: HookName,
    pub(crate) index: Option<u32>,
}

#[cfg(test)]
pub(crate) fn build_scope(global: bool, target: Option<SessionName>) -> ScopeSelector {
    match (global, target) {
        (true, None) => ScopeSelector::Global,
        (false, Some(session_name)) => ScopeSelector::Session(session_name),
        _ => unreachable!("clap scope group should enforce valid combinations"),
    }
}

fn parse_hook_spec(value: &str) -> Result<ParsedHookSpec, String> {
    let (name, index) = if let Some(open_bracket) = value.find('[') {
        let Some(index_text) = value[open_bracket + 1..].strip_suffix(']') else {
            return Err(format!("unknown hook: {value}"));
        };
        let index = index_text
            .parse::<u32>()
            .map_err(|_| format!("invalid hook index: {value}"))?;
        (&value[..open_bracket], Some(index))
    } else {
        (value, None)
    };

    Ok(ParsedHookSpec {
        hook: parse_hook_name(name)?,
        index,
    })
}

fn parse_hook_name(value: &str) -> Result<HookName, String> {
    HookName::from_str(value).ok_or_else(|| format!("invalid option: {value}"))
}
