//! Public CLI dispatch for the RMUX binary.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[path = "cli/alias_fallback.rs"]
mod alias_fallback;
#[path = "cli/attach_transport.rs"]
mod attach_transport;
#[path = "cli/automation/mod.rs"]
mod automation;
#[path = "cli/buffer_commands.rs"]
mod buffer_commands;
#[path = "cli/capabilities.rs"]
mod capabilities;
#[path = "cli/capture_pane.rs"]
mod capture_pane;
#[path = "cli/claude_launcher.rs"]
mod claude_launcher;
#[path = "cli/claude_skill.rs"]
mod claude_skill;
#[path = "cli/client_commands.rs"]
mod client_commands;
#[path = "cli/command_inventory.rs"]
mod command_inventory;
#[path = "cli/command_runner.rs"]
mod command_runner;
#[path = "cli/config_commands.rs"]
mod config_commands;
#[path = "cli/control_mode_error.rs"]
mod control_mode_error;
#[path = "cli/diagnose.rs"]
mod diagnose;
#[path = "cli/dispatch.rs"]
mod dispatch;
#[path = "cli/error.rs"]
mod error;
#[path = "cli/format_print.rs"]
mod format_print;
#[path = "cli/json_output.rs"]
mod json_output;
#[path = "cli/key_commands.rs"]
mod key_commands;
#[path = "cli/message_commands.rs"]
mod message_commands;
#[path = "cli/pane_commands.rs"]
mod pane_commands;
#[path = "cli/scripting_contract.rs"]
mod scripting_contract;
#[path = "cli/server_commands.rs"]
mod server_commands;
#[path = "cli/session_commands.rs"]
mod session_commands;
#[path = "cli/shell_startup.rs"]
mod shell_startup;
#[path = "cli/startup.rs"]
mod startup;
#[path = "cli/target_resolution.rs"]
mod target_resolution;
#[path = "cli/terminal_size.rs"]
mod terminal_size;
#[path = "cli/terminal_theme.rs"]
mod terminal_theme;
#[path = "cli/tmux_dropin.rs"]
mod tmux_dropin;
#[path = "cli/top_level.rs"]
mod top_level;
#[path = "cli/web_commands.rs"]
mod web_commands;
#[path = "cli/web_share_display.rs"]
mod web_share_display;
#[path = "cli/window_commands.rs"]
mod window_commands;

use crate::cli_args::{parse, parse_with_runtime_command_groups, scan_top_level_command, Cli};
use crate::cli_response::{expect_command_output, expect_command_success};
use attach_transport::attach_with_connection;
use client_commands::{
    client_terminal_context_from_cli, optional_client_flags, run_control_mode, run_detach_client,
    run_list_clients, run_refresh_client, run_suspend_client, run_switch_client,
};
use client_commands::{run_switch_client_on_connection, validate_nested_attach_before_connect};
#[cfg(test)]
use command_inventory::render_list_commands_line;
pub(crate) use command_runner::{
    capture_target_action_needs_legacy_retry, cli_target_actions_enabled, run_command,
    run_command_resolved, run_payload_command, run_payload_command_resolved,
    target_action_needs_legacy_retry,
};
use command_runner::{
    finish_command_success, unexpected_response, write_command_output, write_lines_output,
};
use control_mode_error::parse_failure as control_mode_parse_failure;
#[cfg(test)]
use dispatch::default_client_command;
use dispatch::{command_has_start_server_flag, dispatch_command_queue};
pub(crate) use error::{ExitFailure, ExitMessageTermination};
use rmux_client::{
    connect, ensure_server_running_with_config, resolve_socket_path,
    resolve_tmux_compatible_socket_path, Connection,
};
use shell_startup::run_shell_startup;
#[cfg(test)]
use shell_startup::{same_file_identity_for_paths, usable_shell_path};
#[cfg(test)]
use startup::ServerStartupConfig;
use startup::{
    run_foreground_server, startup_config_from_cli, startup_config_from_top_level_scan,
    StartupOptions,
};
use target_resolution::{
    list_session_names, resolve_current_pane_target, resolve_current_session_target,
    resolve_existing_window_target_or_current, resolve_pane_target_or_current,
    resolve_pane_target_spec, resolve_session_listing_target, resolve_session_target_or_current,
    resolve_session_target_spec, resolve_split_window_target_spec, resolve_target_spec,
    resolve_window_index_target_or_current_session, resolve_window_target_or_current,
    resolve_window_target_spec, response_name_for_target,
};
use terminal_size::{build_terminal_size, current_terminal_size};
use top_level::{
    accept_compatibility_options, infer_client_utf8_from_env, scan_claude_top_level_invocation,
    top_level_parse_failure, top_level_version_output, top_level_version_requested,
    validate_claude_top_level_invocation, validate_top_level_invocation,
};

const TMUX_COMPAT_OVERRIDE_ENV: &str = "RMUX_INTERNAL_INVOKED_AS_TMUX";

pub(crate) fn run<I, T>(args: I) -> Result<i32, ExitFailure>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    if let Some(error) = top_level_parse_failure(args.get(1..).unwrap_or(&[])) {
        return Err(error);
    }
    if top_level_version_requested(args.get(1..).unwrap_or(&[])) {
        return Err(ExitFailure::new_stdout(
            0,
            top_level_version_output(invoked_as_tmux(&args)),
        ));
    }
    let claude_invocation = scan_claude_top_level_invocation(args.get(1..).unwrap_or(&[]));
    validate_claude_top_level_invocation(claude_invocation.as_ref())?;
    if let Some(invocation) = claude_launcher::parse_internal_runner(args.get(1..).unwrap_or(&[])) {
        return claude_launcher::run_internal_runner(invocation);
    }
    if let Some(invocation) = diagnose::parse_invocation(args.get(1..).unwrap_or(&[]))? {
        return diagnose::run(invocation);
    }
    if let Some(invocation) = tmux_dropin::parse_invocation(args.get(1..).unwrap_or(&[]))? {
        return tmux_dropin::run(invocation, args.first());
    }
    if let Some(claude_invocation) = claude_invocation {
        if let Some(invocation) = claude_skill::parse_invocation(claude_invocation.arguments())? {
            return claude_skill::run(invocation);
        }
        return claude_launcher::run(claude_launcher::ClaudeInvocation::new(
            claude_invocation.into_arguments(),
        ));
    }
    if let Some(invocation) = capabilities::parse_invocation(args.get(1..).unwrap_or(&[]))? {
        return capabilities::run(invocation);
    }
    let runtime_resolution =
        alias_fallback::runtime_command_resolution_for_invocation(&args, invoked_as_tmux(&args))?;
    if let Some(alias_fallback::RuntimeCommandResolution::LegacyServerDispatch(exit_code)) =
        runtime_resolution.as_ref()
    {
        return Ok(*exit_code);
    }
    let parsed_cli = parse_with_runtime_resolution(&args, runtime_resolution.as_ref());
    let mut startup_connection = None;
    let mut cli = match parsed_cli {
        Ok(cli) => cli,
        Err(error) if runtime_resolution.is_some() => {
            let control_mode = scan_top_level_command(args.get(1..).unwrap_or(&[]))
                .map_or(0, |scan| scan.control_mode);
            if control_mode != 0 {
                return Err(control_mode_parse_failure(error, control_mode));
            }
            return Err(ExitFailure::from_clap(error));
        }
        Err(error) if error.kind() == clap::error::ErrorKind::InvalidSubcommand => {
            match parse_cold_alias_queue_after_startup(&args, error)? {
                ColdAliasParseOutcome::NotApplicable(error) => {
                    return parse_failure_or_absent_server(&args, error);
                }
                ColdAliasParseOutcome::Parsed(cold_cli, connection) => {
                    startup_connection = Some(*connection);
                    *cold_cli
                }
                ColdAliasParseOutcome::Dispatched(exit_code) => return Ok(exit_code),
            }
        }
        Err(error) => return parse_failure_or_absent_server(&args, error),
    };
    cli.utf8 |= infer_client_utf8_from_env();
    let command_was_provided = cli.command.is_some();
    validate_top_level_invocation(&cli, command_was_provided)?;
    accept_compatibility_options(&cli);
    let mut startup_config = startup_config_from_cli(&cli);

    let socket_path = if invoked_as_tmux(&args) {
        resolve_tmux_compatible_socket_path(cli.socket_name(), cli.socket_path())
    } else {
        resolve_socket_path(cli.socket_name(), cli.socket_path())
    }
    .map_err(ExitFailure::from_client)?;

    if let Some(crate::cli_args::Command::AttachSession(args)) = cli.command.as_ref() {
        validate_nested_attach_before_connect(args, &socket_path)?;
    }

    // A start-server command may create the daemon that loads command-alias
    // definitions from `-f`. Resolve the original argv only after that config
    // is ready, while retaining the startup connection so the empty daemon
    // cannot exit between alias resolution and typed dispatch.
    if startup_connection.is_none()
        && runtime_resolution.is_none()
        && cli.control_mode == 0
        && !cli.no_fork
        && !cli.no_start_server
        && cli.shell_command.is_none()
        && cli
            .command
            .as_ref()
            .is_some_and(command_has_start_server_flag)
    {
        startup_connection = Some(
            ensure_server_running_with_config(&socket_path, startup_config.auto_start.clone())
                .map_err(ExitFailure::from_auto_start)
                .map_err(|error| error.with_socket_context(&socket_path))?,
        );
        let cold_resolution = alias_fallback::runtime_command_resolution_after_startup(
            &args,
            &socket_path,
            startup_connection
                .as_mut()
                .expect("startup connection was just stored"),
        )?;
        if let Some(alias_fallback::RuntimeCommandResolution::LegacyServerDispatch(exit_code)) =
            cold_resolution.as_ref()
        {
            return Ok(*exit_code);
        }
        if cold_resolution.is_some() {
            cli = parse_with_runtime_resolution(&args, cold_resolution.as_ref())
                .map_err(ExitFailure::from_clap)?;
            cli.utf8 |= infer_client_utf8_from_env();
            let command_was_provided = cli.command.is_some();
            validate_top_level_invocation(&cli, command_was_provided)?;
            accept_compatibility_options(&cli);
            startup_config = startup_config_from_cli(&cli);
        }
    }

    if let Some(shell_command) = cli.shell_command.as_deref() {
        return run_shell_startup(
            &socket_path,
            StartupOptions::new(cli.no_start_server, startup_config.auto_start.clone()),
            shell_command,
            cli.login_shell,
        )
        .map_err(|error| error.with_socket_context(&socket_path));
    }

    if cli.no_fork {
        return run_foreground_server(&socket_path, &startup_config);
    }

    let startup = StartupOptions::new(cli.no_start_server, startup_config.auto_start);
    if cli.control_mode != 0 {
        return run_control_mode(&cli, &socket_path, startup)
            .map_err(|error| error.with_socket_context(&socket_path));
    }
    let client_terminal = client_terminal_context_from_cli(&cli);
    let commands = cli.into_command_queue();
    let result = dispatch_command_queue(commands, &socket_path, startup, client_terminal)
        .map_err(|error| error.with_socket_context(&socket_path));
    drop(startup_connection);
    result
}

enum ColdAliasParseOutcome {
    NotApplicable(clap::Error),
    Parsed(Box<Cli>, Box<Connection>),
    Dispatched(i32),
}

fn parse_cold_alias_queue_after_startup(
    args: &[OsString],
    original_error: clap::Error,
) -> Result<ColdAliasParseOutcome, ExitFailure> {
    let Ok(scan) = scan_top_level_command(args.get(1..).unwrap_or(&[])) else {
        return Ok(ColdAliasParseOutcome::NotApplicable(original_error));
    };
    if scan.control_mode != 0
        || scan.no_fork
        || scan.no_start_server
        || scan.shell_command.is_some()
    {
        return Ok(ColdAliasParseOutcome::NotApplicable(original_error));
    }
    let Some(first_command) = alias_fallback::first_cold_start_command(args) else {
        return Ok(ColdAliasParseOutcome::NotApplicable(original_error));
    };
    if !command_has_start_server_flag(&first_command) {
        return Ok(ColdAliasParseOutcome::NotApplicable(original_error));
    }

    let socket_path = if invoked_as_tmux(args) {
        resolve_tmux_compatible_socket_path(
            scan.socket_name.as_deref(),
            scan.socket_path.as_deref().map(Path::new),
        )
    } else {
        resolve_socket_path(
            scan.socket_name.as_deref(),
            scan.socket_path.as_deref().map(Path::new),
        )
    }
    .map_err(ExitFailure::from_client)?;
    let startup_config = startup_config_from_top_level_scan(&scan, &first_command);
    let mut connection = ensure_server_running_with_config(&socket_path, startup_config.auto_start)
        .map_err(ExitFailure::from_auto_start)
        .map_err(|error| error.with_socket_context(&socket_path))?;
    let resolution = alias_fallback::runtime_command_resolution_after_startup(
        args,
        &socket_path,
        &mut connection,
    )?;
    let Some(resolution) = resolution else {
        return Err(ExitFailure::from_clap(original_error));
    };
    if let alias_fallback::RuntimeCommandResolution::LegacyServerDispatch(exit_code) = resolution {
        return Ok(ColdAliasParseOutcome::Dispatched(exit_code));
    }
    let cli =
        parse_with_runtime_resolution(args, Some(&resolution)).map_err(ExitFailure::from_clap)?;
    Ok(ColdAliasParseOutcome::Parsed(
        Box::new(cli),
        Box::new(connection),
    ))
}

fn parse_with_runtime_resolution(
    args: &[OsString],
    resolution: Option<&alias_fallback::RuntimeCommandResolution>,
) -> Result<Cli, clap::Error> {
    match resolution {
        Some(alias_fallback::RuntimeCommandResolution::Canonical(groups)) => {
            parse_with_runtime_command_groups(args.to_vec(), groups)
        }
        Some(alias_fallback::RuntimeCommandResolution::LegacyDirect) | None => parse(args.to_vec()),
        Some(alias_fallback::RuntimeCommandResolution::LegacyServerDispatch(_)) => unreachable!(),
    }
}

fn invoked_as_tmux(args: &[OsString]) -> bool {
    invoked_as_tmux_argv0(args) || internal_tmux_compat_override()
}

fn invoked_as_tmux_argv0(args: &[OsString]) -> bool {
    args.first()
        .and_then(|arg| std::path::Path::new(arg).file_stem())
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("tmux"))
}

fn internal_tmux_compat_override() -> bool {
    env::var_os(TMUX_COMPAT_OVERRIDE_ENV)
        .as_deref()
        .is_some_and(|value| value == "1")
}

fn parse_failure_or_absent_server(
    args: &[OsString],
    error: clap::Error,
) -> Result<i32, ExitFailure> {
    if !parse_failure_should_probe_server(args, &error) {
        return Err(ExitFailure::from_clap(error));
    }

    let Some((socket_name, socket_path)) = recover_socket_selection(args.get(1..).unwrap_or(&[]))
    else {
        return Err(ExitFailure::from_clap(error));
    };
    let resolved = if invoked_as_tmux(args) {
        resolve_tmux_compatible_socket_path(socket_name.as_deref(), socket_path.as_deref())
    } else {
        resolve_socket_path(socket_name.as_deref(), socket_path.as_deref())
    }
    .map_err(ExitFailure::from_client)?;

    match connect(&resolved) {
        Ok(mut connection) if error.kind() == clap::error::ErrorKind::InvalidSubcommand => {
            alias_fallback::run_unknown_command_through_server_aliases(
                args,
                &resolved,
                &mut connection,
            )
            .map_err(|error| error.with_socket_context(&resolved))
        }
        Ok(_) => Err(ExitFailure::from_clap(error)),
        Err(connect_error) => Err(ExitFailure::from_client_connect(&resolved, connect_error)),
    }
}

fn parse_failure_should_probe_server(args: &[OsString], error: &clap::Error) -> bool {
    if matches!(
        error.kind(),
        clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
    ) {
        return false;
    }

    if error.kind() == clap::error::ErrorKind::InvalidSubcommand {
        return first_command_token(args.get(1..).unwrap_or(&[])).is_some();
    }

    if error.kind() == clap::error::ErrorKind::ValueValidation {
        let message = error.to_string();
        if message.contains("command too long") {
            return first_command_token(args.get(1..).unwrap_or(&[])).is_some();
        }
        if message.contains("expects an argument") {
            return first_command_token(args.get(1..).unwrap_or(&[]))
                .is_some_and(|command| matches!(command.as_str(), "new-session" | "new"));
        }
        return first_command_token(args.get(1..).unwrap_or(&[]))
            .is_some_and(|command| matches!(command.as_str(), "resize-pane" | "resizep"));
    }

    first_command_token(args.get(1..).unwrap_or(&[])).is_some_and(|command| {
        matches!(
            command.as_str(),
            "new-session" | "new" | "resize-pane" | "resizep"
        )
    })
}

fn recover_socket_selection(arguments: &[OsString]) -> Option<(Option<OsString>, Option<PathBuf>)> {
    let mut socket_name = None;
    let mut socket_path = None;
    let mut index = 0;

    while index < arguments.len() {
        let argument = arguments[index].to_str()?;
        if argument == "--" {
            break;
        }
        if !argument.starts_with('-') || argument == "-" {
            break;
        }

        match argument {
            "-L" => {
                index += 1;
                socket_name = arguments.get(index).cloned();
            }
            "-S" => {
                index += 1;
                socket_path = arguments.get(index).cloned().map(PathBuf::from);
            }
            "-c" | "-f" | "-T" => {
                index += 1;
            }
            value if value.starts_with("-L") && value.len() > 2 => {
                socket_name = Some(OsString::from(&value[2..]));
            }
            value if value.starts_with("-S") && value.len() > 2 => {
                socket_path = Some(PathBuf::from(&value[2..]));
            }
            _ => {}
        }
        index += 1;
    }

    Some((socket_name, socket_path))
}

fn first_command_token(arguments: &[OsString]) -> Option<String> {
    let mut index = 0;

    while index < arguments.len() {
        let argument = arguments[index].to_str()?;
        if argument == "--" {
            return arguments.get(index + 1)?.to_str().map(str::to_owned);
        }
        if !argument.starts_with('-') || argument == "-" {
            return Some(argument.to_owned());
        }

        if matches!(argument, "-c" | "-f" | "-L" | "-S" | "-T") {
            index += 1;
        }
        index += 1;
    }

    None
}

fn connect_with_startserver(
    socket_path: &Path,
    startup: StartupOptions,
) -> Result<Connection, ExitFailure> {
    if startup.no_start_server {
        connect(socket_path).map_err(|error| ExitFailure::from_client_connect(socket_path, error))
    } else {
        ensure_server_running_with_config(socket_path, startup.config)
            .map_err(ExitFailure::from_auto_start)
    }
}

fn shell_command_text(command: Vec<String>) -> String {
    if command.len() == 1 {
        return command.into_iter().next().expect("single shell token");
    }

    command
        .into_iter()
        .map(shell_command_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_command_token(token: String) -> String {
    format!("'{}'", token.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::{
        command_has_start_server_flag, default_client_command, render_list_commands_line, run,
        same_file_identity_for_paths, startup_config_from_cli, top_level_parse_failure,
        usable_shell_path, ServerStartupConfig,
    };
    use crate::cli_args::{
        parse as parse_cli, parse_target_spec, AttachSessionArgs, Command, ListSessionsArgs,
        NewWindowArgs, StartServerArgs,
    };
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static UNIQUE_TEST_ID: AtomicUsize = AtomicUsize::new(0);

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[cfg(unix)]
    fn unique_test_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "rmux-cli-{label}-{}-{}",
            std::process::id(),
            UNIQUE_TEST_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn top_level_preparse_accepts_tmux_short_help() {
        for values in [&["-h"][..], &["-Nh"][..], &["-hV"][..]] {
            let error = top_level_parse_failure(&args(values)).expect("expected short help exit");
            assert_eq!(error.exit_code(), 0);
            assert!(!error.use_stderr());
            assert_eq!(
                error.message(),
                "usage: rmux [-2CDhlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]"
            );
        }
    }

    #[test]
    fn top_level_preparse_rejects_long_options_with_tmux_usage() {
        assert_eq!(
            top_level_parse_failure(&args(&["--help"]))
                .expect("expected --help to fail before clap")
                .message(),
            "usage: rmux [-2CDlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]\n\nRMUX extensions:\n  capabilities [--human|--json]\n  claude [install-skill|claude-args...]\n  diagnose [--human|--json]\n  doctor tmux-dropin\n  setup tmux-shim\n  wait-pane [flags]\n  pane-snapshot [flags]\n  stream-pane [--raw|--lines]\n  collect-pane-output --until-pane-exit --max-bytes bytes\n  locator|expect-pane [flags]\n  find-panes|find-sessions [flags]\n  broadcast-keys -t target... -- key ...\n  with-session session-name -- command ...\n  web-share [flags]\n  web-share list|lookup|stop|disconnect|off|config\n\nUse `rmux list-commands` for the tmux-compatible command surface."
        );
        assert_eq!(
            top_level_parse_failure(&args(&["--not-a-tmux-flag", "-h"]))
                .expect("expected long top-level option to fail before clap")
                .message(),
            "usage: rmux [-2CDlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]"
        );
        assert!(top_level_parse_failure(&args(&["split-window", "-h"])).is_none());
    }

    #[test]
    fn top_level_preparse_rejects_invalid_clusters_with_tmux_unknown_option() {
        assert!(top_level_parse_failure(&args(&["-xh"]))
            .expect("expected invalid cluster to fail before clap")
            .message()
            .contains("unknown option -- x"));
        assert!(top_level_parse_failure(&args(&["-Nxh"]))
            .expect("expected invalid cluster to fail before clap")
            .message()
            .contains("unknown option -- x"));
    }

    #[test]
    fn top_level_preparse_leaves_version_first_clusters_for_clap() {
        assert!(top_level_parse_failure(&args(&["-Vh"])).is_none());
        assert!(top_level_parse_failure(&args(&["-lVh"])).is_none());
    }

    #[test]
    fn top_level_preparse_does_not_parse_option_values_as_flags() {
        assert!(top_level_parse_failure(&args(&["-L", "-h", "list-sessions",])).is_none());
        assert!(top_level_parse_failure(&args(&["-Lhas-h", "list-sessions"])).is_none());
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
        let current_exe = std::env::current_exe().expect("current executable path");
        let link = current_exe
            .parent()
            .expect("current executable has a parent directory")
            .join(format!(
                "rmux-shell-hardlink-{}-{}",
                std::process::id(),
                UNIQUE_TEST_ID.fetch_add(1, Ordering::Relaxed)
            ));
        let _ = fs::remove_file(&link);
        fs::hard_link(&current_exe, &link).expect("create hardlink");

        assert!(same_file_identity_for_paths(&current_exe, &link));
        assert!(
            !usable_shell_path(&link),
            "shell startup must reject a differently named hardlink to the current executable"
        );

        let _ = fs::remove_file(&link);
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
}
