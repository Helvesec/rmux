use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;

use crate::cli_args::Cli;

use super::ExitFailure;

const RMUX_USAGE: &str = "usage: rmux [-2CDhlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]";
const RMUX_LONG_OPTION_USAGE: &str = "usage: rmux [-2CDlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]";

pub(super) fn top_level_parse_failure(args: &[OsString]) -> Option<ExitFailure> {
    let mut index = 0;

    while let Some(argument) = args.get(index) {
        let bytes = argument.as_bytes();
        if bytes == b"--" {
            return None;
        }
        if !bytes.starts_with(b"-") || bytes == b"-" {
            return None;
        }
        if bytes.starts_with(b"--") {
            return Some(ExitFailure::new(1, RMUX_LONG_OPTION_USAGE));
        }
        if short_option_cluster_requests_usage(bytes) {
            return Some(ExitFailure::new_stdout(0, RMUX_USAGE));
        }
        if let Some(flag) = invalid_short_option_in_cluster(bytes) {
            let flag = char::from(flag);
            return Some(ExitFailure::new(
                1,
                format!("rmux: unknown option -- {flag}\n{RMUX_USAGE}"),
            ));
        }
        if short_option_consumes_next_argument(bytes) {
            index += 1;
        }

        index += 1;
    }

    None
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
