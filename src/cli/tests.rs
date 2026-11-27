#[cfg(unix)]
use super::shell_startup::usable_shell_path;
use super::{
    command_has_start_server_flag, default_client_command, render_list_commands_line, run,
    same_file_identity_for_paths, server_starting_command, startup_config_from_cli,
    top_level_decision, ServerStartupConfig,
};
use crate::cli_args::{
    parse as parse_cli, parse_target_spec, AttachSessionArgs, Command, ListSessionsArgs,
    NewWindowArgs, StartServerArgs,
};
use rmux_core::command_inventory::render_list_commands;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::top_level::TopLevelDecision;

static UNIQUE_TEST_ID: AtomicUsize = AtomicUsize::new(0);

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn unique_test_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "rmux-cli-{label}-{}-{}",
        std::process::id(),
        UNIQUE_TEST_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

#[test]
fn native_version_completion_agrees_with_early_decision() {
    use clap::error::ErrorKind;
    let error = crate::cli_args::completion_command()
        .try_get_matches_from(["rmux", "--version"])
        .expect_err("completion advertises the native version flag");
    assert_eq!(error.kind(), ErrorKind::DisplayVersion);
    let exit = run(args(&["rmux", "--version"])).expect_err("local version exit");
    assert_eq!(exit.exit_code(), 0);
    assert!(!exit.use_stderr());
    assert_eq!(error.to_string().trim(), exit.message());
}

#[test]
fn top_level_preparse_accepts_tmux_short_help() {
    for values in [&["-h"][..], &["-Nh"][..], &["-hV"][..]] {
        assert!(matches!(
            top_level_decision(&args(values), false),
            Some(TopLevelDecision::Usage)
        ));
        let invocation = std::iter::once("rmux")
            .chain(values.iter().copied())
            .collect::<Vec<_>>();
        let error = run(args(&invocation)).expect_err("expected short help exit");
        assert_eq!(error.exit_code(), 0);
        assert!(!error.use_stderr());
        assert_eq!(
            error.message(),
            "usage: rmux [-2CDhlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]"
        );
    }
}

#[test]
fn top_level_preparse_maps_long_help_by_personality() {
    let Some(help) = top_level_decision(&args(&["--help"]), false) else {
        panic!("expected --help to exit before clap");
    };
    let help = help.into_exit(false);
    assert_eq!(help.exit_code(), 0);
    assert!(!help.use_stderr());
    let tmux_help = top_level_decision(&args(&["--help"]), true)
        .expect("tmux long-help decision")
        .into_exit(true);
    assert_eq!(tmux_help.exit_code(), 1);
    assert!(tmux_help.use_stderr());
    assert_eq!(tmux_help.message(), help.message());
    let inventory = render_list_commands(None, None).expect("bare inventory renders");
    assert!(inventory.len() > 50);
    assert_eq!(
        help.message().split_once("\nCommands:\n").unwrap().1,
        inventory.join("\n")
    );
    let Some(long_option) = top_level_decision(&args(&["--not-a-tmux-flag", "-h"]), false) else {
        panic!("expected long top-level option to fail before clap");
    };
    let long_option = long_option.into_exit(false);
    assert_eq!(
        long_option.message(),
        "usage: rmux [-2CDlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]"
    );
    assert!(top_level_decision(&args(&["split-window", "-h"]), false).is_none());
}

#[test]
fn top_level_preparse_rejects_invalid_clusters_with_tmux_unknown_option() {
    for values in [&["-xh"][..], &["-Nxh"][..]] {
        let Some(error) = top_level_decision(&args(values), false) else {
            panic!("expected invalid cluster to fail before clap: {values:?}");
        };
        let error = error.into_exit(false);
        assert!(error.message().contains("unknown option -- x"));
    }
}

#[test]
fn top_level_preparse_selects_version_before_later_cluster_flags() {
    assert!(matches!(
        top_level_decision(&args(&["-Vh"]), false),
        Some(TopLevelDecision::Version)
    ));
    assert!(matches!(
        top_level_decision(&args(&["-lVh"]), false),
        Some(TopLevelDecision::Version)
    ));
}

#[test]
fn top_level_preparse_does_not_parse_option_values_as_flags() {
    assert!(top_level_decision(&args(&["-L", "-h", "list-sessions",]), false).is_none());
    assert!(top_level_decision(&args(&["-Lhas-h", "list-sessions"]), false).is_none());
}

#[test]
fn claude_dispatch_rejects_top_level_modes_it_cannot_honor() {
    for flag in ["-h", "-V"] {
        let exit = run(args(&["rmux", flag, "claude"]))
            .expect_err("top-level display option exits before extension dispatch");
        assert_eq!(exit.exit_code(), 0, "{flag}");
        assert!(
            !exit.message().contains("not supported by the managed"),
            "{flag} keeps top-level priority"
        );
    }

    for values in [
        &["rmux", "-D", "claude"][..],
        &["rmux", "-c", "echo ignored", "claude", "install-skill"][..],
        &["rmux", "-cecho ignored", "claude"][..],
        &["rmux", "-u", "-D", "-N", "claude"][..],
    ] {
        let error = run(args(values)).expect_err("managed launcher mode must be rejected");
        assert_eq!(error.exit_code(), 1, "{values:?}");
        assert!(
            error.message().contains("usage: rmux"),
            "the scanner must reject the mode before external dispatch: {values:?}"
        );
    }

    let no_start = run(args(&["rmux", "-N", "claude"]))
        .expect_err("-N must not silently start a private server");
    assert!(no_start.message().contains("-N is incompatible"));

    let control = run(args(&["rmux", "-C", "claude"]))
        .expect_err("-C must not silently launch an attached client");
    assert!(control.message().contains("-C control mode"));

    for (values, option) in [
        (&["rmux", "-2", "claude"][..], "-2"),
        (&["rmux", "-f", "config", "claude"][..], "-f"),
        (&["rmux", "-f", "--unknown", "claude"][..], "-f"),
        (&["rmux", "-l", "claude"][..], "-l"),
        (&["rmux", "-Ldemo", "claude"][..], "-L"),
        (&["rmux", "-S/path", "claude"][..], "-S"),
        (&["rmux", "-TRGB", "claude"][..], "-T"),
        (&["rmux", "-u", "claude"][..], "-u"),
        (&["rmux", "-v", "claude"][..], "-v"),
        (&["rmux", "-v", "-fconfig", "claude"][..], "-f"),
        (&["rmux", "-u", "-v", "-fconfig", "claude"][..], "-f"),
        (&["rmux", "-v", "-Ldemo", "claude"][..], "-L"),
        (&["rmux", "-L", "-f", "claude"][..], "-L"),
    ] {
        let error = run(args(values)).expect_err("ignored option must be rejected");
        assert!(
            error.message().contains(option),
            "diagnostic must name {option}: {error:?}"
        );
    }
}

#[test]
fn top_level_version_precedes_every_extension_dispatch() {
    for values in [
        &["rmux", "-V", "claude"][..],
        &["rmux", "-V", "diagnose", "--json"][..],
        &["rmux", "-V", "doctor", "tmux-dropin"][..],
        &["rmux", "-V", "capabilities", "--json"][..],
    ] {
        let exit = run(args(values)).expect_err("version exits before extension dispatch");
        assert_eq!(exit.exit_code(), 0, "values={values:?}");
        assert_eq!(
            exit.message(),
            format!("rmux {}", env!("CARGO_PKG_VERSION")),
            "values={values:?}"
        );
        assert!(!exit.use_stderr(), "values={values:?}");
    }
}

#[test]
fn command_too_long_parse_errors_probe_for_absent_server_first() {
    let error = clap::Error::raw(clap::error::ErrorKind::ValueValidation, "command too long");

    assert!(super::parse_failure_should_probe_server(
        &args(&["rmux", "-S", "/tmp/missing.sock", "aaaaaaaa"]),
        &error
    ));
}

#[test]
fn start_server_inventory_matches_supported_frozen_commands() {
    assert!(command_has_start_server_flag(&default_client_command()));
    assert!(command_has_start_server_flag(&Command::StartServer(
        StartServerArgs::default()
    )));
    assert!(command_has_start_server_flag(&Command::AttachSession(
        AttachSessionArgs {
            detach_other_clients: false,
            skip_environment_update: false,
            flags: Vec::new(),
            read_only: false,
            target: Some(parse_target_spec("alpha").expect("valid target")),
            kill_other_clients: false,
            working_directory: None,
        }
    )));
    assert!(!command_has_start_server_flag(&Command::KillServer));
    assert!(!command_has_start_server_flag(&Command::ListSessions(
        ListSessionsArgs {
            format: None,
            filter: None,
            json: false,
            sort_order: None,
            reversed: false,
        }
    )));
    assert!(!command_has_start_server_flag(&Command::NewWindow(
        NewWindowArgs {
            after: false,
            before: false,
            target: Some(parse_target_spec("alpha").expect("valid target")),
            name: None,
            detached: false,
            format: None,
            print_target: false,
            kill_existing: false,
            select_existing: false,
            start_directory: None,
            environment: Vec::new(),
            command: Vec::new(),
            queue_command: String::new(),
        }
    )));
    let web_create = parse_cli(["rmux", "web-share", "-t", "alpha"])
        .expect("web-share create parses")
        .command
        .expect("parsed command");
    assert!(command_has_start_server_flag(&web_create));
    for args in [
        &["rmux", "web-share", "-l"][..],
        &["rmux", "web-share", "-K", "abc12345"][..],
        &["rmux", "web-share", "disconnect", "abc12345"][..],
        &["rmux", "web-share", "-X"][..],
        &["rmux", "web-share", "--lookup", "abc12345"][..],
        &["rmux", "web-share", "--config"][..],
    ] {
        let command = parse_cli(args.iter().copied())
            .expect("web-share lifecycle command parses")
            .command
            .expect("parsed command");
        assert!(!command_has_start_server_flag(&command));
    }
}

#[test]
fn a_queue_starts_the_server_with_the_options_of_the_command_that_starts_it() {
    let parse = |values: &[&str]| parse_cli(values.iter().copied()).expect("queue parses");
    let starts = |values: &[&str]| server_starting_command(&parse(values)).is_some();

    assert!(starts(&[
        "rmux",
        "set-option",
        "-g",
        "status",
        "off",
        ";",
        "new-session",
        "-d"
    ]));
    assert!(starts(&["rmux", "new-session", "-d"]));
    assert!(!starts(&["rmux", "list-sessions", ";", "has-session"]));

    let queued_web_server = parse(&[
        "rmux",
        "list-commands",
        ";",
        "start-server",
        "--web-port",
        "4321",
    ]);
    assert_eq!(
        startup_config_from_cli(&queued_web_server).web_port,
        Some(4321)
    );
}

#[test]
fn explicit_config_files_disable_quiet_startup_loading() {
    let cli = parse_cli(["rmux", "-f", "one.conf", "-f", "two.conf"]).expect("cli parses");
    let startup = startup_config_from_cli(&cli);

    match startup.server {
        ServerStartupConfig::Files { files, quiet, .. } => {
            assert!(!quiet);
            assert_eq!(
                files,
                vec![PathBuf::from("one.conf"), PathBuf::from("two.conf")]
            );
        }
        ServerStartupConfig::Default { .. } => panic!("expected explicit config files"),
    }
}

#[test]
fn start_server_rejects_zero_web_port() {
    assert!(parse_cli(["rmux", "start-server", "--web-port", "0"]).is_err());
}

#[cfg(unix)]
#[test]
fn usable_shell_path_rejects_symlink_to_the_current_executable() {
    use std::os::unix::fs::symlink;

    let current_exe = std::env::current_exe().expect("current executable path");
    let dir = unique_test_dir("shell-symlink");
    fs::create_dir_all(&dir).expect("create temp dir");
    let link = dir.join("rmux-shell-link");
    symlink(&current_exe, &link).expect("create symlink");

    assert!(
        !usable_shell_path(&link),
        "shell startup must reject a differently named symlink to the current executable"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn usable_shell_path_rejects_hardlink_to_the_current_executable() {
    let running_exe = std::env::current_exe().expect("current executable path");
    let dir = unique_test_dir("shell-hardlink");
    fs::create_dir_all(&dir).expect("create temp dir");
    // Linking the running image changes its ctime during parallel identity checks.
    let current_exe = dir.join("current-executable");
    fs::copy(running_exe, &current_exe).expect("copy executable fixture");
    let link = dir.join("rmux-shell-hardlink");
    fs::hard_link(&current_exe, &link).expect("create hardlink");

    assert!(same_file_identity_for_paths(&current_exe, &link));
    assert!(
        !super::shell_startup::usable_shell_path_with_current_executable(&link, Some(&current_exe)),
        "shell startup must reject a differently named hardlink to the current executable"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn list_commands_tmux_format_variables_expand() {
    assert_eq!(
        render_list_commands_line(
            Some("#{command_list_name}|#{command_list_alias}|#{command_name}|#{command_alias}"),
            "attach-session",
            Some("attach"),
        ),
        "attach-session|attach||"
    );
}

#[test]
fn list_commands_default_output_matches_tmux_signature_shape() {
    assert_eq!(
        render_list_commands_line(None, "attach-session", Some("attach")),
        "attach-session (attach) [-dErx] [-c working-directory] [-f flags] [-t target-session]"
    );
    assert_eq!(
        render_list_commands_line(None, "kill-server", None),
        "kill-server "
    );
}

#[test]
fn list_commands_usage_variable_expands_tmux_signature_suffix() {
    assert_eq!(
        render_list_commands_line(
            Some("#{command_list_name}|#{command_list_usage}"),
            "attach-session",
            Some("attach"),
        ),
        "attach-session|[-dErx] [-c working-directory] [-f flags] [-t target-session]"
    );
}
