use std::ffi::OsString;

use crate::cli_args::{scan_top_level_command, Cli};
use crate::os_string::os_str_bytes;

use super::ExitFailure;

const RMUX_USAGE: &str = "usage: rmux [-2CDhlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]";
const RMUX_LONG_OPTION_USAGE: &str = "usage: rmux [-2CDlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]";
const RMUX_LONG_HELP: &str = "usage: rmux [-2CDlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]\n\nRMUX extensions:\n  capabilities [--human|--json]\n  claude [install-skill|claude-args...]\n  diagnose [--human|--json]\n  doctor tmux-dropin\n  setup tmux-shim\n  wait-pane [flags]\n  pane-snapshot [flags]\n  stream-pane [--raw|--lines]\n  collect-pane-output --until-pane-exit --max-bytes bytes\n  locator|expect-pane [flags]\n  find-panes|find-sessions [flags]\n  broadcast-keys -t target... -- key ...\n  with-session session-name -- command ...\n  web-share [flags]\n  web-share list|lookup|stop|disconnect|off|config\n\nUse `rmux list-commands` for the tmux-compatible command surface.";

pub(super) fn top_level_parse_failure(args: &[OsString]) -> Option<ExitFailure> {
    let mut index = 0;

    while let Some(argument) = args.get(index) {
        let bytes = os_str_bytes(argument);
        if bytes == b"--" {
            return None;
        }
        if !bytes.starts_with(b"-") || bytes == b"-" {
            return None;
        }
        if bytes == b"--help" {
            return Some(ExitFailure::new(1, RMUX_LONG_HELP));
        }
        if bytes.starts_with(b"--") {
            return Some(ExitFailure::new(1, RMUX_LONG_OPTION_USAGE));
        }
        if short_option_cluster_requests_usage(&bytes) {
            return Some(ExitFailure::new_stdout(0, RMUX_USAGE));
        }
        if let Some(flag) = invalid_short_option_in_cluster(&bytes) {
            let flag = char::from(flag);
            return Some(ExitFailure::new(
                1,
                format!("rmux: unknown option -- {flag}\n{RMUX_USAGE}"),
            ));
        }
        if short_option_consumes_next_argument(&bytes) {
            index += 1;
        }

        index += 1;
    }

    None
}

pub(super) fn top_level_version_requested(args: &[OsString]) -> bool {
    let mut index = 0;

    while let Some(argument) = args.get(index) {
        let bytes = os_str_bytes(argument);
        if bytes == b"--" || !bytes.starts_with(b"-") || bytes == b"-" {
            return false;
        }
        if !bytes.starts_with(b"--") && short_option_cluster_requests_version(&bytes) {
            return true;
        }
        if short_option_consumes_next_argument(&bytes) {
            index += 1;
        }

        index += 1;
    }

    false
}

fn short_option_cluster_requests_version(bytes: &[u8]) -> bool {
    for flag in bytes.iter().copied().skip(1) {
        if flag == b'V' {
            return true;
        }
        if short_option_takes_argument(flag) || !short_option_takes_no_argument(flag) {
            return false;
        }
    }

    false
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

fn invalid_short_option_in_cluster(bytes: &[u8]) -> Option<u8> {
    for flag in bytes.iter().copied().skip(1) {
        if flag == b'V' {
            return None;
        }
        if short_option_takes_argument(flag) {
            return None;
        }
        if !short_option_takes_no_argument(flag) {
            return Some(flag);
        }
    }

    None
}

fn short_option_cluster_requests_usage(bytes: &[u8]) -> bool {
    for flag in bytes.iter().copied().skip(1) {
        if flag == b'h' {
            return true;
        }
        if flag == b'V' || short_option_takes_argument(flag) {
            return false;
        }
        if !short_option_takes_no_argument(flag) {
            return false;
        }
    }

    false
}

fn short_option_consumes_next_argument(bytes: &[u8]) -> bool {
    bytes.len() == 2 && short_option_takes_argument(bytes[1])
}

fn short_option_takes_argument(flag: u8) -> bool {
    matches!(flag, b'c' | b'f' | b'L' | b'S' | b'T')
}

fn short_option_takes_no_argument(flag: u8) -> bool {
    matches!(flag, b'2' | b'C' | b'D' | b'l' | b'N' | b'u' | b'v')
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
        scan_claude_top_level_invocation, top_level_version_requested,
        validate_claude_top_level_invocation,
    };
    use crate::cli_args::scan_top_level_command;
    use std::ffi::OsString;

    fn requests_version(args: &[&str]) -> bool {
        let args = args.iter().map(OsString::from).collect::<Vec<_>>();
        top_level_version_requested(&args)
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
