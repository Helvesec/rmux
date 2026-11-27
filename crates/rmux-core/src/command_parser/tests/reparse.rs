use super::super::*;

fn scalar_values() -> Vec<String> {
    let prefixes = [
        "",
        "/",
        "cmy:0/",
        "C:",
        "C:\\",
        "\\\\host\\share\\",
        ".\\",
        "--script=C:/",
        "é/",
        "🚀/",
    ];
    let mut values = Vec::new();
    for prefix in prefixes {
        for character in (0u8..=31).chain([127]).map(char::from) {
            values.push(format!("{prefix}{character}tail/$HOME/'\""));
        }
    }
    values.extend(
        [
            "\\n/later",
            "\\\\host\\share\\",
            "C:\\tools\\new\\",
            "--script=.\\tools\\",
            "--script=C:\\folder=one\\new",
            "\\\\?\\C:\\tools\\",
            "\"\\n/later",
            "'\\v/later",
            "plain",
            "",
            "/$HOME",
            "C:$HOME",
            "foo/$HOME",
            "é/$HOME",
            "🚀/$HOME",
            "~/$HOME",
            "/\"$HOME",
            "/'quoted'/$HOME",
        ]
        .map(str::to_owned),
    );
    values
}

fn scalar_command(value: &str) -> ParsedCommands {
    CommandParser::new()
        .parse_arguments(["set-buffer", "-b", "roundtrip", value])
        .unwrap()
}

#[test]
fn canonical_reparse_preserves_every_control_and_windows_path_prefix() {
    for value in scalar_values() {
        let parsed = scalar_command(&value);
        let rendered = parsed.to_tmux_reparse_string();
        for (name, reparsed) in [
            ("command", CommandParser::new().parse(&rendered)),
            ("source", CommandParser::new().parse_source_file(&rendered)),
            ("one group", CommandParser::new().parse_one_group(&rendered)),
        ] {
            let reparsed = reparsed.unwrap_or_else(|error| panic!("{name}: {rendered:?}: {error}"));
            assert_eq!(
                reparsed.commands(),
                parsed.commands(),
                "{name}: {value:?}: {rendered:?}"
            );
            assert_eq!(reparsed.assignments(), parsed.assignments());
        }
    }
}

#[test]
fn canonical_reparse_preserves_assignments_and_nested_command_lists() {
    let value = "C:/\x0b\r\n\t\\tail/$HOME/'\"";
    let parser = CommandParser::new().with_environment_value("HOME", "must-not-expand");
    let parsed = parser
        .parse_arguments_with_assignments([
            format!("VALUE={value}"),
            "set-buffer".into(),
            "-b".into(),
            "roundtrip".into(),
            value.into(),
        ])
        .unwrap();
    let rendered = parsed.to_tmux_reparse_string();
    assert_eq!(parser.parse(&rendered).unwrap(), parsed, "{rendered:?}");
    let assignments = parsed.assignments_to_tmux_reparse_string();
    assert_eq!(
        parser.parse(&assignments).unwrap().assignments(),
        parsed.assignments()
    );

    let nested = scalar_command(value);
    let outer = parser
        .parse_arguments(["if-shell", "-F", "1", "placeholder"])
        .unwrap();
    let mut command = outer.commands()[0].clone();
    let mut arguments = command.arguments().to_vec();
    arguments[2] = CommandArgument::Commands(nested);
    command = command.with_arguments(arguments);
    let rendered = command.to_tmux_reparse_string();
    assert_eq!(
        parser.parse(&rendered).unwrap().commands(),
        &[command],
        "{rendered:?}"
    );
}

#[test]
fn canonical_reparse_preserves_binding_strings_and_repeated_roundtrips() {
    for value in scalar_values() {
        let original = scalar_command(&value);
        let mut parsed = original.clone();
        for _ in 0..3 {
            let rendered = parsed.to_tmux_binding_string();
            parsed = CommandParser::new().parse(&rendered).unwrap();
            assert_eq!(parsed, original, "{value:?}: {rendered:?}");
        }
    }
}

#[cfg(windows)]
#[test]
fn canonical_reparse_numeric_prefix_keeps_explicit_escapes_in_text_mode() {
    for (source, expected) in [
        (r#""\057\v""#, "/\x0b"),
        (r#""\103:\n\t\v""#, "C:\n\t\x0b"),
        (r#""\u0043:/\v""#, "C:/\x0b"),
        (r#""\U0001F680/\v""#, "🚀/\x0b"),
        (r#""\134\134host\\share\\""#, "\\\\host\\share\\"),
    ] {
        let parsed = CommandParser::new()
            .parse(&format!("display-message {source}"))
            .unwrap();
        assert_eq!(
            parsed.commands()[0].arguments()[0].as_string(),
            Some(expected),
            "{source}"
        );
    }
    for source in [r#""\057\400""#, r#""\103:\u12xz""#] {
        assert!(CommandParser::new()
            .parse(&format!("display-message {source}"))
            .is_err());
    }
}

#[cfg(windows)]
#[test]
fn canonical_reparse_does_not_change_literal_windows_config_paths() {
    let paths = [
        r"C:\new\tab\vspace\033\u1234",
        r"C:\\new\\tab",
        r"\\host\share\new\vspace",
        r"\\?\C:\new\vspace",
        r".\new\tab",
        r"..\new\tab",
        r"--script=C:\folder=one\new",
    ];
    for path in paths {
        let source = format!("display-message \"{path}\"");
        let parser = CommandParser::new();
        let before = parser.parse(&source).unwrap();
        let after = parser.parse_source_file(&source).unwrap();
        assert_eq!(before, after);
        let expected = if path == r"C:\\new\\tab" {
            r"C:\new\tab"
        } else {
            path
        };
        assert_eq!(
            before.commands()[0].arguments()[0].as_string(),
            Some(expected)
        );
        let canon = before.to_tmux_reparse_string();
        assert_eq!(parser.parse(&canon).unwrap(), before, "{canon:?}");
    }
}
