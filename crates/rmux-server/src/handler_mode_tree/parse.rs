use std::collections::VecDeque;

use rmux_core::command_parser::{CommandArgument, ParsedCommand};
use rmux_proto::RmuxError;

use super::mode_tree_model::{
    ModeTreeKind, ParsedModeTreeCommand, PreviewMode, SortOrder, TreeDepth,
};

pub(super) fn parse_mode_tree_queue_command(
    command: ParsedCommand,
) -> Result<Option<ParsedModeTreeCommand>, RmuxError> {
    if command.name() == "find-window" {
        return parse_find_window_as_tree(command).map(Some);
    }

    let kind = match command.name() {
        "choose-tree" => ModeTreeKind::Tree,
        "choose-buffer" => ModeTreeKind::Buffer,
        "choose-client" => ModeTreeKind::Client,
        "customize-mode" => ModeTreeKind::Customize,
        _ => return Ok(None),
    };

    let mut args = VecDeque::from(command.arguments().to_vec());
    let mut preview_state = PreviewMode::Normal;
    let mut row_format = None;
    let mut filter_format = None;
    let mut key_format = None;
    let mut sort_order = None;
    let mut reversed = false;
    let mut tree_depth = TreeDepth::Pane;
    let mut show_all_group_members = false;
    let auto_accept = false;
    let mut zoom = false;
    let mut target = None;

    while let Some(token) = args.front().and_then(CommandArgument::as_string) {
        if token == "--" {
            let _ = args.pop_front();
            break;
        }
        if token == "-" || !token.starts_with('-') {
            break;
        }

        let flag_token = pop_string_argument(&mut args, "mode-tree flag")?;
        let chars = flag_token.chars().collect::<Vec<_>>();
        let mut index = 1;
        while index < chars.len() {
            match chars[index] {
                'F' => {
                    row_format = Some(inline_flag_value(
                        &chars, &mut index, &mut args, "-F value",
                    )?);
                    break;
                }
                'f' => {
                    filter_format = Some(inline_flag_value(
                        &chars, &mut index, &mut args, "-f value",
                    )?);
                    break;
                }
                'G' => show_all_group_members = true,
                'K' => {
                    key_format = Some(inline_flag_value(
                        &chars, &mut index, &mut args, "-K value",
                    )?);
                    break;
                }
                'N' => {
                    preview_state = match preview_state {
                        PreviewMode::Normal => PreviewMode::Off,
                        PreviewMode::Off => PreviewMode::Big,
                        PreviewMode::Big => PreviewMode::Big,
                    };
                }
                'O' => {
                    let value = inline_flag_value(&chars, &mut index, &mut args, "-O value")?;
                    sort_order = Some(parse_sort_order(kind, &value)?);
                    break;
                }
                'r' => reversed = true,
                's' => tree_depth = TreeDepth::Session,
                'w' => tree_depth = TreeDepth::Window,
                'y' => {
                    return Err(RmuxError::Server(format!(
                        "unsupported flag '-y' for {}",
                        command.name()
                    )));
                }
                'Z' => zoom = true,
                't' => {
                    target = Some(inline_flag_value(
                        &chars,
                        &mut index,
                        &mut args,
                        "-t target",
                    )?);
                    break;
                }
                other => {
                    return Err(RmuxError::Server(format!(
                        "unsupported flag '-{other}' for {}",
                        command.name()
                    )));
                }
            }
            index += 1;
        }
    }

    let template = if kind == ModeTreeKind::Customize || args.is_empty() {
        None
    } else {
        Some(join_command_arguments(&mut args, "mode-tree template")?)
    };

    if let Some(extra) = args.front() {
        return Err(RmuxError::Server(format!(
            "unexpected argument '{}' for {}",
            command_argument_for_error(extra),
            command.name()
        )));
    }

    Ok(Some(ParsedModeTreeCommand {
        kind,
        target,
        preview_mode: preview_state,
        row_format,
        filter_format,
        key_format,
        template,
        sort_order,
        reversed,
        tree_depth,
        show_all_group_members,
        auto_accept,
        zoom,
    }))
}

/// Transforms `find-window [-CiNrTZ] [-t target] match-string` into a
/// `choose-tree` mode tree command with the appropriate filter format,
/// matching tmux `cmd-find-window.c` semantics.
fn parse_find_window_as_tree(command: ParsedCommand) -> Result<ParsedModeTreeCommand, RmuxError> {
    let mut args = VecDeque::from(command.arguments().to_vec());
    let mut search_content = false;
    let mut search_name = false;
    let mut search_title = false;
    let mut case_insensitive = false;
    let mut regex = false;
    let mut zoom = false;
    let mut target = None;

    while let Some(token) = args.front().and_then(CommandArgument::as_string) {
        if token == "--" {
            let _ = args.pop_front();
            break;
        }
        if token == "-" || !token.starts_with('-') {
            break;
        }

        let flag_token = pop_string_argument(&mut args, "find-window flag")?;
        let flag_iter = flag_token
            .strip_prefix('-')
            .unwrap_or_default()
            .char_indices();
        for (offset, ch) in flag_iter {
            match ch {
                'C' => search_content = true,
                'i' => case_insensitive = true,
                'N' => search_name = true,
                'r' => regex = true,
                'T' => search_title = true,
                'Z' => zoom = true,
                't' => {
                    let attached_start = 1 + offset + ch.len_utf8();
                    target = Some(if attached_start < flag_token.len() {
                        flag_token[attached_start..].to_owned()
                    } else {
                        pop_string_argument(&mut args, "-t target")?
                    });
                    break;
                }
                other => {
                    return Err(RmuxError::Server(format!(
                        "unsupported flag '-{other}' for find-window"
                    )));
                }
            }
        }
    }

    let match_string = pop_string_argument(&mut args, "find-window match-string")?;

    if let Some(extra) = args.front() {
        return Err(RmuxError::Server(format!(
            "unexpected argument '{}' for find-window",
            command_argument_for_error(extra)
        )));
    }

    if !search_content && !search_name && !search_title {
        search_content = true;
        search_name = true;
        search_title = true;
    }

    let suffix = match (regex, case_insensitive) {
        (true, true) => "/ri",
        (true, false) => "/r",
        (false, true) => "/i",
        (false, false) => "",
    };
    let star = if regex { "" } else { "*" };

    let filter = match (search_content, search_name, search_title) {
        (true, true, true) => format!(
            "#{{||:#{{C{suffix}:{match_string}}},#{{||:#{{m{suffix}:{star}{match_string}{star},#{{window_name}}}},#{{m{suffix}:{star}{match_string}{star},#{{pane_title}}}}}}}}"
        ),
        (true, true, false) => format!(
            "#{{||:#{{C{suffix}:{match_string}}},#{{m{suffix}:{star}{match_string}{star},#{{window_name}}}}}}"
        ),
        (true, false, true) => format!(
            "#{{||:#{{C{suffix}:{match_string}}},#{{m{suffix}:{star}{match_string}{star},#{{pane_title}}}}}}"
        ),
        (false, true, true) => format!(
            "#{{||:#{{m{suffix}:{star}{match_string}{star},#{{window_name}}}},#{{m{suffix}:{star}{match_string}{star},#{{pane_title}}}}}}"
        ),
        (true, false, false) => format!("#{{C{suffix}:{match_string}}}"),
        (false, true, false) => {
            format!("#{{m{suffix}:{star}{match_string}{star},#{{window_name}}}}")
        }
        (false, false, true) => {
            format!("#{{m{suffix}:{star}{match_string}{star},#{{pane_title}}}}")
        }
        (false, false, false) => unreachable!("defaults applied above"),
    };

    Ok(ParsedModeTreeCommand {
        kind: ModeTreeKind::Tree,
        target,
        preview_mode: PreviewMode::Normal,
        row_format: None,
        filter_format: Some(filter),
        key_format: None,
        template: None,
        sort_order: None,
        reversed: false,
        tree_depth: TreeDepth::Pane,
        show_all_group_members: false,
        auto_accept: false,
        zoom,
    })
}

pub(super) fn default_order_seq(kind: ModeTreeKind) -> Vec<SortOrder> {
    match kind {
        ModeTreeKind::Tree => vec![SortOrder::Index, SortOrder::Name, SortOrder::Activity],
        ModeTreeKind::Buffer => vec![SortOrder::Creation, SortOrder::Name, SortOrder::Size],
        ModeTreeKind::Client => vec![
            SortOrder::Name,
            SortOrder::Size,
            SortOrder::Creation,
            SortOrder::Activity,
        ],
        ModeTreeKind::Customize => Vec::new(),
    }
}

fn parse_sort_order(kind: ModeTreeKind, value: &str) -> Result<SortOrder, RmuxError> {
    let lowered = value.to_ascii_lowercase();
    let order = match lowered.as_str() {
        "index" | "order" => SortOrder::Index,
        "name" | "title" => SortOrder::Name,
        "activity" => SortOrder::Activity,
        "creation" => SortOrder::Creation,
        "size" => SortOrder::Size,
        _ => return Err(RmuxError::Server(format!("invalid sort order: {value}"))),
    };
    if default_order_seq(kind).contains(&order) {
        Ok(order)
    } else {
        Err(RmuxError::Server(format!("invalid sort order: {value}")))
    }
}

fn pop_string_argument(
    args: &mut VecDeque<CommandArgument>,
    what: &str,
) -> Result<String, RmuxError> {
    let argument = args
        .pop_front()
        .ok_or_else(|| RmuxError::Server(format!("missing {what}")))?;
    argument
        .as_string()
        .map(str::to_owned)
        .ok_or_else(|| RmuxError::Server(format!("invalid {what}")))
}

fn inline_flag_value(
    chars: &[char],
    index: &mut usize,
    args: &mut VecDeque<CommandArgument>,
    what: &str,
) -> Result<String, RmuxError> {
    if *index + 1 < chars.len() {
        Ok(chars[*index + 1..].iter().collect())
    } else {
        pop_string_argument(args, what)
    }
}

fn join_command_arguments(
    args: &mut VecDeque<CommandArgument>,
    what: &str,
) -> Result<String, RmuxError> {
    if args.is_empty() {
        return Err(RmuxError::Server(format!("missing {what}")));
    }
    Ok(args
        .drain(..)
        .map(|argument| argument.to_tmux_string())
        .collect::<Vec<_>>()
        .join(" "))
}

fn command_argument_for_error(argument: &CommandArgument) -> String {
    argument.to_tmux_string()
}
