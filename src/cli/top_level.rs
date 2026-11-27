use std::ffi::OsString;

use crate::cli_args::{scan_top_level_command, Cli};
use crate::os_string::os_str_bytes;
use rmux_core::command_inventory::{
    render_list_commands, render_list_commands_line, RMUX_EXTENSION_COMMANDS,
};

use super::{diagnose::DIAGNOSE_HELP, ExitFailure};

pub(super) const RMUX_USAGE: &str = "usage: rmux [-2CDhlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]";
const RMUX_LONG_OPTION_USAGE: &str = "usage: rmux [-2CDlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]";

fn rmux_long_help() -> String {
    let mut help = format!("{RMUX_USAGE}\n\nRMUX extensions:\n");
    for entry in RMUX_EXTENSION_COMMANDS {
        help.push_str("  ");
        help.push_str(&render_list_commands_line(None, entry.name, entry.alias));
        help.push('\n');
    }
    // Diagnose is CLI-only; publishing it in the parser inventory would change
    // server dispatch and list-commands. Share its handler's help here instead.
    help.push_str("  ");
    help.push_str(DIAGNOSE_HELP);
    help.push_str("\n\nCommands:\n");
    let inventory = render_list_commands(None, None).expect("bare command inventory is infallible");
    help.push_str(&inventory.join("\n"));
    help
}

pub(super) use crate::cli_top_level::{top_level_decision, TopLevelDecision};

impl TopLevelDecision {
    pub(super) fn into_exit(self, invoked_as_tmux: bool) -> ExitFailure {
        match self {
            Self::Version => ExitFailure::new_stdout(0, top_level_version_output(invoked_as_tmux)),
            Self::Usage => ExitFailure::new_stdout(0, RMUX_USAGE),
            Self::LongHelp if invoked_as_tmux => ExitFailure::new(1, rmux_long_help()),
            Self::LongHelp => ExitFailure::new_stdout(0, rmux_long_help()),
            Self::InvalidLongOption => ExitFailure::new(1, RMUX_LONG_OPTION_USAGE),
            Self::InvalidShortOption(flag) => ExitFailure::new(
                1,
                format!("rmux: unknown option -- {}\n{RMUX_USAGE}", char::from(flag)),
            ),
            Self::MissingArgument(flag) => ExitFailure::new(
                1,
                format!(
                    "rmux: option requires an argument -- {}\n{RMUX_USAGE}",
                    char::from(flag)
                ),
            ),
        }
    }
}

pub(super) fn top_level_version_output(invoked_as_tmux: bool) -> String {
    if invoked_as_tmux {
        // TPM and most tmux plugin managers parse only the leading product token
        // and version. The shim keeps the rest of the CLI on the normal rmux path.
        return format!("tmux {}", tmux_compatible_version());
    }
    format!("rmux {}", env!("CARGO_PKG_VERSION"))
}

fn tmux_compatible_version() -> &'static str {
    "3.4"
}

pub(super) fn infer_client_utf8_from_env() -> bool {
    if std::env::var_os("RMUX").is_some() {
        return true;
    }

    first_non_empty_env_value(&["LC_ALL", "LC_CTYPE", "LANG"])
        .is_some_and(|value| env_value_contains_utf8(&value))
}

fn first_non_empty_env_value(names: &[&str]) -> Option<std::ffi::OsString> {
    names
        .iter()
        .find_map(|name| std::env::var_os(name).filter(|value| !value.is_empty()))
}

fn env_value_contains_utf8(value: &std::ffi::OsStr) -> bool {
    let lower = value.to_string_lossy().to_ascii_lowercase();
    lower.contains("utf-8") || lower.contains("utf8")
}

pub(super) fn validate_top_level_invocation(
    cli: &Cli,
    command_was_provided: bool,
) -> Result<(), ExitFailure> {
    if cli.shell_command.is_some() && command_was_provided {
        return Err(ExitFailure::new(1, RMUX_USAGE));
    }
    if cli.no_fork && command_was_provided {
        return Err(ExitFailure::new(1, RMUX_USAGE));
    }

    Ok(())
}

/// Applies the top-level execution-mode rules before the public `claude`
/// extension is dispatched outside clap. Without this preflight, `-c`, `-D`,
/// `-N`, and `-C` would be parsed as a harmless prefix and then silently
/// discarded by the managed launcher.
pub(super) fn validate_claude_top_level_invocation(
    invocation: Option<&ClaudeTopLevelInvocation>,
) -> Result<(), ExitFailure> {
    let Some(invocation) = invocation else {
        return Ok(());
    };

    if invocation.shell_command || invocation.no_fork {
        return Err(ExitFailure::new(1, RMUX_USAGE));
    }
    if invocation.no_start_server {
        return Err(ExitFailure::new(
            1,
            "rmux claude: -N is incompatible with the managed private server",
        ));
    }
    if invocation.control_mode {
        return Err(ExitFailure::new(
            1,
            "rmux claude: -C control mode is not supported by the managed launcher",
        ));
    }
    if let Some(option) = invocation.unsupported_option {
        return Err(ExitFailure::new(
            1,
            format!(
                "rmux claude: top-level option {option} is not supported by the managed private launcher; use `rmux claude [claude-args...]`"
            ),
        ));
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClaudeTopLevelInvocation {
    arguments: Vec<OsString>,
    control_mode: bool,
    no_fork: bool,
    no_start_server: bool,
    shell_command: bool,
    unsupported_option: Option<&'static str>,
}

impl ClaudeTopLevelInvocation {
    pub(super) fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    pub(super) fn into_arguments(self) -> Vec<OsString> {
        self.arguments
    }
}

/// Scans a potential Claude extension through the same clap model used by the
/// main CLI. A malformed prefix deliberately returns `None`: the ordinary
/// parse path then surfaces the clap error without starting Claude.
pub(super) fn scan_claude_top_level_invocation(
    arguments: &[OsString],
) -> Option<ClaudeTopLevelInvocation> {
    let scan = scan_top_level_command(arguments).ok()?;
    let mut command = scan.command.into_iter();
    let first = command.next()?;
    if os_str_bytes(&first) != b"claude" {
        return None;
    }
    let unsupported_option = scan
        .assume_256_colors
        .then_some("-2")
        .or_else(|| (!scan.config_files.is_empty()).then_some("-f"))
        .or_else(|| scan.login_shell.then_some("-l"))
        .or_else(|| scan.socket_name.is_some().then_some("-L"))
        .or_else(|| scan.socket_path.is_some().then_some("-S"))
        .or_else(|| (!scan.terminal_features.is_empty()).then_some("-T"))
        .or_else(|| scan.utf8.then_some("-u"))
        .or_else(|| (scan.verbose != 0).then_some("-v"));
    Some(ClaudeTopLevelInvocation {
        arguments: command.collect(),
        control_mode: scan.control_mode != 0,
        no_fork: scan.no_fork,
        no_start_server: scan.no_start_server,
        shell_command: scan.shell_command.is_some(),
        unsupported_option,
    })
}

pub(super) fn accept_compatibility_options(cli: &Cli) {
    let _ = (
        cli.assume_256_colors,
        cli.login_shell,
        cli.utf8,
        cli.verbose,
        cli.config_file_selection(),
        cli.terminal_features(),
    );
}

#[cfg(test)]
mod top_level_option_tests {
    use super::{
        scan_claude_top_level_invocation, top_level_decision, validate_claude_top_level_invocation,
        TopLevelDecision,
    };
    use crate::cli_args::scan_top_level_command;
    use std::ffi::OsString;

    fn requests_version(args: &[&str]) -> bool {
        let args = args.iter().map(OsString::from).collect::<Vec<_>>();
        matches!(
            top_level_decision(&args, false),
            Some(TopLevelDecision::Version)
        )
    }

    #[test]
    fn version_scan_stops_at_attached_short_option_values() {
        for argument in [
            "-cechoV",
            "-f/Volumes/rmux.conf",
            "-LValue",
            "-S/Volumes/rmux.sock",
            "-TRGBV",
            "-vS/Volumes/rmux.sock",
        ] {
            assert!(
                !requests_version(&[argument, "list-sessions"]),
                "{argument} must treat V as part of the option value"
            );
        }
    }

    #[test]
    fn version_scan_skips_separate_short_option_values() {
        for option in ["-c", "-f", "-L", "-S", "-T"] {
            assert!(!requests_version(&[
                option,
                "-Value-containing-V",
                "list-sessions"
            ]));
        }
    }

    #[test]
    fn version_scan_still_accepts_real_clustered_version_flags() {
        assert!(requests_version(&["-V"]));
        assert!(requests_version(&["-vV"]));
        assert!(requests_version(&["-CvV"]));
    }

    #[test]
    fn decision_walk_stops_at_the_first_decisive_option() {
        for arguments in [
            &["-V", "-x"][..],
            &["-V", "-h"][..],
            &["-V", "-x", "-x"][..],
            &["-u", "-V", "-x"][..],
            &["-V", "--help"][..],
            &["-V", "--version"][..],
            &["-Vh"][..],
            &["-uVx"][..],
        ] {
            assert!(requests_version(arguments), "arguments={arguments:?}");
        }

        for arguments in [&["-h", "-V"][..], &["-hV"][..]] {
            let arguments = arguments.iter().map(OsString::from).collect::<Vec<_>>();
            assert!(matches!(
                top_level_decision(&arguments, false),
                Some(TopLevelDecision::Usage)
            ));
        }

        for arguments in [&["-x", "-V"][..], &["-uxV"][..], &["-xuV"][..]] {
            let arguments = arguments.iter().map(OsString::from).collect::<Vec<_>>();
            let Some(error) = top_level_decision(&arguments, false) else {
                panic!("expected unknown-option decision for {arguments:?}");
            };
            let error = error.into_exit(false);
            assert_eq!(error.exit_code(), 1);
            assert_eq!(
                error.message(),
                "rmux: unknown option -- x\nusage: rmux [-2CDhlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]"
            );
        }
    }

    #[test]
    fn decision_walk_consumes_cluster_tail_values() {
        for arguments in [
            &["-vL", "-V"][..],
            &["-uL", "-V"][..],
            &["-vf", "-V"][..],
            &["-2T", "-V"][..],
            &["-vS", "-V"][..],
            &["-vc", "-V"][..],
        ] {
            let arguments = arguments.iter().map(OsString::from).collect::<Vec<_>>();
            assert!(
                top_level_decision(&arguments, false).is_none(),
                "the final option must consume its separate value: {arguments:?}"
            );
        }

        assert!(requests_version(&["-vL", "sockA", "-V"]));
    }

    #[test]
    fn decision_walk_preserves_scan_boundaries_and_long_option_surfaces() {
        for arguments in [
            &["--", "-V"][..],
            &["list-sessions", "-V"][..],
            &["-", "-V"][..],
            &["", "-V"][..],
        ] {
            let arguments = arguments.iter().map(OsString::from).collect::<Vec<_>>();
            assert!(top_level_decision(&arguments, false).is_none());
        }

        for (option, expected) in [
            ("--help", TopLevelDecision::LongHelp),
            ("--vesion", TopLevelDecision::InvalidLongOption),
        ] {
            let arguments = [OsString::from(option)];
            for tmux in [false, true] {
                assert_eq!(
                    top_level_decision(&arguments, tmux).as_ref(),
                    Some(&expected)
                );
            }
        }
    }

    #[test]
    fn claude_scan_uses_clap_cluster_and_value_boundaries() {
        for arguments in [
            &["-f", "config", "claude"][..],
            &["-fconfig", "claude"][..],
            &["-v", "-fconfig", "claude"][..],
            &["-Ldemo", "claude"][..],
            &["-u", "-v", "-fconfig", "claude", "--flag"][..],
            // Clap accepts an unrecognized hyphenated token as -f's value,
            // while recognized options such as -L and --help retain priority.
            &["-f", "--unknown", "claude"][..],
            // Unlike -f, -L explicitly accepts a hyphen-prefixed value.
            &["-L", "-f", "claude"][..],
        ] {
            let arguments = arguments.iter().map(OsString::from).collect::<Vec<_>>();
            assert!(
                scan_claude_top_level_invocation(&arguments).is_some(),
                "valid clap prefix must find claude: {arguments:?}"
            );
        }

        for arguments in [
            &["-f", "-D", "claude"][..],
            &["-f", "-Ldemo", "claude"][..],
            &["-Lfixed", "-f", "-Ldemo", "claude"][..],
            &["-f", "--", "claude"][..],
            &["-f", "--help", "claude"][..],
            &["-x", "claude"][..],
        ] {
            let arguments = arguments.iter().map(OsString::from).collect::<Vec<_>>();
            assert!(
                scan_claude_top_level_invocation(&arguments).is_none(),
                "invalid clap prefix must not dispatch claude: {arguments:?}"
            );
        }

        for arguments in [
            &["-vfconfig", "claude"][..],
            &["-vLdemo", "claude"][..],
            &["-uvfconfig", "claude"][..],
            &["-vcfoo", "claude"][..],
        ] {
            let arguments = arguments.iter().map(OsString::from).collect::<Vec<_>>();
            let scan = scan_top_level_command(&arguments)
                .expect("the public parser preserves the compact token as command input");
            assert_eq!(
                &scan.command, &arguments,
                "compact flag-leading token stays in the public command tail"
            );
            assert!(
                scan_claude_top_level_invocation(&arguments).is_none(),
                "the extension scanner must not reinterpret command-tail clusters"
            );
        }
    }

    #[test]
    fn claude_scan_preserves_execution_mode_validation_with_other_flags() {
        for arguments in [
            &["-v", "-D", "claude"][..],
            &["-v", "-N", "claude"][..],
            &["-v", "-C", "claude"][..],
            &["-cfoo", "claude"][..],
        ] {
            let arguments = arguments.iter().map(OsString::from).collect::<Vec<_>>();
            let invocation = scan_claude_top_level_invocation(&arguments)
                .expect("valid clap prefix finds claude");
            assert!(
                validate_claude_top_level_invocation(Some(&invocation)).is_err(),
                "incompatible mode must be rejected: {arguments:?}"
            );
        }
    }

    #[test]
    fn claude_rejects_every_top_level_option_the_private_launcher_cannot_honor() {
        for (arguments, option) in [
            (&["-2", "claude"][..], "-2"),
            (&["-f", "config", "claude"][..], "-f"),
            (&["-f", "--unknown", "claude"][..], "-f"),
            (&["-l", "claude"][..], "-l"),
            (&["-Ldemo", "claude"][..], "-L"),
            (&["-S/path", "claude"][..], "-S"),
            (&["-TRGB", "claude"][..], "-T"),
            (&["-u", "claude"][..], "-u"),
            (&["-v", "claude"][..], "-v"),
            (&["-v", "-fconfig", "claude"][..], "-f"),
            (&["-u", "-v", "-fconfig", "claude"][..], "-f"),
            (&["-v", "-Ldemo", "claude"][..], "-L"),
            (&["-L", "-f", "claude"][..], "-L"),
        ] {
            let arguments = arguments.iter().map(OsString::from).collect::<Vec<_>>();
            let invocation = scan_claude_top_level_invocation(&arguments)
                .expect("syntactically valid prefix finds claude");
            let error = validate_claude_top_level_invocation(Some(&invocation))
                .expect_err("unhonored top-level option must be rejected");
            assert!(
                error.message().contains(option),
                "diagnostic must name {option}: {error:?}"
            );
        }
    }
}

#[cfg(test)]
mod utf8_env_tests {
    use super::{env_value_contains_utf8, infer_client_utf8_from_env};
    use std::ffi::{OsStr, OsString};
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        name: &'static str,
        value: Option<OsString>,
    }

    impl EnvVarGuard {
        fn capture(name: &'static str) -> Self {
            Self {
                name,
                value: std::env::var_os(name),
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.value.as_ref() {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    #[test]
    fn env_utf8_detection_matches_tmux_substring_rules() {
        assert!(env_value_contains_utf8(OsStr::new("en_US.UTF-8")));
        assert!(env_value_contains_utf8(OsStr::new("C.UTF8")));
        assert!(env_value_contains_utf8(OsStr::new("x.UTF-8@y")));
        assert!(!env_value_contains_utf8(OsStr::new("C")));
        assert!(!env_value_contains_utf8(OsStr::new("latin1")));
    }

    #[test]
    fn client_utf8_detection_skips_empty_locale_variables_like_tmux() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let _tmux = EnvVarGuard::capture("RMUX");
        let _lc_all = EnvVarGuard::capture("LC_ALL");
        let _lc_ctype = EnvVarGuard::capture("LC_CTYPE");
        let _lang = EnvVarGuard::capture("LANG");

        std::env::remove_var("RMUX");
        std::env::set_var("LC_ALL", "");
        std::env::set_var("LC_CTYPE", "");
        std::env::set_var("LANG", "en_US.UTF-8");

        assert!(infer_client_utf8_from_env());
    }

    #[test]
    fn rmux_environment_forces_client_utf8_even_without_utf8_locale() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let _tmux = EnvVarGuard::capture("RMUX");
        let _lc_all = EnvVarGuard::capture("LC_ALL");
        let _lc_ctype = EnvVarGuard::capture("LC_CTYPE");
        let _lang = EnvVarGuard::capture("LANG");

        std::env::set_var("RMUX", "/tmp/rmux-1000/default,123,0");
        std::env::set_var("LC_ALL", "C");
        std::env::remove_var("LC_CTYPE");
        std::env::remove_var("LANG");

        assert!(infer_client_utf8_from_env());
    }

    #[test]
    fn ascii_locale_without_rmux_does_not_enable_client_utf8() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let _tmux = EnvVarGuard::capture("RMUX");
        let _lc_all = EnvVarGuard::capture("LC_ALL");
        let _lc_ctype = EnvVarGuard::capture("LC_CTYPE");
        let _lang = EnvVarGuard::capture("LANG");

        std::env::remove_var("RMUX");
        std::env::set_var("LC_ALL", "C");
        std::env::remove_var("LC_CTYPE");
        std::env::remove_var("LANG");

        assert!(!infer_client_utf8_from_env());
    }
}
