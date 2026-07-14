use std::ffi::OsString;

use rmux_core::command_parser::CommandArgument;

pub(super) fn render_control_command_lines(
    arguments: &[OsString],
) -> Result<Vec<String>, clap::Error> {
    let mut lines = Vec::new();
    let mut current = Vec::new();

    for argument in arguments {
        let value = argument.to_str().ok_or_else(invalid_utf8)?;
        let (value, ends_command) = split_command_terminator(value);
        if !ends_command || !value.is_empty() {
            current.push(value);
        }
        if ends_command && !current.is_empty() {
            lines.push(render_command(&current));
            current.clear();
        }
    }

    if !current.is_empty() {
        lines.push(render_command(&current));
    }
    Ok(lines)
}

fn split_command_terminator(value: &str) -> (String, bool) {
    let mut value = value.to_owned();
    if !value.ends_with(';') {
        return (value, false);
    }
    value.pop();
    if value.ends_with('\\') {
        value.pop();
        value.push(';');
        return (value, false);
    }
    (value, true)
}

fn render_command(arguments: &[String]) -> String {
    arguments
        .iter()
        .map(|argument| CommandArgument::String(argument.clone()).to_tmux_reparse_string())
        .collect::<Vec<_>>()
        .join(" ")
}

fn invalid_utf8() -> clap::Error {
    clap::Error::raw(
        clap::error::ErrorKind::InvalidUtf8,
        "invalid UTF-8 in command argument",
    )
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use rmux_core::command_parser::CommandParser;

    use super::render_control_command_lines;

    #[test]
    fn control_lines_preserve_alias_names_groups_and_literal_arguments() {
        let arguments = [
            "first-alias",
            "space ; dollar $HOME slash\\ quote' double\"",
            ";",
            "second-alias",
            "semi\\;",
        ]
        .map(OsString::from);

        let lines = render_control_command_lines(&arguments).expect("control lines");
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("first-alias "));
        assert!(lines[1].starts_with("second-alias "));
        let first = CommandParser::new()
            .with_command_aliases([
                "first-alias=display-message".to_owned(),
                "second-alias=display-message".to_owned(),
            ])
            .parse_one_group(&lines[0])
            .expect("first control line reparses");
        assert_eq!(first.commands()[0].name(), "display-message");
        assert_eq!(
            first.commands()[0].arguments()[0].as_string(),
            Some("space ; dollar $HOME slash\\ quote' double\"")
        );
        let second = CommandParser::new()
            .with_command_aliases([
                "first-alias=display-message".to_owned(),
                "second-alias=display-message".to_owned(),
            ])
            .parse_one_group(&lines[1])
            .expect("second control line reparses");
        assert_eq!(second.commands()[0].name(), "display-message");
        assert_eq!(
            second.commands()[0].arguments()[0].as_string(),
            Some("semi;")
        );
    }

    #[test]
    fn control_lines_keep_cr_lf_tab_inside_one_literal_argument() {
        let arguments = ["display-message", "a\rb\nc\td"].map(OsString::from);
        let lines = render_control_command_lines(&arguments).expect("control lines");

        assert_eq!(lines.len(), 1);
        assert!(!lines[0].contains(['\r', '\n', '\t']));
        let parsed = CommandParser::new()
            .with_command_aliases(std::iter::empty::<String>())
            .parse_one_group(&lines[0])
            .expect("control line reparses");
        assert_eq!(
            parsed.commands()[0].arguments()[0].as_string(),
            Some("a\rb\nc\td")
        );
    }
}
