use super::parse;
use clap::{ArgAction, CommandFactory};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

const FROZEN_TMUX_REFERENCE: &str = "tests/reference/tmux_compat/frozen_reference.yaml";
const ERROR_EXIT_MATRIX: &str = "tests/reference/tmux_compat/error_exit_matrix.yaml";

#[test]
fn long_option_values_stay_opaque_in_all_schema_projections() {
    use super::schema::CommandSpec;

    let completion = super::completion_command();
    for (name, flags, id) in [
        ("pane-snapshot", &["-t", "--target"][..], "target"),
        (
            "web-share",
            &["--frontend-url", "--web-frontend"][..],
            "frontend_url",
        ),
    ] {
        for flag in flags {
            for value in ["-tfoo", "-ltfoo", "--help"] {
                for attached in [false, true] {
                    if attached && !flag.starts_with("--") {
                        continue;
                    }
                    let arguments = if attached {
                        vec![format!("{flag}={value}")]
                    } else {
                        vec![(*flag).to_owned(), value.to_owned()]
                    };
                    for projection in [
                        super::schema::command_help(name).unwrap(),
                        completion.find_subcommand(name).unwrap().clone(),
                    ] {
                        let matches = projection.try_get_matches_from(&arguments).unwrap();
                        assert_eq!(matches.get_raw(id).unwrap().next().unwrap(), value);
                    }
                    let typed = match name {
                        "pane-snapshot" => {
                            super::schema::PaneSnapshot::parse(arguments.clone(), String::new())
                                .map(super::Command::PaneSnapshot)
                        }
                        _ => super::schema::WebShare::parse(arguments.clone(), String::new())
                            .map(super::Command::WebShare),
                    };
                    let argv = ["rmux".to_owned(), name.to_owned()]
                        .into_iter()
                        .chain(arguments.iter().cloned())
                        .collect::<Vec<_>>();
                    let group = super::RuntimeCommandGroup::Canonical(
                        std::iter::once(name.to_owned())
                            .chain(arguments.iter().cloned())
                            .collect::<Vec<_>>()
                            .join(" "),
                    );
                    for result in [
                        typed,
                        parse(argv.clone()).map(|cli| cli.command.unwrap()),
                        super::parse_with_runtime_command_groups(argv, &[group])
                            .map(|cli| cli.command.unwrap()),
                    ] {
                        let command =
                            result.unwrap_or_else(|error| panic!("{name} {arguments:?}: {error}"));
                        let actual = match command {
                            super::Command::PaneSnapshot(args) => {
                                args.target.unwrap().raw().to_owned()
                            }
                            super::Command::WebShare(args) => args.frontend_url.unwrap(),
                            _ => panic!("unexpected command"),
                        };
                        assert_eq!(actual, value, "{name} {arguments:?}");
                    }
                }
            }
        }
    }
}

fn parse_args(args: &[&str]) -> Result<super::Cli, clap::Error> {
    let mut full_args = vec!["rmux"];
    full_args.extend_from_slice(args);
    parse(full_args)
}

fn target_text(target: &Option<super::TargetSpec>) -> String {
    target.as_ref().expect("target").to_string()
}

fn repo_file(path: &str) -> String {
    fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path))
        .unwrap_or_else(|error| panic!("failed to read {path}: {error}"))
}

fn troff_section<'a>(contents: &'a str, heading: &str, next_heading: &str) -> &'a str {
    contents
        .split_once(heading)
        .and_then(|(_, tail)| tail.split_once(next_heading).map(|(section, _)| section))
        .unwrap_or_else(|| panic!("missing troff section {heading}"))
}

fn troff_literal_block(contents: &str, heading: &str, next_heading: &str) -> Vec<String> {
    troff_section(contents, heading, next_heading)
        .split_once(".nf\n")
        .and_then(|(_, tail)| tail.split_once("\n.fi").map(|(block, _)| block))
        .unwrap_or_else(|| panic!("missing literal block under {heading}"))
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn rendered_surface_entry(entry: &rmux_core::command_parser::CommandEntry) -> String {
    match entry.alias {
        Some(alias) => format!("{} ({alias})", entry.name),
        None => entry.name.to_owned(),
    }
}

fn help_dispatch_is_supported(name: &str) -> bool {
    let parsed = super::TmuxCommandParser::new()
        .parse(&format!("{name} --help"))
        .unwrap_or_else(|error| panic!("failed to parse help probe for {name}: {error}"))
        .into_commands()
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("missing parsed command for {name}"));

    match super::command_from_parsed(parsed) {
        Ok(super::Command::Unsupported(_)) => false,
        Ok(_) => true,
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => true,
        Err(error) => panic!("{name} --help failed dispatch classification: {error}"),
    }
}

#[test]
fn direct_cli_rejects_unknown_options_before_command_tails() {
    for (command, arguments) in [
        ("wait-for", &["-Q"][..]),
        ("pipe-pane", &["-Q", "true"][..]),
        ("respawn-pane", &["-Q", "true"][..]),
        ("bind-key", &["-Q", "X", "display-message", "ok"][..]),
        ("display-panes", &["-Q", "select-pane -t %%"][..]),
    ] {
        let mut invocation = Vec::with_capacity(arguments.len() + 1);
        invocation.push(command);
        invocation.extend_from_slice(arguments);

        let error = parse_args(&invocation).expect_err("unknown option must fail direct CLI parse");
        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
        assert!(
            error
                .to_string()
                .contains(&format!("command {command}: unknown flag -Q")),
            "unexpected direct CLI error for {command}: {error}"
        );
    }

    for (command, arguments) in [
        ("send-keys", &["-x", "C-c"][..]),
        ("rename-session", &["-x"][..]),
        ("rename-window", &["-x"][..]),
        ("set-buffer", &["-x", "payload"][..]),
        ("display-message", &["-p", "-x"][..]),
        ("set-environment", &["-x", "poisoned"][..]),
    ] {
        let mut invocation = Vec::with_capacity(arguments.len() + 1);
        invocation.push(command);
        invocation.extend_from_slice(arguments);

        let error =
            parse_args(&invocation).expect_err("unknown option must fail direct CLI parsing");
        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
        assert!(
            error.to_string().contains("-x"),
            "unexpected direct CLI error for {command}: {error}"
        );
    }
}

#[test]
fn direct_cli_rejects_new_unknown_option_shapes_before_positionals() {
    for (command, arguments) in [
        ("set-option", &["-x", "value"][..]),
        ("set-option", &["--bogus", "value"][..]),
        ("set-option", &["-gx", "value"][..]),
        ("set-window-option", &["-x", "value"][..]),
        ("set-window-option", &["--bogus", "value"][..]),
        ("set-window-option", &["-gx", "value"][..]),
        ("show-options", &["-x"][..]),
        ("show-options", &["--bogus"][..]),
        ("show-options", &["-gx"][..]),
        ("show-window-options", &["-x"][..]),
        ("show-window-options", &["--bogus"][..]),
        ("show-window-options", &["-gx"][..]),
        ("unbind-key", &["-x"][..]),
        ("unbind-key", &["--bogus"][..]),
        ("unbind-key", &["-nx"][..]),
        ("list-keys", &["-x"][..]),
        ("list-keys", &["--bogus"][..]),
        ("list-keys", &["-ax"][..]),
        ("select-layout", &["-x"][..]),
        ("select-layout", &["--bogus"][..]),
        ("select-layout", &["-nx"][..]),
    ] {
        let mut invocation = Vec::with_capacity(arguments.len() + 1);
        invocation.push(command);
        invocation.extend_from_slice(arguments);

        let error =
            parse_args(&invocation).expect_err("unknown option must fail direct CLI parsing");
        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
        // tmux 3.7b: `-x` is unknown; `--bogus` is the invalid flag '-'.
        let flag = if arguments[0] == "--bogus" {
            "invalid flag --".to_owned()
        } else {
            "unknown flag -x".to_owned()
        };
        let rendered = error.to_string();
        assert!(
            rendered.contains(&format!("command {command}: {flag}")),
            "unexpected direct CLI error for {command} {arguments:?}: {rendered}"
        );
    }
}

#[test]
fn direct_cli_names_unknown_flags_and_extra_arguments_like_tmux() {
    // tmux 3.7b, measured with a server on a private socket.
    for (arguments, kind, expected) in [
        (
            &["list-sessions", "-Z"][..],
            clap::error::ErrorKind::UnknownArgument,
            "command list-sessions: unknown flag -Z",
        ),
        (
            &["has-session", "-Z"][..],
            clap::error::ErrorKind::UnknownArgument,
            "command has-session: unknown flag -Z",
        ),
        (
            &["list-windows", "-Q"][..],
            clap::error::ErrorKind::UnknownArgument,
            "command list-windows: unknown flag -Q",
        ),
        (
            &["capture-pane", "-p", "-Q"][..],
            clap::error::ErrorKind::UnknownArgument,
            "command capture-pane: unknown flag -Q",
        ),
        (
            &["list-windows", "-aQ"][..],
            clap::error::ErrorKind::UnknownArgument,
            "command list-windows: unknown flag -Q",
        ),
        (
            &["list-sessions", "-."][..],
            clap::error::ErrorKind::UnknownArgument,
            "command list-sessions: invalid flag -.",
        ),
        (
            &["new-window", "-."][..],
            clap::error::ErrorKind::UnknownArgument,
            "command new-window: invalid flag -.",
        ),
        (
            &["has-session", "--bogus"][..],
            clap::error::ErrorKind::UnknownArgument,
            "command has-session: invalid flag --",
        ),
        (
            &["new-window", "--bogus"][..],
            clap::error::ErrorKind::UnknownArgument,
            "command new-window: invalid flag --",
        ),
        (
            &["list-sessions", "-Z", "extra"][..],
            clap::error::ErrorKind::UnknownArgument,
            "command list-sessions: unknown flag -Z",
        ),
        (
            &["list-sessions", "extra"][..],
            clap::error::ErrorKind::TooManyValues,
            "command list-sessions: too many arguments (need at most 0)",
        ),
        (
            &["list-sessions", "extra", "-Z"][..],
            clap::error::ErrorKind::TooManyValues,
            "command list-sessions: too many arguments (need at most 0)",
        ),
        (
            &["list-sessions", "-"][..],
            clap::error::ErrorKind::TooManyValues,
            "command list-sessions: too many arguments (need at most 0)",
        ),
        (
            &["list-sessions", "--", "extra"][..],
            clap::error::ErrorKind::TooManyValues,
            "command list-sessions: too many arguments (need at most 0)",
        ),
        (
            &["kill-server", "extra"][..],
            clap::error::ErrorKind::TooManyValues,
            "command kill-server: too many arguments (need at most 0)",
        ),
        (
            &["kill-server", "--", "extra"][..],
            clap::error::ErrorKind::TooManyValues,
            "command kill-server: too many arguments (need at most 0)",
        ),
        (
            &["has-session", "-t", "s0", "extra"][..],
            clap::error::ErrorKind::TooManyValues,
            "command has-session: too many arguments (need at most 0)",
        ),
        (
            &["list-windows", "-a", "--", "x"][..],
            clap::error::ErrorKind::TooManyValues,
            "command list-windows: too many arguments (need at most 0)",
        ),
        (
            &["capture-pane", "-p", "-t", "s0", "extra"][..],
            clap::error::ErrorKind::TooManyValues,
            "command capture-pane: too many arguments (need at most 0)",
        ),
        (
            &["rename-window", "--", "a", "b"][..],
            clap::error::ErrorKind::TooManyValues,
            "command rename-window: too many arguments (need at most 1)",
        ),
        (
            &["list-sessions", "-?Z"][..],
            clap::error::ErrorKind::UnknownArgument,
            "usage: list-sessions [-r] [-F format] [-f filter] [-O order]",
        ),
        (
            &["list-sessions", "-Z?"][..],
            clap::error::ErrorKind::UnknownArgument,
            "command list-sessions: unknown flag -Z",
        ),
        (
            &["has-session", "-?"][..],
            clap::error::ErrorKind::UnknownArgument,
            "usage: has-session [-t target-session]",
        ),
    ] {
        let error = parse_args(arguments).expect_err("tmux rejects this command line");
        assert_eq!(error.kind(), kind, "{arguments:?}: {error}");
        assert_eq!(
            error.to_string(),
            format!("error: {expected}"),
            "{arguments:?}"
        );
    }
}

#[test]
fn direct_cli_help_request_ignores_the_tokens_after_it() {
    // Clap prints a command's help as soon as it reads --help, so the option
    // scan must not answer `list-sessions --help x` with "too many arguments".
    // A command read before clap, such as new-pane, keeps its own answer.
    let mut checked = 0;
    for entry in super::COMMAND_TABLE {
        let Err(help) = parse_args(&[entry.name, "--help"]) else {
            continue;
        };
        if help.kind() != clap::error::ErrorKind::DisplayHelp {
            continue;
        }
        for tail in [&["x"][..], &["x", "y"], &["--", "x"]] {
            let mut arguments = vec![entry.name, "--help"];
            arguments.extend_from_slice(tail);
            let Err(error) = parse_args(&arguments) else {
                continue;
            };
            assert_eq!(error.kind(), help.kind(), "{arguments:?}: {error}");
            assert_eq!(error.to_string(), help.to_string(), "{arguments:?}");
        }
        checked += 1;
    }
    assert!(checked > 40, "only {checked} commands print help");
}

#[test]
fn direct_cli_stops_parsing_options_after_the_first_positional() {
    for arguments in [
        &["rename-session", "renamed", "-t", "audit"][..],
        &["set-buffer", "payload", "-b", "named"][..],
        &["set-option", "status", "off", "-g"][..],
        &["set-environment", "FOO", "BAR", "-g"][..],
        &["list-commands", "list-windows", "-F", "format"][..],
    ] {
        let error = parse_args(arguments).expect_err("post-positional option must be rejected");
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::TooManyValues,
            "unexpected error kind for {arguments:?}: {error}"
        );
        assert!(
            error.to_string().contains("too many arguments"),
            "unexpected direct CLI error for {arguments:?}: {error}"
        );
    }
}

#[test]
fn direct_cli_consumes_option_like_required_option_values() {
    let cli = parse_args(&["list-windows", "-F", "-tfoo", "-t", "beta"])
        .expect("list-windows must consume a hyphenated format before its target");
    let super::Command::ListWindows(args) = cli.command.expect("list-windows command") else {
        panic!("expected list-windows command");
    };
    assert_eq!(args.format.as_deref(), Some("-tfoo"));
    assert_eq!(target_text(&args.target), "beta");

    let cli = parse_args(&["list-panes", "-F", "--", "-t", "beta:0"])
        .expect("list-panes must consume a literal separator as its format");
    let super::Command::ListPanes(args) = cli.command.expect("list-panes command") else {
        panic!("expected list-panes command");
    };
    assert_eq!(args.format.as_deref(), Some("--"));
    assert_eq!(target_text(&args.target), "beta:0");

    let cli = parse_args(&["list-sessions", "-F", "-Q", "-r"])
        .expect("list-sessions must consume an unknown-looking format token");
    let super::Command::ListSessions(args) = cli.command.expect("list-sessions command") else {
        panic!("expected list-sessions command");
    };
    assert_eq!(args.format.as_deref(), Some("-Q"));
    assert!(args.reversed);

    let cli = parse_args(&["list-buffers", "-F", "-tfoo", "-r"])
        .expect("list-buffers must consume its hyphenated format before later flags");
    let super::Command::ListBuffers(args) = cli.command.expect("list-buffers command") else {
        panic!("expected list-buffers command");
    };
    assert_eq!(args.format.as_deref(), Some("-tfoo"));
    assert!(args.reversed);

    let cli = parse_args(&["break-pane", "-F", "-Q", "-s", "alpha:0.0", "-t", "beta:0"])
        .expect("break-pane must consume its option-like format before source and target flags");
    let super::Command::BreakPane(args) = cli.command.expect("break-pane command") else {
        panic!("expected break-pane command");
    };
    assert_eq!(args.format.as_deref(), Some("-Q"));
    assert_eq!(target_text(&args.source), "alpha:0.0");
    assert_eq!(target_text(&args.target), "beta:0");

    let error = parse_args(&["list-windows", "-Q"])
        .expect_err("an option-like token outside a value position must remain invalid");
    assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
}

#[test]
fn direct_cli_keeps_optional_option_values_separate_from_following_flags() {
    let cli = parse_args(&["resize-pane", "-D", "-t", "beta:0.0"])
        .expect("an optional resize delta must not consume the following target flag");
    let super::Command::ResizePane(args) = cli.command.expect("resize-pane command") else {
        panic!("expected resize-pane command");
    };
    assert_eq!(args.down, Some(1));
    assert_eq!(target_text(&args.target), "beta:0.0");
}

#[test]
fn direct_cli_keeps_option_prefix_and_command_tail_semantics() {
    for arguments in [
        &["rename-session", "-t", "audit", "renamed"][..],
        &["set-buffer", "-b", "named", "payload"][..],
        &["set-option", "-g", "status", "off"][..],
        &["set-environment", "-g", "FOO", "BAR"][..],
    ] {
        parse_args(arguments).unwrap_or_else(|error| {
            panic!("valid option prefix failed for {arguments:?}: {error}")
        });
    }

    let cli = parse_args(&["new-session", "-d", "sh", "-c", "printf ok"])
        .expect("child command flags must remain positional tail values");
    let super::Command::NewSession(args) = cli.command.expect("new-session command") else {
        panic!("expected new-session command");
    };
    assert_eq!(args.command, ["sh", "-c", "printf ok"]);

    let cli = parse_args(&["set-option", "@tail-value", "-g"])
        .expect("recognized flag text after an option name must stay a value");
    let super::Command::SetOption(args) = cli.command.expect("set-option command") else {
        panic!("expected set-option command");
    };
    assert!(!args.global);
    assert_eq!(args.value.as_deref(), Some("-g"));

    let cli = parse_args(&["set-environment", "TAIL_VALUE", "-g"])
        .expect("recognized flag text after an environment name must stay a value");
    let super::Command::SetEnvironment(args) = cli.command.expect("set-environment command") else {
        panic!("expected set-environment command");
    };
    assert!(!args.global);
    assert_eq!(args.value.as_deref(), Some("-g"));

    let cli = parse_args(&["set-option", "-g", "@compact", "-tfoo"])
        .expect("an option-like value after the option name must stay positional");
    let super::Command::SetOption(args) = cli.command.expect("set-option command") else {
        panic!("expected set-option command");
    };
    assert!(args.global);
    assert!(args.target.is_none());
    assert_eq!(args.option, "@compact");
    assert_eq!(args.value.as_deref(), Some("-tfoo"));

    let cli = parse_args(&["source-file", "missing.conf", "-tfoo"])
        .expect("an option-like path after the first path must stay positional");
    let super::Command::SourceFile(args) = cli.command.expect("source-file command") else {
        panic!("expected source-file command");
    };
    assert!(args.target.is_none());
    assert_eq!(args.paths, ["missing.conf", "-tfoo"]);
}

#[path = "cli_args_tests/session_and_top_level.rs"]
mod session_and_top_level;

#[path = "cli_args_tests/window_commands.rs"]
mod window_commands;

#[path = "cli_args_tests/overlays_and_prompts.rs"]
mod overlays_and_prompts;

#[path = "cli_args_tests/surface_docs.rs"]
mod surface_docs;

#[path = "cli_args_tests/compat_reference.rs"]
mod compat_reference;

#[path = "cli_args_tests/queue_and_window_ops.rs"]
mod queue_and_window_ops;

#[path = "cli_args_tests/pane_layout.rs"]
mod pane_layout;

#[path = "cli_args_tests/pane_io.rs"]
mod pane_io;

#[path = "cli_args_tests/automation.rs"]
mod automation;

#[path = "cli_args_tests/scripting_and_buffers.rs"]
mod scripting_and_buffers;

#[path = "cli_args_tests/options_and_scopes.rs"]
mod options_and_scopes;

#[path = "cli_args_tests/fmp1.rs"]
mod fmp1;
#[path = "cli_args_tests/server_lifecycle.rs"]
mod server_lifecycle;

#[path = "cli_args_tests/delegate_help.rs"]
mod delegate_help;

#[path = "cli_args_tests/delegate_neighbors.rs"]
mod delegate_neighbors;

#[path = "cli_args_tests/resize_value_ownership.rs"]
mod resize_value_ownership;

#[path = "cli_args_tests/resize_value_boundaries.rs"]
mod resize_value_boundaries;

#[path = "cli_args_tests/option_value_ownership.rs"]
mod option_value_ownership;
