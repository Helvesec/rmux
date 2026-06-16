use rmux_proto::ProcessCommand;

pub(crate) fn from_legacy_command(command: Option<&[String]>) -> Option<ProcessCommand> {
    match command {
        Some([single]) => Some(ProcessCommand::Shell(single.clone())),
        #[cfg(unix)]
        Some(argv) if !argv.is_empty() => Some(ProcessCommand::Shell(shell_join(argv))),
        #[cfg(windows)]
        Some(argv) if !argv.is_empty() => Some(ProcessCommand::Argv(argv.to_vec())),
        _ => None,
    }
}

#[cfg(unix)]
fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|argument| shell_quote(argument))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(unix)]
fn shell_quote(argument: &str) -> String {
    if !argument.is_empty()
        && argument.bytes().all(|byte| {
            matches!(
                byte,
                b'a'..=b'z'
                    | b'A'..=b'Z'
                    | b'0'..=b'9'
                    | b'_'
                    | b'+'
                    | b'='
                    | b':'
                    | b','
                    | b'.'
                    | b'/'
                    | b'-'
            )
        })
    {
        return argument.to_owned();
    }
    format!("'{}'", argument.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use rmux_proto::ProcessCommand;

    #[cfg(unix)]
    #[test]
    fn legacy_command_vectors_become_quoted_shell_commands() {
        let command = vec![
            "bash".to_owned(),
            "-lc".to_owned(),
            "printf '%s\\n' hello world".to_owned(),
        ];

        assert_eq!(
            super::from_legacy_command(Some(&command)),
            Some(ProcessCommand::Shell(
                "bash -lc 'printf '\\''%s\\n'\\'' hello world'".to_owned()
            ))
        );
    }

    #[cfg(unix)]
    #[test]
    fn legacy_command_shell_join_quotes_empty_and_spaces() {
        let command = vec!["printf".to_owned(), "".to_owned(), "a b".to_owned()];

        assert_eq!(
            super::from_legacy_command(Some(&command)),
            Some(ProcessCommand::Shell("printf '' 'a b'".to_owned()))
        );
    }

    #[cfg(windows)]
    #[test]
    fn legacy_command_vectors_stay_argv_on_windows() {
        let command = vec!["cmd.exe".to_owned(), "/C".to_owned(), "echo hi".to_owned()];

        assert_eq!(
            super::from_legacy_command(Some(&command)),
            Some(ProcessCommand::Argv(command))
        );
    }
}
