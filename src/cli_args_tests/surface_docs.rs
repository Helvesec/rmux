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
fn implemented_surface_matches_the_full_tmux_command_table() {
    let expected = super::super::COMMAND_TABLE
        .iter()
        .map(rendered_surface_entry)
        .collect::<Vec<_>>();
    let actual = super::super::implemented_command_surface()
        .iter()
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
fn overview_supported_surface_matches_implemented_commands_and_aliases() {
    let overview = repo_file("public overview");
    let supported_surface =
        structured_section(&overview, "## Supported Surface", "## Workspace Layout");

    let documented_commands = supported_surface
        .lines()
        .filter(|line| line.starts_with("- ") && line.contains(": `"))
        .flat_map(extract_backtick_tokens)
        .collect::<BTreeSet<_>>();
    let expected_commands = super::super::implemented_command_surface()
        .iter()
        .map(|entry| entry.name.to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(documented_commands, expected_commands);
    assert!(supported_surface.contains(&format!("dispatches {} commands", expected_commands.len())));

    let documented_aliases = supported_surface
        .lines()
        .filter(|line| line.starts_with("- `") && line.contains("=>"))
        .map(|line| {
            let tokens = extract_backtick_tokens(line);
            assert_eq!(tokens.len(), 2, "unexpected public overview alias line: {line}");
            format!("{} => {}", tokens[0], tokens[1])
        })
        .collect::<BTreeSet<_>>();
    let expected_aliases = super::super::documented_cli_aliases()
        .iter()
        .map(|alias| format!("{} => {}", alias.alias, alias.expansion))
        .collect::<BTreeSet<_>>();
    assert_eq!(documented_aliases, expected_aliases);

    for entry in super::super::COMMAND_TABLE
        .iter()
        .filter(|entry| !expected_commands.contains(entry.name))
    {
        assert!(
            !supported_surface.contains(&format!("`{}`", entry.name)),
            "public overview supported surface should not document unsupported command {}",
            entry.name
        );
    }

    let note_section = structured_section(&overview, "## Supported Surface", "## Workspace Layout");
    assert!(note_section.contains("`link-window` and `unlink-window`"));
    assert!(note_section.contains("share tmux-style window links"));
    assert!(supported_surface.contains("`rmux <command> --help`"));
    assert!(supported_surface.contains("`split-window -h`"));
    assert!(supported_surface.contains("`rmux -Vh`"));
}

#[test]
fn manpage_surface_matches_implemented_commands_and_aliases() {
    let manpage = repo_file("rmux.1");
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
    assert!(manpage.contains(".BR \"rmux <command> --help\" ."));
    assert!(manpage.contains(".BR \"rmux split-window -h\" ."));
}

#[test]
fn release_docs_track_workspace_version() {
    let version = env!("CARGO_PKG_VERSION");
    let cargo_toml = repo_file("Cargo.toml");
    let overview = repo_file("public overview");
    let manpage = repo_file("rmux.1");
    let specification = repo_file("spec/specification.txt");
    let ledger = repo_file("spec/tmux-manpage-reference.yaml");

    assert!(cargo_toml.contains(&format!("version = \"{version}\"")));
    assert!(overview.contains(&format!("RMUX {version}")));
    assert!(manpage.contains(&format!("\"RMUX {version}\"")));
    assert!(manpage.contains(&format!(".B rmux {version}")));
    assert!(specification.contains(&format!("RMUX {version}")));
    assert!(ledger.contains(&format!("release_line: \"{version}\"")));
}
