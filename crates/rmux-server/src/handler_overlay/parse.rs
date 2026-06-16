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
        match token.as_str() {
            "-b" => {
                let value = args.pop("-b border-lines")?;
                border_lines = Some(BoxLines::parse(Some(value.as_str())));
            }
            "-c" => target_client = Some(args.pop("-c target-client")?),
            "-C" => {
                let value = args.pop("-C starting-choice")?;
                starting_choice = Some(if value == "-" {
                    None
                } else {
                    Some(value.parse::<usize>().map_err(|_| {
                        RmuxError::Server(format!("invalid display-menu starting choice '{value}'"))
                    })?)
                });
            }
            "-H" => selected_style = Some(args.pop("-H style")?),
            "-M" => force_mouse = true,
            "-O" => stay_open = true,
            "-s" => style = Some(args.pop("-s style")?),
            "-S" => border_style = Some(args.pop("-S style")?),
            "-t" => {
                target_pane = Some(parse_overlay_pane_target(
                    "display-menu",
                    args.pop("-t target")?,
                )?)
            }
            "-T" => title = args.pop("-T title")?,
            "-x" => x = Some(args.pop("-x position")?),
            "-y" => y = Some(args.pop("-y position")?),
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
        let shortcut = args.pop("display-menu item shortcut")?;
        let command = args.pop("display-menu item command")?;
        items.push(ParsedMenuItem {
            label,
            shortcut,
            command,
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
        match token.as_str() {
            "-B" => border_lines = Some(BoxLines::None),
            "-b" => {
                let value = args.pop("-b border-lines")?;
                border_lines = Some(BoxLines::parse(Some(value.as_str())));
            }
            "-C" => close_existing = true,
            "-c" => target_client = Some(args.pop("-c target-client")?),
            "-d" => start_directory = Some(PathBuf::from(args.pop("-d start-directory")?)),
            "-e" => environment.push(args.pop("-e name=value")?),
            flag if flag.starts_with("-e") && flag.len() > 2 => {
                environment.push(flag[2..].to_owned());
            }
            "-E" => {
                if args.peek() == Some("-E") {
                    let _ = args.optional();
                    close_on_zero_exit = true;
                } else {
                    close_on_exit = true;
                }
            }
            "-h" => height = Some(parse_popup_size_spec(&args.pop("-h height")?)?),
            "-k" => {
                close_any_key = true;
                no_job = true;
            }
            "-N" => no_job = true,
            "-s" => style = Some(args.pop("-s style")?),
            "-S" => border_style = Some(args.pop("-S style")?),
            "-t" => {
                target_pane = Some(parse_overlay_pane_target(
                    "display-popup",
                    args.pop("-t target")?,
                )?)
            }
            "-T" => title = args.pop("-T title")?,
            "-w" => width = Some(parse_popup_size_spec(&args.pop("-w width")?)?),
            "-x" => x = Some(args.pop("-x position")?),
            "-y" => y = Some(args.pop("-y position")?),
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
    rmux_core::key_string_lookup_string(value)
        .map(decode_prompt_key)
        .or_else(|| {
            let mut chars = value.chars();
            match (chars.next(), chars.next(), chars.next()) {
                (Some(ch), None, None) => Some(PromptInputEvent::Char(ch)),
                _ => None,
            }
        })
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
    use super::parse_display_menu;
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
}
