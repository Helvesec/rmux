use rmux_proto::RmuxError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedFlaggedArgs {
    pub(super) flags: Vec<char>,
    pub(super) positionals: Vec<String>,
}

pub(super) fn strip_leading_separator(args: &[String]) -> &[String] {
    match args.first().map(String::as_str) {
        Some("--") => &args[1..],
        _ => args,
    }
}

pub(super) fn parse_flagged_args(
    args: &[String],
    allowed_flags: &str,
) -> Result<ParsedFlaggedArgs, RmuxError> {
    let mut flags = Vec::new();
    let mut positionals = Vec::new();
    let mut positional_mode = false;
    for arg in args {
        if positional_mode {
            positionals.push(arg.clone());
            continue;
        }
        if arg == "--" {
            positional_mode = true;
            continue;
        }
        if let Some(flag_cluster) = arg.strip_prefix('-')
            && !flag_cluster.is_empty() && flag_cluster.chars().all(|ch| allowed_flags.contains(ch))
            {
                flags.extend(flag_cluster.chars());
                continue;
            }
        positionals.push(arg.clone());
    }
    Ok(ParsedFlaggedArgs { flags, positionals })
}

pub(super) fn parse_positionals(args: &[String]) -> Result<Vec<String>, RmuxError> {
    Ok(parse_flagged_args(args, "")?.positionals)
}

pub(super) fn parse_single_argument(command: &str, args: &[String]) -> Result<String, RmuxError> {
    let positionals = parse_positionals(args)?;
    let first = positionals
        .first()
        .cloned()
        .ok_or_else(|| RmuxError::Server(format!("{command} expects one argument")))?;
    ensure_max_positional(command, &positionals, 1)?;
    Ok(first)
}

pub(super) fn ensure_max_positional(
    command: &str,
    args: &[String],
    max: usize,
) -> Result<(), RmuxError> {
    if args.len() > max {
        return Err(RmuxError::Server(format!(
            "{command} accepts at most {max} argument{}",
            if max == 1 { "" } else { "s" }
        )));
    }
    Ok(())
}

pub(super) fn ensure_no_extra_args(command: &str, args: &[String]) -> Result<(), RmuxError> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(RmuxError::Server(format!(
            "{command} does not accept extra arguments"
        )))
    }
}

pub(super) fn is_readonly_command(command: &str) -> bool {
    matches!(
        command,
        "back-to-indentation"
            | "bottom-line"
            | "cancel"
            | "cursor-down"
            | "cursor-down-and-cancel"
            | "cursor-left"
            | "cursor-right"
            | "cursor-up"
            | "cursor-centre-vertical"
            | "cursor-centre-horizontal"
            | "end-of-line"
            | "goto-line"
            | "halfpage-down"
            | "halfpage-down-and-cancel"
            | "halfpage-up"
            | "history-bottom"
            | "history-top"
            | "jump-to-mark"
            | "next-prompt"
            | "previous-prompt"
            | "middle-line"
            | "next-matching-bracket"
            | "next-paragraph"
            | "next-space"
            | "next-space-end"
            | "next-word"
            | "next-word-end"
            | "page-down"
            | "page-down-and-cancel"
            | "page-up"
            | "previous-matching-bracket"
            | "previous-paragraph"
            | "previous-space"
            | "previous-word"
            | "refresh-from-pane"
            | "scroll-bottom"
            | "scroll-down"
            | "scroll-down-and-cancel"
            | "scroll-middle"
            | "scroll-to-mouse"
            | "scroll-top"
            | "scroll-up"
            | "set-mark"
            | "start-of-line"
            | "toggle-position"
            | "top-line"
    )
}
