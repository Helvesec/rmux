use super::super::{parse, parse_with_runtime_command_groups, Command, RuntimeCommandGroup};

fn parse_both(arguments: &[&str]) -> [Result<super::super::Cli, clap::Error>; 2] {
    let argv: Vec<_> = std::iter::once("rmux")
        .chain(arguments.iter().copied())
        .collect();
    let groups = [RuntimeCommandGroup::Canonical(arguments.join(" "))];
    [
        parse(argv.clone()),
        parse_with_runtime_command_groups(argv, &groups),
    ]
}

#[test]
fn delegated_help_preserves_each_queue_policy() {
    for command in [
        "capabilities",
        "claude",
        "doctor",
        "setup",
        "new-pane",
        "set-pane-placement",
    ] {
        for tail in [
            &[][..],
            &["--help"][..],
            &["--help", "--json"][..],
            &["--json", "--help"][..],
        ] {
            let args: Vec<_> = std::iter::once(command)
                .chain(tail.iter().copied())
                .collect();
            for result in parse_both(&args) {
                let has_help = tail.contains(&"--help");
                if command == "capabilities" && has_help {
                    let error = result.expect_err("any capabilities help aborts parsing");
                    assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
                    assert_eq!(
                        error.to_string(),
                        "error: usage: rmux capabilities [--human|--json]\n"
                    );
                } else if matches!(command, "new-pane" | "set-pane-placement") && tail == ["--help"]
                {
                    let error = result.expect_err("negotiated help-only request");
                    assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
                    assert!(error.to_string().starts_with(&format!("Usage: {command} ")));
                } else {
                    let cli = result.expect("other delegated tails are unsupported, not help");
                    let Command::Unsupported(unsupported) = cli.command.expect("command") else {
                        panic!("{args:?} must remain delegated");
                    };
                    assert_eq!(unsupported.name, command);
                    assert_eq!(unsupported.arguments, tail);
                    assert_eq!(cli.command_queue.len(), 1);
                }
            }
        }
    }
}

#[test]
fn delegated_help_keeps_queue_order_and_error_precedence() {
    for command in ["claude", "doctor", "setup"] {
        let args = [
            "set-option",
            "-g",
            "@r2",
            "changed",
            ";",
            command,
            "--help",
            ";",
            "show-options",
            "-g",
            "@r2",
        ];
        for result in parse_both(&args) {
            let cli =
                result.expect("local help tokens do not turn queued delegates into help exits");
            assert!(matches!(cli.command, Some(Command::SetOption(_))));
            let [Command::SetOption(_), Command::Unsupported(delegated), Command::ShowOptions(_)] =
                cli.command_queue.as_slice()
            else {
                panic!("queued command order changed");
            };
            assert_eq!(delegated.name, command);
            assert_eq!(delegated.arguments, ["--help"]);
        }
    }
    for args in [
        &[
            "capabilities",
            "--help",
            ";",
            "set-option",
            "-g",
            "@r2",
            "changed",
        ][..],
        &[
            "set-option",
            "-g",
            "@r2",
            "before",
            ";",
            "capabilities",
            "--json",
            "--help",
            ";",
            "set-option",
            "-g",
            "@r2",
            "after",
        ][..],
        &["capabilities", "--help", ";", "set-option", "--help"][..],
    ] {
        for result in parse_both(args) {
            let error = result.expect_err("capabilities help returns no queue");
            assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
            assert_eq!(
                error.to_string(),
                "error: usage: rmux capabilities [--human|--json]\n"
            );
        }
    }
    for (args, expected) in [
        (
            &["set-option", "--help", ";", "capabilities", "--help"][..],
            clap::error::ErrorKind::DisplayHelp,
        ),
        (
            &["set-option", "-g", ";", "capabilities", "--help"][..],
            clap::error::ErrorKind::MissingRequiredArgument,
        ),
        (
            &["capabilities", "--help", ";", "set-option", "-g"][..],
            clap::error::ErrorKind::DisplayHelp,
        ),
    ] {
        for result in parse_both(args) {
            let error = result.expect_err("queue parsing stops at its first typed error");
            assert_eq!(error.kind(), expected, "{args:?}: {error}");
            if args[0] == "set-option" {
                assert!(error.to_string().contains("set-option"));
            }
        }
    }
}
