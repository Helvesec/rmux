use std::ffi::OsString;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use rmux_client::{connect, resolve_socket_path};
use rmux_proto::Response;
use rmux_server::config_paths::{ConfigPathEncoding, ConfigPathResolver, ResolvedConfigPaths};
use rmux_server::ConfigFileSelection;

use super::ExitFailure;

mod configuration;
mod report;

// Keep the complete help on one registry line, shared with top-level --help.
pub(super) const DIAGNOSE_HELP: &str = "diagnose [--human|--json]  \
    --json: Emit JSON results on stdout; command errors use text on stderr \
    with a nonzero exit and empty stdout. Supported unsuccessful results remain JSON";

#[cfg(windows)]
mod diagnose_windows;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiagnoseFormat {
    Human,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DiagnoseInvocation {
    format: DiagnoseFormat,
    socket_name: Option<OsString>,
    socket_path: Option<PathBuf>,
    config_files: Vec<PathBuf>,
    terminal_features: Vec<String>,
    assume_256_colors: bool,
    utf8: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiagnoseReport {
    schema_version: u32,
    compatibility_registry: &'static str,
    configuration: Option<configuration::Compatibility>,
    version: String,
    os_name: String,
    os_arch: String,
    os_version: String,
    terminal_host: String,
    term: String,
    term_program: String,
    shell: String,
    config_mode: String,
    config_paths: Vec<String>,
    config_messages: Vec<String>,
    socket_path: String,
    conpty: String,
    terminal_features: Vec<String>,
    osc52: String,
}

pub(super) fn parse_invocation(
    arguments: &[OsString],
) -> Result<Option<DiagnoseInvocation>, ExitFailure> {
    let Some((command_index, prefix)) = split_top_level_prefix(arguments) else {
        return Ok(None);
    };
    let Some(command) = arguments
        .get(command_index)
        .and_then(|value| value.to_str())
    else {
        return Ok(None);
    };
    if command != "diagnose" {
        return Ok(None);
    }

    let format = parse_diagnose_format(&arguments[command_index + 1..])?;
    Ok(Some(DiagnoseInvocation {
        format,
        socket_name: prefix.socket_name,
        socket_path: prefix.socket_path,
        config_files: prefix.config_files,
        terminal_features: prefix.terminal_features,
        assume_256_colors: prefix.assume_256_colors,
        utf8: prefix.utf8,
    }))
}

pub(super) fn run(invocation: DiagnoseInvocation) -> Result<i32, ExitFailure> {
    let (mut report, paths) = DiagnoseReport::collect(&invocation)?;
    // Inspect file contents only at this explicit, non-executing boundary.
    report.configuration = Some(configuration::collect(&paths));
    let output = match invocation.format {
        DiagnoseFormat::Human => report.render_human(),
        DiagnoseFormat::Json => report.render_json(),
    };
    write_stdout(&output)
}

#[derive(Default)]
struct TopLevelPrefix {
    socket_name: Option<OsString>,
    socket_path: Option<PathBuf>,
    config_files: Vec<PathBuf>,
    terminal_features: Vec<String>,
    assume_256_colors: bool,
    utf8: bool,
}

fn split_top_level_prefix(arguments: &[OsString]) -> Option<(usize, TopLevelPrefix)> {
    let mut prefix = TopLevelPrefix::default();
    let index = crate::cli_top_level::project_top_level_options(arguments, |flag, value| {
        match (flag, value) {
            (b'2', _) => prefix.assume_256_colors = true,
            (b'u', _) => prefix.utf8 = true,
            (b'L', Some(value)) => prefix.socket_name = Some(value.to_owned()),
            (b'S', Some(value)) => prefix.socket_path = Some(PathBuf::from(value)),
            (b'f', Some(value)) => prefix.config_files.push(PathBuf::from(value)),
            (b'T', Some(value)) => {
                if let Some(features) = value.to_str() {
                    push_terminal_features(&mut prefix.terminal_features, features);
                }
            }
            _ => {}
        }
    })?;
    Some((index, prefix))
}

fn parse_diagnose_format(arguments: &[OsString]) -> Result<DiagnoseFormat, ExitFailure> {
    crate::cli_args::parse_diagnose_args(arguments)
        .map(|args| {
            if args.json {
                DiagnoseFormat::Json
            } else {
                DiagnoseFormat::Human
            }
        })
        .map_err(|error| match error {
            crate::cli_args::DiagnoseArgumentError::Help => {
                ExitFailure::new_stdout(0, format!("usage: rmux {DIAGNOSE_HELP}"))
            }
            crate::cli_args::DiagnoseArgumentError::Invalid(message) => {
                ExitFailure::new(1, message)
            }
        })
}

impl DiagnoseReport {
    fn collect(
        invocation: &DiagnoseInvocation,
    ) -> Result<(Self, ResolvedConfigPaths), ExitFailure> {
        let socket_path = resolve_socket_path(
            invocation.socket_name.as_deref(),
            invocation.socket_path.as_deref(),
        )
        .map_err(ExitFailure::from_client)?;
        let mut terminal_features = invocation.terminal_features.clone();
        if invocation.assume_256_colors {
            push_unique(&mut terminal_features, "256".to_owned());
        }

        let term = env_value("TERM");
        let term_program = env_value("TERM_PROGRAM");
        let terminal_host = detect_terminal_host(&term, &term_program);
        let paths = resolve_config_paths(&invocation.config_files);
        let config_paths = paths
            .primary
            .iter()
            .map(|path| redact_path(path))
            .collect::<Vec<_>>();
        let config_messages = collect_config_messages(&socket_path);
        let osc52 = if terminal_looks_clipboard_capable(&term, &term_program, &terminal_features) {
            "available-when-requested"
        } else {
            "not-advertised"
        };

        Ok((
            Self {
                // Additive report metadata; `version` remains the product version.
                schema_version: 1,
                configuration: None,
                compatibility_registry: "tests/reference/tmux_compat/divergences.toml",
                version: env!("CARGO_PKG_VERSION").to_owned(),
                os_name: std::env::consts::OS.to_owned(),
                os_arch: std::env::consts::ARCH.to_owned(),
                os_version: os_version(),
                terminal_host,
                term,
                term_program,
                shell: redact_path(Path::new(&detected_shell())),
                config_mode: if invocation.config_files.is_empty() {
                    "default".to_owned()
                } else {
                    "custom".to_owned()
                },
                config_paths,
                config_messages,
                socket_path: redact_path(&socket_path),
                conpty: conpty_status().to_owned(),
                terminal_features,
                osc52: osc52.to_owned(),
            },
            paths,
        ))
    }
}

fn collect_config_messages(socket_path: &Path) -> Vec<String> {
    let Ok(mut connection) = connect(socket_path) else {
        return Vec::new();
    };
    let Ok(Response::ShowMessages(response)) = connection.show_messages(false, false, None) else {
        return Vec::new();
    };
    let Ok(output) = std::str::from_utf8(response.command_output().stdout()) else {
        return Vec::new();
    };
    output
        .lines()
        .filter_map(config_message_from_show_messages_line)
        .map(|message| private_config_message(&message))
        .collect()
}

// The daemon retains operational error details. The support report exports
// only their location and allowlisted class, never configuration argument values.
fn private_config_message(message: &str) -> String {
    let (stamp, body) = message.split_once(": ").unwrap_or(("", message));
    let prefix = if stamp.is_empty() {
        String::new()
    } else {
        format!("{stamp}: ")
    };
    if body.starts_with("config ignored:") {
        return format!("{prefix}config ignored: [redacted]");
    }
    let detail = body.strip_prefix("config error: ").unwrap_or(body);
    let label = if detail != body { "config error: " } else { "" };
    if let Some(class) = private_config_detail(detail) {
        return format!("{prefix}{label}{class}");
    }
    for (index, _) in detail.match_indices(':') {
        let after_colon = &detail[index + 1..];
        let digits = after_colon.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || !after_colon[digits..].starts_with(':') {
            continue;
        }
        if let Some(class) = private_config_detail(after_colon[digits + 1..].trim_start()) {
            let location = &detail[..index + digits + 2];
            return format!("{prefix}{label}{location} {class}");
        }
    }
    format!("{prefix}{label}[redacted]")
}

fn private_config_detail(detail: &str) -> Option<String> {
    if detail == "syntax error" {
        return Some(detail.to_owned());
    }
    for class in [
        "unknown command:",
        "unknown option:",
        "invalid option:",
        "ambiguous option:",
    ] {
        if detail.starts_with(class) {
            return Some(format!("{class} [redacted]"));
        }
    }
    ["unmatched }", "missing }", "No such file or directory"]
        .into_iter()
        .find(|class| detail.starts_with(class))
        .map(str::to_owned)
}

fn config_message_from_show_messages_line(line: &str) -> Option<String> {
    let message = line
        .split_once(": ")
        .map(|(_, message)| message)
        .unwrap_or(line);
    if !is_config_diagnostic_message(message) {
        return None;
    }
    Some(truncate_diagnose_line(&redact_text_paths(line), 512))
}

fn is_config_diagnostic_message(message: &str) -> bool {
    message.starts_with("config ignored:")
        || message.starts_with("config error:")
        || source_location_config_diagnostic(message)
}

fn source_location_config_diagnostic(message: &str) -> bool {
    for (colon_index, _) in message.match_indices(':') {
        let after_colon = &message[colon_index + 1..];
        let digits_len = after_colon
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .map(char::len_utf8)
            .sum::<usize>();
        if digits_len == 0 || !after_colon[digits_len..].starts_with(':') {
            continue;
        }
        let path = &message[..colon_index];
        if path.trim().is_empty() {
            continue;
        }
        let detail = after_colon[digits_len + 1..].trim_start();
        if source_location_config_detail(detail) {
            return true;
        }
    }
    false
}

fn source_location_config_detail(detail: &str) -> bool {
    detail == "syntax error"
        || detail == "unmatched }"
        || detail == "missing }"
        || detail.starts_with("unknown command:")
        || detail.starts_with("unknown option:")
        || detail.starts_with("invalid option:")
        || detail.starts_with("ambiguous option:")
        || detail.starts_with("No such file or directory")
}

fn redact_text_paths(text: &str) -> String {
    let mut redacted = text.to_owned();
    for name in ["HOME", "USERPROFILE"] {
        let Some(home) = nonempty_env_os(name) else {
            continue;
        };
        let home = home.to_string_lossy();
        if !home.is_empty() {
            redacted = redacted.replace(home.as_ref(), "~");
            redacted = redacted.replace("~\\", "~/");
        }
    }
    redacted
}

fn truncate_diagnose_line(line: &str, max_bytes: usize) -> String {
    if line.len() <= max_bytes {
        return line.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &line[..end])
}

fn push_terminal_features(features: &mut Vec<String>, raw: &str) {
    for feature in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        push_unique(features, feature.to_owned());
    }
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn env_value(name: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unset".to_owned())
}

fn detect_terminal_host(term: &str, term_program: &str) -> String {
    if std::env::var_os("WT_SESSION").is_some() {
        return "windows-terminal".to_owned();
    }
    if term_program != "unset" {
        return term_program.to_owned();
    }
    if term != "unset" {
        return term.to_owned();
    }
    "unknown".to_owned()
}

fn detected_shell() -> String {
    #[cfg(windows)]
    {
        diagnose_windows::detected_pane_shell()
    }
    #[cfg(not(windows))]
    {
        env_value("SHELL")
    }
}

fn os_version() -> String {
    #[cfg(windows)]
    {
        command_output("cmd", &["/C", "ver"])
    }
    #[cfg(not(windows))]
    {
        command_output("uname", &["-sr"])
    }
}

fn command_output(program: &str, args: &[&str]) -> String {
    ProcessCommand::new(program)
        .args(args)
        .output()
        .ok()
        .and_then(|output| {
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn conpty_status() -> &'static str {
    #[cfg(windows)]
    {
        "available"
    }
    #[cfg(not(windows))]
    {
        "not-applicable"
    }
}

fn terminal_looks_clipboard_capable(
    term: &str,
    term_program: &str,
    terminal_features: &[String],
) -> bool {
    terminal_features
        .iter()
        .any(|feature| feature.eq_ignore_ascii_case("clipboard"))
        || term.starts_with("xterm")
        || term.starts_with("tmux")
        || term.contains("mintty")
        || term.starts_with("foot")
        || term.starts_with("iterm")
        || term_program.eq_ignore_ascii_case("iTerm.app")
        || term_program.eq_ignore_ascii_case("mintty")
}

fn resolve_config_paths(files: &[PathBuf]) -> ResolvedConfigPaths {
    let selection = if files.is_empty() {
        ConfigFileSelection::Default
    } else {
        ConfigFileSelection::Files(files.to_vec())
    };
    ConfigPathResolver::capture().resolve(&selection, ConfigPathEncoding::Native)
}

#[cfg(all(test, unix))]
fn default_config_paths() -> Vec<PathBuf> {
    resolve_config_paths(&[]).primary
}

fn nonempty_env_os(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

fn redact_path(path: &Path) -> String {
    for name in ["HOME", "USERPROFILE"] {
        let Some(home) = nonempty_env_os(name).map(PathBuf::from) else {
            continue;
        };
        if let Ok(rest) = path.strip_prefix(&home) {
            if rest.as_os_str().is_empty() {
                return "~".to_owned();
            }
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

fn render_feature_list(features: &[String]) -> String {
    if features.is_empty() {
        "none".to_owned()
    } else {
        features.join(",")
    }
}

fn json_array(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| json_string(value))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("strings serialize to JSON")
}

fn write_stdout(output: &str) -> Result<i32, ExitFailure> {
    match io::stdout().lock().write_all(output.as_bytes()) {
        Ok(()) => Ok(0),
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(0),
        Err(error) => Err(ExitFailure::new(1, error.to_string())),
    }
}

#[cfg(test)]
#[path = "../diagnose_tests.rs"]
mod tests;
