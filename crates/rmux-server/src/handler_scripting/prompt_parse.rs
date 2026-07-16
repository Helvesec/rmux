use std::collections::VecDeque;

use rmux_core::command_parser::{CommandArgument, ParsedCommand};
use rmux_proto::RmuxError;

use super::super::prompt_support::{
    PromptType, PROMPT_FLAG_BSPACE_EXIT, PROMPT_FLAG_INCREMENTAL, PROMPT_FLAG_KEY,
    PROMPT_FLAG_NUMERIC, PROMPT_FLAG_SINGLE,
};
use super::command_args::{
    command_argument_for_error, pop_command_list_argument, pop_string_argument, CommandListArgument,
};
use super::tokens::CommandTokens;
use super::values::unsupported_flag;

#[derive(Debug, Clone, Copy)]
pub(in crate::handler) enum PromptHistoryAction {
    Show,
    Clear,
}

#[derive(Debug, Clone)]
pub(in crate::handler) struct ParsedPromptHistoryCommand {
    pub(in crate::handler) action: PromptHistoryAction,
    pub(in crate::handler) prompt_type: Option<PromptType>,
}

#[derive(Debug, Clone)]
pub(super) struct ParsedCommandPromptCommand {
    pub(super) background: bool,
    pub(super) format_template: bool,
    pub(super) literal: bool,
    pub(super) prompts: Option<String>,
    pub(super) inputs: Option<String>,
    pub(super) prompt_type: PromptType,
    pub(super) target_client: Option<String>,
    pub(super) flags: u8,
    pub(super) template: Option<CommandListArgument>,
}

#[derive(Debug, Clone)]
pub(super) struct ParsedConfirmBeforeCommand {
    pub(super) background: bool,
    pub(super) prompt: Option<String>,
    pub(super) confirm_key: char,
    pub(super) default_yes: bool,
    pub(super) target_client: Option<String>,
    pub(super) command: CommandListArgument,
}

pub(super) fn parse_queued_command_prompt(
    command: ParsedCommand,
) -> Result<ParsedCommandPromptCommand, RmuxError> {
    let mut args = VecDeque::from(command.arguments().to_vec());
    let mut background = false;
    let mut format_template = false;
    let mut literal = false;
    let mut single = false;
    let mut numeric = false;
    let mut incremental = false;
    let mut key = false;
    let mut backspace_exit = false;
    let mut prompts = None;
    let mut inputs = None;
    let mut prompt_type = PromptType::Command;
    let mut target_client = None;

    while let Some(token) = args.front().and_then(CommandArgument::as_string) {
        if token == "--" {
            let _ = args.pop_front();
            break;
        }
        if token == "-" || !token.starts_with('-') {
            break;
        }

        let flag_token = pop_string_argument(&mut args, "command-prompt flag")?;
        let chars = flag_token.chars().collect::<Vec<_>>();
        let mut index = 1;
        while index < chars.len() {
            match chars[index] {
                '1' => single = true,
                'b' => background = true,
                'C' => {}
                'e' => backspace_exit = true,
                'F' => format_template = true,
                'i' => incremental = true,
                'k' => key = true,
                'l' => literal = true,
                'N' => numeric = true,
                'I' => {
                    inputs = Some(inline_flag_value(
                        &chars,
                        &mut index,
                        &mut args,
                        "command-prompt -I inputs",
                    )?);
                    break;
                }
                'p' => {
                    prompts = Some(inline_flag_value(
                        &chars,
                        &mut index,
                        &mut args,
                        "command-prompt -p prompts",
                    )?);
                    break;
                }
                'T' => {
                    let value = inline_flag_value(
                        &chars,
                        &mut index,
                        &mut args,
                        "command-prompt -T prompt-type",
                    )?;
                    prompt_type = PromptType::parse(&value)
                        .ok_or_else(|| RmuxError::Server(format!("unknown type: {value}")))?;
                    break;
                }
                't' => {
                    target_client = Some(inline_flag_value(
                        &chars,
                        &mut index,
                        &mut args,
                        "command-prompt -t target-client",
                    )?);
                    break;
                }
                other => return Err(unsupported_flag("command-prompt", &format!("-{other}"))),
            }
            index += 1;
        }
    }

    let template = if args.is_empty() {
        None
    } else {
        Some(pop_command_list_argument(
            &mut args,
            "command-prompt template",
        )?)
    };
    if let Some(extra) = args.front() {
        return Err(RmuxError::Server(format!(
            "unexpected argument '{}' for command-prompt",
            command_argument_for_error(extra)
        )));
    }

    let flags = if single {
        PROMPT_FLAG_SINGLE
    } else if numeric {
        PROMPT_FLAG_NUMERIC
    } else if incremental {
        PROMPT_FLAG_INCREMENTAL
    } else if key {
        PROMPT_FLAG_KEY
    } else if backspace_exit {
        PROMPT_FLAG_BSPACE_EXIT
    } else {
        0
    };

    Ok(ParsedCommandPromptCommand {
        background: background || incremental,
        format_template,
        literal,
        prompts,
        inputs,
        prompt_type,
        target_client,
        flags,
        template,
    })
}

pub(super) fn parse_queued_confirm_before(
    command: ParsedCommand,
) -> Result<ParsedConfirmBeforeCommand, RmuxError> {
    let mut args = VecDeque::from(command.arguments().to_vec());
    let mut background = false;
    let mut prompt = None;
    let mut confirm_key = 'y';
    let mut default_yes = false;
    let mut target_client = None;

    while let Some(token) = args.front().and_then(CommandArgument::as_string) {
        if token == "--" {
            let _ = args.pop_front();
            break;
        }
        if token == "-" || !token.starts_with('-') {
            break;
        }

        let flag_token = pop_string_argument(&mut args, "confirm-before flag")?;
        let chars = flag_token.chars().collect::<Vec<_>>();
        let mut index = 1;
        while index < chars.len() {
            match chars[index] {
                'b' => background = true,
                'y' => default_yes = true,
                'c' => {
                    let value = inline_flag_value(
                        &chars,
                        &mut index,
                        &mut args,
                        "confirm-before -c confirm-key",
                    )?;
                    confirm_key = parse_confirm_key(&value)?;
                    break;
                }
                'p' => {
                    prompt = Some(inline_flag_value(
                        &chars,
                        &mut index,
                        &mut args,
                        "confirm-before -p prompt",
                    )?);
                    break;
                }
                't' => {
                    target_client = Some(inline_flag_value(
                        &chars,
                        &mut index,
                        &mut args,
                        "confirm-before -t target-client",
                    )?);
                    break;
                }
                other => return Err(unsupported_flag("confirm-before", &format!("-{other}"))),
            }
            index += 1;
        }
    }

    let command = pop_command_list_argument(&mut args, "confirm-before command")?;
    if let Some(extra) = args.front() {
        return Err(RmuxError::Server(format!(
            "unexpected argument '{}' for confirm-before",
            command_argument_for_error(extra)
        )));
    }

    Ok(ParsedConfirmBeforeCommand {
        background,
        prompt,
        confirm_key,
        default_yes,
        target_client,
        command,
    })
}

/// Recognises `clear-prompt-history` / `show-prompt-history` and parses their
/// shared `-T <prompt-type>` option. Returns `Ok(None)` for any other command
/// so callers can fall through to the next recogniser (mirroring the
/// `parse_overlay_queue_command` pattern).
pub(super) fn parse_prompt_history_queue_command(
    command_name: &str,
    arguments: Vec<String>,
) -> Result<Option<ParsedPromptHistoryCommand>, RmuxError> {
    let action = match command_name {
        "clear-prompt-history" => PromptHistoryAction::Clear,
        "show-prompt-history" => PromptHistoryAction::Show,
        _ => return Ok(None),
    };

    let mut tokens = CommandTokens::new(arguments);
    let mut prompt_type: Option<PromptType> = None;
    while let Some(token) = tokens.peek() {
        match token {
            "--" => {
                let _ = tokens.optional();
                break;
            }
            "-T" => {
                let _ = tokens.optional();
                let value = tokens.required("-T prompt-type")?;
                prompt_type = Some(
                    PromptType::parse(&value)
                        .ok_or_else(|| RmuxError::Server(format!("invalid type: {value}")))?,
                );
            }
            flag if flag.starts_with('-') => return Err(unsupported_flag(command_name, flag)),
            _ => break,
        }
    }
    tokens.no_extra(command_name)?;
    Ok(Some(ParsedPromptHistoryCommand {
        action,
        prompt_type,
    }))
}

fn inline_flag_value(
    chars: &[char],
    index: &mut usize,
    args: &mut VecDeque<CommandArgument>,
    description: &str,
) -> Result<String, RmuxError> {
    if *index + 1 < chars.len() {
        let value = chars[*index + 1..].iter().collect();
        *index = chars.len();
        return Ok(value);
    }

    pop_string_argument(args, description)
}

fn parse_confirm_key(value: &str) -> Result<char, RmuxError> {
    let mut chars = value.chars();
    let ch = chars
        .next()
        .ok_or_else(|| RmuxError::Server("invalid confirm key".to_owned()))?;
    if chars.next().is_some() || !matches!(ch as u32, 32..=126) {
        return Err(RmuxError::Server("invalid confirm key".to_owned()));
    }
    Ok(ch)
}
