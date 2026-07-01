use std::collections::VecDeque;
use std::path::PathBuf;
use std::str::FromStr;

use rmux_core::BoxLines;
use rmux_proto::{PaneTarget, RmuxError, Target};

use super::super::prompt_support::{decode_prompt_key, PromptInputEvent};

#[derive(Debug, Clone)]
pub(in super::super) enum ParsedOverlayCommand {
    Menu(ParsedDisplayMenuCommand),
    Popup(ParsedDisplayPopupCommand),
}

#[derive(Debug, Clone)]
pub(in super::super) struct ParsedDisplayMenuCommand {
    pub(super) target_client: Option<String>,
    pub(super) target_pane: Option<PaneTarget>,
    pub(super) title: String,
    pub(super) x: Option<String>,
    pub(super) y: Option<String>,
    pub(super) style: Option<String>,
    pub(super) selected_style: Option<String>,
    pub(super) border_style: Option<String>,
    pub(super) border_lines: Option<BoxLines>,
    pub(super) force_mouse: bool,
    pub(super) stay_open: bool,
    pub(super) starting_choice: Option<Option<usize>>,
    pub(super) items: Vec<ParsedMenuItem>,
}

#[derive(Debug, Clone)]
pub(super) struct ParsedMenuItem {
    pub(super) label: String,
    pub(super) shortcut: String,
    pub(super) command: String,
    pub(super) separator: bool,
}

#[derive(Debug, Clone)]
pub(in super::super) struct ParsedDisplayPopupCommand {
    pub(super) target_client: Option<String>,
    pub(super) target_pane: Option<PaneTarget>,
    pub(super) title: String,
    pub(super) x: Option<String>,
    pub(super) y: Option<String>,
    pub(super) width: Option<PopupSizeSpec>,
    pub(super) height: Option<PopupSizeSpec>,
    pub(super) style: Option<String>,
    pub(super) border_style: Option<String>,
    pub(super) border_lines: Option<BoxLines>,
    pub(super) close_existing: bool,
    pub(super) close_on_exit: bool,
    pub(super) close_on_zero_exit: bool,
    pub(super) close_any_key: bool,
    pub(super) no_job: bool,
    pub(super) start_directory: Option<PathBuf>,
    pub(super) environment: Vec<String>,
    pub(super) command: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PopupSizeSpec {
    Absolute(u16),
    Percent(u8),
}

#[derive(Debug)]
struct OverlayCommandTokens {
    tokens: VecDeque<String>,
}

impl OverlayCommandTokens {
    fn new(tokens: Vec<String>) -> Self {
        Self {
            tokens: tokens.into_iter().collect(),
        }
    }

    fn peek(&self) -> Option<&str> {
        self.tokens.front().map(String::as_str)
    }

    fn pop(&mut self, description: &str) -> Result<String, RmuxError> {
        self.tokens
            .pop_front()
            .ok_or_else(|| RmuxError::Server(format!("missing {description}")))
    }

    fn optional(&mut self) -> Option<String> {
        self.tokens.pop_front()
    }

    fn remaining(self) -> Vec<String> {
        self.tokens.into_iter().collect()
    }

    fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

fn split_attached_short_flag(token: String) -> (String, Option<String>) {
    let mut chars = token.chars();
    if chars.next() != Some('-') || chars.as_str().starts_with('-') {
        return (token, None);
    }
    let Some(flag) = chars.next() else {
        return (token, None);
    };
    let attached = chars.as_str();
    (
        format!("-{flag}"),
        (!attached.is_empty()).then(|| attached.to_owned()),
    )
}

fn pop_flag_value(
    args: &mut OverlayCommandTokens,
    attached: Option<String>,
    description: &str,
) -> Result<String, RmuxError> {
    match attached {
        Some(value) => Ok(value),
        None => args.pop(description),
    }
}

pub(super) fn parse_display_menu(
    arguments: Vec<String>,
) -> Result<ParsedDisplayMenuCommand, RmuxError> {
    let mut args = OverlayCommandTokens::new(arguments);
    let mut target_client = None;
    let mut target_pane = None;
    let mut title = String::new();
    let mut x = None;
    let mut y = None;
    let mut style = None;
    let mut selected_style = None;
    let mut border_style = None;
    let mut border_lines = None;
    let mut force_mouse = false;
    let mut stay_open = false;
    let mut starting_choice = None;

    while let Some(token) = args.peek() {
        if token == "--" {
            let _ = args.optional();
            break;
        }
        if !token.starts_with('-') || token == "-" {
            break;
        }
        let token = args.pop("display-menu flag")?;
        let (flag, attached) = split_attached_short_flag(token);
        match flag.as_str() {
            "-b" => {
                let value = pop_flag_value(&mut args, attached, "-b border-lines")?;
                border_lines = Some(BoxLines::parse(Some(value.as_str())));
            }
            "-c" => target_client = Some(pop_flag_value(&mut args, attached, "-c target-client")?),
            "-C" => {
                let value = pop_flag_value(&mut args, attached, "-C starting-choice")?;
                starting_choice = Some(if value == "-" {
                    None
                } else {
                    Some(value.parse::<usize>().map_err(|_| {
                        RmuxError::Server(format!("invalid display-menu starting choice '{value}'"))
                    })?)
                });
            }
            "-H" => selected_style = Some(pop_flag_value(&mut args, attached, "-H style")?),
            "-M" if attached.is_none() => force_mouse = true,
            "-O" if attached.is_none() => stay_open = true,
            "-s" => style = Some(pop_flag_value(&mut args, attached, "-s style")?),
            "-S" => border_style = Some(pop_flag_value(&mut args, attached, "-S style")?),
            "-t" => {
                target_pane = Some(parse_overlay_pane_target(
                    "display-menu",
                    pop_flag_value(&mut args, attached, "-t target")?,
                )?)
            }
            "-T" => title = pop_flag_value(&mut args, attached, "-T title")?,
            "-x" => x = Some(pop_flag_value(&mut args, attached, "-x position")?),
            "-y" => y = Some(pop_flag_value(&mut args, attached, "-y position")?),
            flag => {
                return Err(RmuxError::Server(format!(
                    "unsupported flag '{flag}' for display-menu"
                )));
            }
        }
    }

    let mut items = Vec::new();
    while !args.is_empty() {
        let label = args.pop("display-menu item label")?;
        if label.is_empty() || label == "-" {
            items.push(ParsedMenuItem {
                label,
                shortcut: String::new(),
                command: String::new(),
                separator: true,
            });
            continue;
        }
        let shortcut = args.pop("display-menu item shortcut")?;
        let command = args.pop("display-menu item command")?;
        items.push(ParsedMenuItem {
            label,
            shortcut,
            command,
            separator: false,
        });
    }
    if items.is_empty() {
        return Err(RmuxError::Message(
            "command display-menu: too few arguments (need at least 1)".to_owned(),
        ));
    }

    Ok(ParsedDisplayMenuCommand {
        target_client,
        target_pane,
        title,
        x,
        y,
        style,
        selected_style,
        border_style,
        border_lines,
        force_mouse,
        stay_open,
        starting_choice,
        items,
    })
}

pub(super) fn parse_display_popup(
    arguments: Vec<String>,
) -> Result<ParsedDisplayPopupCommand, RmuxError> {
    let mut args = OverlayCommandTokens::new(arguments);
    let mut target_client = None;
    let mut target_pane = None;
    let mut title = String::new();
    let mut x = None;
    let mut y = None;
    let mut width = None;
    let mut height = None;
    let mut style = None;
    let mut border_style = None;
    let mut border_lines = None;
    let mut close_existing = false;
    let mut close_on_exit = false;
    let mut close_on_zero_exit = false;
    let mut close_any_key = false;
    let mut no_job = false;
    let mut start_directory = None;
    let mut environment = Vec::new();

    while let Some(token) = args.peek() {
        if token == "--" {
            let _ = args.optional();
            break;
        }
        if !token.starts_with('-') || token == "-" {
            break;
        }
        let token = args.pop("display-popup flag")?;
        if token.starts_with("-EE") || token == "-EE" {
            close_on_zero_exit = true;
            continue;
        }
        let (flag, attached) = split_attached_short_flag(token);
        match flag.as_str() {
            "-B" if attached.is_none() => border_lines = Some(BoxLines::None),
            "-b" => {
                let value = pop_flag_value(&mut args, attached, "-b border-lines")?;
                border_lines = Some(BoxLines::parse(Some(value.as_str())));
            }
            "-C" if attached.is_none() => close_existing = true,
            "-c" => target_client = Some(pop_flag_value(&mut args, attached, "-c target-client")?),
            "-d" => {
                start_directory = Some(PathBuf::from(pop_flag_value(
                    &mut args,
                    attached,
                    "-d start-directory",
                )?));
            }
            "-e" => environment.push(pop_flag_value(&mut args, attached, "-e name=value")?),
            "-E" if attached.is_none() => {
                if args.peek() == Some("-E") {
                    let _ = args.optional();
                    close_on_zero_exit = true;
                } else {
                    close_on_exit = true;
                }
            }
            "-h" => {
                height = Some(parse_popup_size_spec(&pop_flag_value(
                    &mut args,
                    attached,
                    "-h height",
                )?)?)
            }
            "-k" if attached.is_none() => {
                close_any_key = true;
                no_job = true;
            }
            "-N" if attached.is_none() => no_job = true,
            "-s" => style = Some(pop_flag_value(&mut args, attached, "-s style")?),
            "-S" => border_style = Some(pop_flag_value(&mut args, attached, "-S style")?),
            "-t" => {
                target_pane = Some(parse_overlay_pane_target(
                    "display-popup",
                    pop_flag_value(&mut args, attached, "-t target")?,
                )?)
            }
            "-T" => title = pop_flag_value(&mut args, attached, "-T title")?,
            "-w" => {
                width = Some(parse_popup_size_spec(&pop_flag_value(
                    &mut args, attached, "-w width",
                )?)?)
            }
            "-x" => x = Some(pop_flag_value(&mut args, attached, "-x position")?),
            "-y" => y = Some(pop_flag_value(&mut args, attached, "-y position")?),
            flag => {
                return Err(RmuxError::Server(format!(
                    "unsupported flag '{flag}' for display-popup"
                )));
            }
        }
    }

    let command = {
        let remaining = args.remaining();
        if remaining.is_empty() {
            None
        } else {
            Some(rebuild_shell_command(remaining))
        }
    };

    Ok(ParsedDisplayPopupCommand {
        target_client,
        target_pane,
        title,
        x,
        y,
        width,
        height,
        style,
        border_style,
        border_lines,
        close_existing,
        close_on_exit,
        close_on_zero_exit,
        close_any_key,
        no_job,
        start_directory,
        environment,
        command,
    })
}

pub(super) fn parse_menu_shortcut(value: &str) -> Option<PromptInputEvent> {
    if value.is_empty() {
        return None;
    }
    let mut chars = value.chars();
    if let (Some(ch), None) = (chars.next(), chars.next()) {
        return Some(PromptInputEvent::Char(ch));
    }
    rmux_core::key_string_lookup_string(value).map(decode_prompt_key)
}

pub(super) fn parse_popup_size_spec(value: &str) -> Result<PopupSizeSpec, RmuxError> {
    if let Some(percent) = value.strip_suffix('%') {
        let percent = percent
            .parse::<u8>()
            .map_err(|_| RmuxError::Server(format!("invalid popup percentage '{value}'")))?;
        return Ok(PopupSizeSpec::Percent(percent.clamp(1, 100)));
    }
    let absolute = value
        .parse::<u16>()
        .map_err(|_| RmuxError::Server(format!("invalid popup size '{value}'")))?;
    Ok(PopupSizeSpec::Absolute(absolute.max(1)))
}

fn parse_overlay_pane_target(command: &str, value: String) -> Result<PaneTarget, RmuxError> {
    match Target::from_str(&value) {
        Ok(Target::Pane(target)) => Ok(target),
        Ok(_) => Err(RmuxError::Server(format!(
            "{command} target must match 'session:window.pane'"
        ))),
        Err(error) => Err(RmuxError::Server(format!(
            "invalid {command} target '{value}': {error}"
        ))),
    }
}

fn rebuild_shell_command(command_parts: Vec<String>) -> String {
    if command_parts.len() == 1 {
        return command_parts
            .into_iter()
            .next()
            .expect("single popup shell token");
    }
    command_parts
        .into_iter()
        .map(|token| format!("'{}'", token.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::{parse_display_menu, parse_menu_shortcut};
    use crate::handler::prompt_support::PromptInputEvent;
    use rmux_proto::RmuxError;

    #[test]
    fn display_menu_requires_at_least_one_item_argument() {
        let error = parse_display_menu(vec!["-T".to_owned(), "Menu".to_owned()])
            .expect_err("empty display-menu should be rejected before client lookup");
        assert_eq!(
            error,
            RmuxError::Message(
                "command display-menu: too few arguments (need at least 1)".to_owned()
            )
        );
    }

    #[test]
    fn display_menu_single_empty_label_parses_as_separator() {
        let parsed = parse_display_menu(vec![
            "-T".to_owned(),
            "Menu".to_owned(),
            "One".to_owned(),
            "o".to_owned(),
            "display-message one".to_owned(),
            String::new(),
            "Two".to_owned(),
            "t".to_owned(),
            "display-message two".to_owned(),
        ])
        .expect("display-menu with a single-token separator should parse");

        assert_eq!(parsed.items.len(), 3);
        assert_eq!(parsed.items[1].label, "");
        assert_eq!(parsed.items[1].shortcut, "");
        assert_eq!(parsed.items[1].command, "");
        assert!(parsed.items[1].separator);
        assert_eq!(parsed.items[2].label, "Two");
    }

    #[test]
    fn display_menu_single_dash_label_parses_as_separator() {
        let parsed = parse_display_menu(vec![
            "-T".to_owned(),
            "Menu".to_owned(),
            "One".to_owned(),
            "o".to_owned(),
            "display-message one".to_owned(),
            "-".to_owned(),
            "Two".to_owned(),
            "t".to_owned(),
            "display-message two".to_owned(),
        ])
        .expect("display-menu with a dash separator should parse");

        assert_eq!(parsed.items.len(), 3);
        assert_eq!(parsed.items[1].label, "-");
        assert!(parsed.items[1].separator);
        assert_eq!(parsed.items[2].label, "Two");
    }

    #[test]
    fn display_menu_hyphen_prefixed_label_with_command_is_actionable() {
        let parsed = parse_display_menu(vec![
            "-T".to_owned(),
            "Menu".to_owned(),
            "--".to_owned(),
            "-Danger".to_owned(),
            "d".to_owned(),
            "display-message danger".to_owned(),
        ])
        .expect("hyphen-prefixed menu label with command should parse");

        assert_eq!(parsed.items.len(), 1);
        assert_eq!(parsed.items[0].label, "-Danger");
        assert_eq!(parsed.items[0].shortcut, "d");
        assert_eq!(parsed.items[0].command, "display-message danger");
        assert!(!parsed.items[0].separator);
    }

    #[test]
    fn menu_shortcut_prefers_literal_single_character_events() {
        assert_eq!(parse_menu_shortcut("n"), Some(PromptInputEvent::Char('n')));
        assert_eq!(parse_menu_shortcut("X"), Some(PromptInputEvent::Char('X')));
        assert_eq!(parse_menu_shortcut("<"), Some(PromptInputEvent::Char('<')));
        assert_eq!(
            parse_menu_shortcut("C-r"),
            Some(PromptInputEvent::KeyName("C-r".to_owned()))
        );
    }
}
