use super::*;

#[test]
fn top_level_help_footer_tracks_supported_surface_and_aliases() {
    let error = parse_args(&["--help"]).unwrap_err();
    let rendered = error.to_string();

    assert!(rendered.contains("list-commands (lscm)"));
    assert!(rendered.contains("set-window-option (setw)"));
    assert!(rendered.contains("show-window-options (showw)"));
    assert!(rendered.contains("choose-window => choose-tree -w"));
    assert!(rendered.contains("choose-session => choose-tree -s"));
    assert!(rendered.contains("display-menu (menu)"));
    assert!(rendered.contains("display-popup (popup)"));
    assert!(rendered.contains("clear-prompt-history (clearphist)"));
    assert!(rendered.contains("show-prompt-history (showphist)"));
}

#[test]
fn raw_cli_top_level_flags_match_tmux_usage_contract() {
    let mut switch_flags = BTreeSet::new();
    let mut valued_flags = BTreeSet::new();

    for argument in super::super::RawCli::command().get_arguments() {
        let Some(short) = argument.get_short() else {
            continue;
        };

        match argument.get_action() {
            ArgAction::Set | ArgAction::Append => {
                valued_flags.insert(short);
            }
            ArgAction::SetTrue | ArgAction::Count | ArgAction::Help | ArgAction::Version => {
                switch_flags.insert(short);
            }
            action => panic!("unexpected top-level arg action for -{short}: {action:?}"),
        }
    }

    assert_eq!(
        switch_flags,
        BTreeSet::from(['2', 'C', 'D', 'N', 'l', 'u', 'v'])
    );
    assert_eq!(valued_flags, BTreeSet::from(['L', 'S', 'T', 'c', 'f']));

    let help = super::super::RawCli::command().try_get_matches_from(["rmux", "-h"]);
    assert!(matches!(
        help,
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp
    ));

    let version = super::super::RawCli::command().try_get_matches_from(["rmux", "-V"]);
    assert!(matches!(
        version,
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayVersion
    ));
}

#[test]
fn synthetic_completion_tree_tracks_public_command_surface() {
    let completion = super::super::completion_command();
    let actual = completion
        .get_subcommands()
        .map(|command| command.get_name().to_owned())
        .collect::<BTreeSet<_>>();
    let mut expected = super::super::implemented_command_surface()
        .iter()
        .map(|entry| entry.name.to_owned())
        .collect::<BTreeSet<_>>();
    expected.extend(
        super::super::documented_cli_aliases()
            .iter()
            .map(|alias| alias.alias.to_owned()),
    );

    assert_eq!(actual, expected);

    let split_window = completion
        .get_subcommands()
        .find(|command| command.get_name() == "split-window")
        .expect("split-window completion subcommand");
    let horizontal = split_window
        .get_arguments()
        .find(|argument| argument.get_short() == Some('h'))
        .expect("split-window -h horizontal flag");
    assert!(matches!(horizontal.get_action(), ArgAction::SetTrue));
    assert!(
        !split_window
            .get_arguments()
            .any(|argument| argument.get_short() == Some('h')
                && matches!(argument.get_action(), ArgAction::Help)),
        "synthetic completions must not turn split-window -h into clap help"
    );
}

#[test]
fn command_help_completions_and_parser_share_supported_window_flags() {
    let completion = super::super::completion_command();
    for (command_name, supported, unsupported) in [
        (
            "set-window-option",
            &['F', 'a', 'g', 'o', 'q', 't', 'u'][..],
            &['s', 'w', 'p', 'U'][..],
        ),
        (
            "show-window-options",
            &['g', 't', 'v'][..],
            &['A', 'H', 'q', 's', 'w', 'p'][..],
        ),
        ("select-window", &['T', 'l', 'n', 'p', 't'][..], &['Z'][..]),
        ("swap-window", &['d', 's', 't'][..], &['a'][..]),
    ] {
        let help = parse_args(&[command_name, "--help"]).expect_err("--help renders command help");
        assert_eq!(help.kind(), clap::error::ErrorKind::DisplayHelp);
        let help = help.to_string();

        let completion_command = completion
            .get_subcommands()
            .find(|command| command.get_name() == command_name)
            .unwrap_or_else(|| panic!("missing {command_name} completion subcommand"));
        let completion_flags = completion_command
            .get_arguments()
            .filter(|argument| !argument.is_hide_set())
            .filter_map(|argument| argument.get_short())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            completion_flags,
            supported.iter().copied().collect::<BTreeSet<_>>(),
            "{command_name} completion flags drifted from its accepted parser surface"
        );

        for flag in unsupported {
            let rendered_flag = format!("-{flag}");
            assert!(
                !help
                    .lines()
                    .any(|line| line.trim_start().starts_with(&rendered_flag)),
                "{command_name} help advertised rejected flag {rendered_flag}: {help}"
            );
            assert!(
                !completion_flags.contains(flag),
                "{command_name} completion advertised rejected flag {rendered_flag}"
            );

            let error = parse_args(&[command_name, &rendered_flag])
                .expect_err("unsupported public flag must be rejected");
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::UnknownArgument,
                "{command_name} {rendered_flag}: {error}"
            );
            assert!(
                error.to_string().contains(&format!(
                    "command {command_name}: unknown flag {rendered_flag}"
                )),
                "{command_name} {rendered_flag} must retain the tmux diagnostic: {error}"
            );
        }
    }
}

#[test]
fn split_window_percentage_is_public_and_value_taking_everywhere() {
    let cli = parse_args(&["split-window", "-p", "50"])
        .expect("implemented split-window percentage parses");
    match cli.command.expect("parsed command") {
        super::super::Command::SplitWindow(args) => {
            assert_eq!(args.size_spec().as_deref(), Some("50%"));
        }
        _ => panic!("expected SplitWindow command"),
    }

    let help =
        parse_args(&["split-window", "--help"]).expect_err("--help renders split-window help");
    assert_eq!(help.kind(), clap::error::ErrorKind::DisplayHelp);
    assert!(
        help.to_string()
            .lines()
            .any(|line| line.trim_start().starts_with("-p")),
        "split-window help must advertise its implemented -p value flag"
    );

    let completion = super::super::completion_command();
    let split_window = completion
        .get_subcommands()
        .find(|command| command.get_name() == "split-window")
        .expect("split-window completion subcommand");
    let percentage = split_window
        .get_arguments()
        .find(|argument| argument.get_short() == Some('p'))
        .expect("split-window -p percentage completion");
    assert!(matches!(percentage.get_action(), ArgAction::Set));

    assert_eq!(
        rmux_core::command_inventory::render_list_commands_line(
            None,
            "split-window",
            Some("splitw"),
        ),
        "split-window (splitw) [-bdefhIklPvZ] [-c start-directory] [-e environment] [-F format] [-l size] [-p percentage] [-t target-pane] [shell-command [argument ...]]"
    );
}

#[test]
fn refresh_client_unsupported_fields_are_absent_from_cli_help_and_completion() {
    for arguments in [
        &["refresh-client", "-A", "%0:on"][..],
        &["refresh-client", "-B", "name:%0:#{pane_id}"][..],
        &["refresh-client", "-r", "%0:rgb"][..],
        &["refresh-client", "-c"][..],
        &["refresh-client", "-D"][..],
        &["refresh-client", "-L"][..],
        &["refresh-client", "-R"][..],
        &["refresh-client", "-U"][..],
        &["refresh-client", "10"][..],
    ] {
        let error = super::parse_args(arguments).expect_err("unsupported field must not parse");
        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    for arguments in [
        &["refresh-client", "-C", "80x24"][..],
        &["refresh-client", "-f", "active-pane"][..],
        &["refresh-client", "-F", "active-pane"][..],
        &["refresh-client", "-l"][..],
        &["refresh-client", "-S"][..],
        &["refresh-client", "-t", "="][..],
    ] {
        super::parse_args(arguments).expect("supported refresh-client field parses");
    }

    let help =
        super::parse_args(&["refresh-client", "--help"]).expect_err("--help renders command help");
    assert_eq!(help.kind(), clap::error::ErrorKind::DisplayHelp);
    let help = help.to_string();
    assert!(
        !help.contains("adjustment"),
        "refresh-client help advertised unsupported adjustment: {help}"
    );
    for unsupported in ["-A", "-B", "-r", "-c", "-D", "-L", "-R", "-U"] {
        assert!(
            !help
                .lines()
                .any(|line| line.trim_start().starts_with(unsupported)),
            "refresh-client help advertised reserved flag {unsupported}: {help}"
        );
    }

    let completion = super::super::completion_command();
    let refresh = completion
        .get_subcommands()
        .find(|command| command.get_name() == "refresh-client")
        .expect("refresh-client completion subcommand");
    let shorts = refresh
        .get_arguments()
        .filter_map(|argument| argument.get_short())
        .collect::<BTreeSet<_>>();
    assert!(
        refresh
            .get_arguments()
            .all(|argument| argument.get_id() != "adjustment"),
        "refresh-client completion advertised unsupported adjustment"
    );
    for unsupported in ['A', 'B', 'D', 'L', 'R', 'U', 'c', 'r'] {
        assert!(!shorts.contains(&unsupported));
    }
    for supported in ['C', 'F', 'S', 'f', 'l', 't'] {
        assert!(
            shorts.contains(&supported),
            "missing supported -{supported}"
        );
    }

    assert_eq!(
        rmux_core::command_inventory::render_list_commands_line(
            None,
            "refresh-client",
            Some("refresh"),
        ),
        "refresh-client (refresh) [-lS] [-C XxY] [-f flags] [-F flags] [-t target-client]"
    );
}

#[test]
fn command_target_flags_accept_hyphen_prefixed_values() {
    for args in [
        &["list-panes", "-t", "-scratch"][..],
        &["list-windows", "-t", "-scratch"][..],
        &["set-option", "-t", "-scratch", "@flag", "on"][..],
        &["source-file", "-t", "-scratch", "/tmp/rmux.conf"][..],
        &["send-keys", "-t", "-scratch", "Enter"][..],
    ] {
        parse_args(args).unwrap_or_else(|error| {
            panic!("target value beginning with '-' should parse for {args:?}: {error}")
        });
    }
}

#[test]
fn command_value_flags_report_missing_values_with_tmux_style_prefix() {
    for (args, expected) in [
        (
            &["list-panes", "-t"][..],
            "command list-panes: -t expects an argument",
        ),
        (
            &["set-option", "-t"][..],
            "command set-option: -t expects an argument",
        ),
        (
            &["web-share", "--frontend-url"][..],
            "command web-share: --frontend-url expects an argument",
        ),
    ] {
        let error = parse_args(args).unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "expected {expected:?} in {error}"
        );
    }
}

#[test]
fn implemented_surface_matches_the_full_tmux_command_table() {
    let expected = super::super::COMMAND_TABLE
        .iter()
        .map(rendered_surface_entry)
        .collect::<Vec<_>>();
    let tmux_command_names = super::super::COMMAND_TABLE
        .iter()
        .map(|entry| entry.name)
        .collect::<BTreeSet<_>>();
    let actual = super::super::implemented_command_surface()
        .iter()
        .filter(|entry| tmux_command_names.contains(entry.name))
        .map(|entry| rendered_surface_entry(entry))
        .collect::<Vec<_>>();

    assert_eq!(actual, expected);

    for entry in super::super::COMMAND_TABLE {
        assert!(
            help_dispatch_is_supported(entry.name),
            "{} dropped out of the public help/dispatch surface",
            entry.name
        );
    }

    let top_level_help = parse_args(&["--help"]).unwrap_err().to_string();
    assert!(top_level_help.contains("capabilities"));
    assert!(top_level_help.contains("claude"));
    assert!(top_level_help.contains("doctor"));
    assert!(top_level_help.contains("setup"));
}

#[test]
fn default_key_bindings_reference_only_implemented_commands() {
    let implemented = super::super::implemented_command_surface()
        .iter()
        .map(|entry| entry.name.to_owned())
        .collect::<BTreeSet<_>>();
    let store = rmux_core::KeyBindingStore::default();
    let mut referenced = BTreeSet::new();

    for binding in store.list_bindings(None, rmux_core::KeyBindingSortOrder::Name, false) {
        collect_nested_command_names(binding.binding().commands(), &mut referenced);
    }

    let unknown = referenced
        .difference(&implemented)
        .cloned()
        .collect::<BTreeSet<_>>();
    assert!(
        unknown.is_empty(),
        "default list-keys bindings reference commands outside the implemented inventory: {unknown:?}"
    );
    assert!(
        referenced.contains("list-keys"),
        "serverless default-table list-keys should be represented in default bindings"
    );
}

#[test]
fn supported_commands_do_not_treat_short_h_as_clap_help() {
    for entry in super::super::implemented_command_surface() {
        if let Err(error) = parse_args(&[entry.name, "-h"]) {
            assert_ne!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp,
                "{} consumed -h as Clap help",
                entry.name
            );
        }
    }
}

#[test]
fn manpage_surface_matches_implemented_commands_and_aliases() {
    let manpage = repo_file("docs/man/rmux.1");
    let surface_entries = troff_literal_block(
        &manpage,
        ".SH IMPLEMENTED COMMAND SURFACE",
        ".SH BUILT-IN COMMAND ALIASES",
    );
    let expected_entries = super::super::implemented_command_surface()
        .iter()
        .map(|entry| rendered_surface_entry(entry))
        .collect::<Vec<_>>();
    assert_eq!(surface_entries, expected_entries);
    assert!(manpage.contains(&format!(
        "The public CLI dispatches {} commands:",
        expected_entries.len()
    )));

    let alias_section = troff_section(&manpage, ".SH BUILT-IN COMMAND ALIASES", ".SH NOTES");
    for alias in super::super::documented_cli_aliases() {
        assert!(alias_section.contains(&format!(".B {}", alias.alias)));
        assert!(alias_section.contains(&format!(".BR \"{}\" .", alias.expansion)));
    }

    assert!(manpage.contains(".B -Vh"));
    assert!(manpage.contains(&format!("\"RMUX {}\"", env!("CARGO_PKG_VERSION"))));
    assert!(manpage.contains(".RB [ -2CDhlNuVv ]"));
    assert!(manpage.contains(".BR \"rmux <command> --help\" ."));
    assert!(manpage.contains(".BR \"rmux split-window -h\" ."));
}

fn collect_nested_command_names(
    commands: &rmux_core::command_parser::ParsedCommands,
    names: &mut BTreeSet<String>,
) {
    for command in commands.commands() {
        names.insert(command.name().to_owned());
        for argument in command.arguments() {
            if let rmux_core::command_parser::CommandArgument::Commands(nested) = argument {
                collect_nested_command_names(nested, names);
            }
        }
    }
}
