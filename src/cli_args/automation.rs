use std::time::Duration;

use clap::{ArgAction, Args};

use super::{parse_session_name, parse_target_spec, TargetSpec};

#[derive(Debug, Clone, Args)]
pub(crate) struct WaitPaneArgs {
    #[arg(short = 't', long = "target", value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(long = "text")]
    pub(crate) text: Option<String>,
    #[arg(long = "next-text")]
    pub(crate) next_text: Option<String>,
    #[arg(long = "visible-text")]
    pub(crate) visible_text: Option<String>,
    #[arg(long = "quiet", action = ArgAction::SetTrue)]
    pub(crate) quiet: bool,
    #[arg(long = "stable-for", value_parser = parse_duration)]
    pub(crate) stable_for: Option<Duration>,
    #[arg(long = "pane-exit", action = ArgAction::SetTrue)]
    pub(crate) pane_exit: bool,
    #[arg(long = "timeout", value_parser = parse_duration)]
    pub(crate) timeout: Option<Duration>,
    #[arg(long = "json", action = ArgAction::SetTrue)]
    pub(crate) json: bool,
    #[arg(long = "get-by-text")]
    pub(crate) get_by_text: Option<String>,
}

impl WaitPaneArgs {
    pub(crate) fn validate(self) -> Result<Self, clap::Error> {
        let conditions = [
            self.text.is_some(),
            self.next_text.is_some(),
            self.visible_text.is_some(),
            self.quiet,
            self.pane_exit,
            self.get_by_text.is_some(),
        ]
        .into_iter()
        .filter(|selected| *selected)
        .count();
        if conditions != 1 {
            return Err(value_error(
                "wait-pane",
                "exactly one wait condition is required",
            ));
        }
        reject_empty("wait-pane", "--text", self.text.as_deref())?;
        reject_empty("wait-pane", "--next-text", self.next_text.as_deref())?;
        reject_empty("wait-pane", "--visible-text", self.visible_text.as_deref())?;
        reject_empty("wait-pane", "--get-by-text", self.get_by_text.as_deref())?;
        if self.stable_for.is_some() && !self.quiet {
            return Err(value_error(
                "wait-pane",
                "--stable-for is valid only with --quiet",
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct PaneSnapshotArgs {
    #[arg(short = 't', long = "target", value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(long = "json", action = ArgAction::SetTrue)]
    pub(crate) json: bool,
    #[arg(long = "style", action = ArgAction::SetTrue)]
    pub(crate) style: bool,
    #[arg(long = "region", value_parser = parse_region)]
    pub(crate) region: Option<SnapshotRegion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SnapshotRegion {
    pub(crate) row: u16,
    pub(crate) col: u16,
    pub(crate) rows: u16,
    pub(crate) cols: u16,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct StreamPaneArgs {
    #[arg(short = 't', long = "target", value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(long = "raw", action = ArgAction::SetTrue)]
    pub(crate) raw: bool,
    #[arg(long = "lines", action = ArgAction::SetTrue)]
    pub(crate) lines: bool,
}

impl StreamPaneArgs {
    pub(crate) fn validate(self) -> Result<Self, clap::Error> {
        if self.raw && self.lines {
            return Err(value_error(
                "stream-pane",
                "--raw and --lines are mutually exclusive",
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct CollectPaneOutputArgs {
    #[arg(short = 't', long = "target", value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(long = "until-pane-exit", action = ArgAction::SetTrue)]
    pub(crate) until_pane_exit: bool,
    #[arg(long = "max-bytes", value_parser = parse_positive_usize)]
    pub(crate) max_bytes: usize,
    #[arg(long = "json", action = ArgAction::SetTrue)]
    pub(crate) json: bool,
}

impl CollectPaneOutputArgs {
    pub(crate) fn validate(self) -> Result<Self, clap::Error> {
        if !self.until_pane_exit {
            return Err(value_error(
                "collect-pane-output",
                "--until-pane-exit is required",
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct LocatorArgs {
    #[arg(short = 't', long = "target", value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(long = "get-by-text")]
    pub(crate) get_by_text: String,
    #[arg(long = "json", action = ArgAction::SetTrue)]
    pub(crate) json: bool,
}

impl LocatorArgs {
    pub(crate) fn validate(self) -> Result<Self, clap::Error> {
        reject_empty("locator", "--get-by-text", Some(&self.get_by_text))?;
        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ExpectPaneArgs {
    #[arg(short = 't', long = "target", value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(long = "get-by-text")]
    pub(crate) get_by_text: String,
    #[arg(long = "visible", action = ArgAction::SetTrue)]
    pub(crate) visible: bool,
    #[arg(long = "hidden", action = ArgAction::SetTrue)]
    pub(crate) hidden: bool,
    #[arg(long = "count")]
    pub(crate) count: Option<usize>,
    #[arg(long = "json", action = ArgAction::SetTrue)]
    pub(crate) json: bool,
}

impl ExpectPaneArgs {
    pub(crate) fn validate(self) -> Result<Self, clap::Error> {
        reject_empty("expect-pane", "--get-by-text", Some(&self.get_by_text))?;
        let assertions = [self.visible, self.hidden, self.count.is_some()]
            .into_iter()
            .filter(|selected| *selected)
            .count();
        if assertions != 1 {
            return Err(value_error(
                "expect-pane",
                "exactly one assertion is required",
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct FindPanesArgs {
    #[arg(long = "title")]
    pub(crate) title: Option<String>,
    #[arg(long = "title-prefix")]
    pub(crate) title_prefix: Option<String>,
    #[arg(long = "current-command")]
    pub(crate) current_command: Option<String>,
    #[arg(long = "cwd")]
    pub(crate) cwd: Option<String>,
    #[arg(long = "json", action = ArgAction::SetTrue)]
    pub(crate) json: bool,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct FindSessionsArgs {
    #[arg(long = "name")]
    pub(crate) name: Option<String>,
    #[arg(long = "name-prefix")]
    pub(crate) name_prefix: Option<String>,
    #[arg(long = "json", action = ArgAction::SetTrue)]
    pub(crate) json: bool,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct BroadcastKeysArgs {
    #[arg(short = 't', long = "target", value_parser = parse_target_spec, allow_hyphen_values = true, action = ArgAction::Append)]
    pub(crate) targets: Vec<TargetSpec>,
    #[arg(short = 'l', action = ArgAction::SetTrue)]
    pub(crate) literal: bool,
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) keys: Vec<String>,
}

impl BroadcastKeysArgs {
    pub(crate) fn validate(self) -> Result<Self, clap::Error> {
        if self.targets.is_empty() {
            return Err(value_error(
                "broadcast-keys",
                "at least one --target is required",
            ));
        }
        if self.keys.is_empty() {
            return Err(value_error(
                "broadcast-keys",
                "at least one key is required",
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct WithSessionArgs {
    #[arg(value_parser = parse_session_name)]
    pub(crate) session_name: rmux_proto::SessionName,
    #[arg(long = "kill-on-owner-exit", action = ArgAction::SetTrue)]
    pub(crate) kill_on_owner_exit: bool,
    #[arg(long = "ttl", value_parser = parse_duration, default_value = "30s")]
    pub(crate) ttl: Duration,
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) command: Vec<String>,
}

impl WithSessionArgs {
    pub(crate) fn validate(self) -> Result<Self, clap::Error> {
        if self.command.is_empty() {
            return Err(value_error("with-session", "a child command is required"));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendKeysWaitMode {
    Quiet,
}

pub(crate) fn parse_duration(value: &str) -> Result<Duration, String> {
    let (number, multiplier) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1_000)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60_000)
    } else {
        return Err("duration requires an explicit unit: ms, s, or m".to_owned());
    };
    if number.is_empty() || number.starts_with('-') {
        return Err("duration must be positive".to_owned());
    }
    let amount = number
        .parse::<u64>()
        .map_err(|_| "duration must be an integer".to_owned())?;
    if amount == 0 {
        return Err("duration must be positive".to_owned());
    }
    amount
        .checked_mul(multiplier)
        .map(Duration::from_millis)
        .ok_or_else(|| "duration is too large".to_owned())
}

fn parse_region(value: &str) -> Result<SnapshotRegion, String> {
    let parts = value.split(',').collect::<Vec<_>>();
    let [row, col, rows, cols] = parts.as_slice() else {
        return Err("region must use row,col,rows,cols".to_owned());
    };
    Ok(SnapshotRegion {
        row: parse_u16(row, "row")?,
        col: parse_u16(col, "col")?,
        rows: parse_positive_u16(rows, "rows")?,
        cols: parse_positive_u16(cols, "cols")?,
    })
}

fn parse_u16(value: &str, field: &str) -> Result<u16, String> {
    value
        .parse::<u16>()
        .map_err(|_| format!("{field} must be a u16"))
}

fn parse_positive_u16(value: &str, field: &str) -> Result<u16, String> {
    let parsed = parse_u16(value, field)?;
    if parsed == 0 {
        return Err(format!("{field} must be positive"));
    }
    Ok(parsed)
}

fn parse_positive_usize(value: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| "value must be a positive integer".to_owned())?;
    if parsed == 0 {
        return Err("value must be a positive integer".to_owned());
    }
    Ok(parsed)
}

fn reject_empty(command_name: &str, flag: &str, value: Option<&str>) -> Result<(), clap::Error> {
    if value.is_some_and(str::is_empty) {
        return Err(value_error(
            command_name,
            format!("{flag} must not be empty"),
        ));
    }
    Ok(())
}

fn value_error(command_name: &str, message: impl std::fmt::Display) -> clap::Error {
    clap::Error::raw(
        clap::error::ErrorKind::ValueValidation,
        format!("command {command_name}: {message}"),
    )
}
