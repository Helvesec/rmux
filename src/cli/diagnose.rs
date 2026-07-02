use std::ffi::OsString;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use rmux_client::{connect, resolve_socket_path};
use rmux_proto::Response;

use super::ExitFailure;

#[cfg(windows)]
#[path = "diagnose_windows.rs"]
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
    let report = DiagnoseReport::collect(&invocation)?;
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
    let mut index = 0;

    while let Some(argument) = arguments.get(index) {
        let value = argument.to_str()?;
        if value == "--" {
            return Some((index + 1, prefix));
        }
        if !value.starts_with('-') || value == "-" {
            return Some((index, prefix));
        }

        match value {
            "-2" => prefix.assume_256_colors = true,
            "-u" => prefix.utf8 = true,
            "-D" | "-N" | "-l" => {}
            "-C" | "-v" => {}
            "-L" => {
                index += 1;
                prefix.socket_name = arguments.get(index).cloned();
            }
            "-S" => {
                index += 1;
                prefix.socket_path = arguments.get(index).map(PathBuf::from);
            }
            "-f" => {
                index += 1;
                if let Some(path) = arguments.get(index) {
                    prefix.config_files.push(PathBuf::from(path));
                }
            }
            "-T" => {
                index += 1;
                if let Some(features) = arguments.get(index).and_then(|value| value.to_str()) {
                    push_terminal_features(&mut prefix.terminal_features, features);
                }
            }
            _ if value.starts_with("-L") && value.len() > 2 => {
                prefix.socket_name = Some(OsString::from(&value[2..]));
            }
            _ if value.starts_with("-S") && value.len() > 2 => {
                prefix.socket_path = Some(PathBuf::from(&value[2..]));
            }
            _ if value.starts_with("-f") && value.len() > 2 => {
                prefix.config_files.push(PathBuf::from(&value[2..]));
            }
            _ if value.starts_with("-T") && value.len() > 2 => {
                push_terminal_features(&mut prefix.terminal_features, &value[2..]);
            }
            _ if is_short_flag_cluster(value, "2CDNluv") => {
                prefix.assume_256_colors |= value.contains('2');
                prefix.utf8 |= value.contains('u');
            }
            _ => return Some((index, prefix)),
        }

        index += 1;
    }

    None
}

fn is_short_flag_cluster(value: &str, allowed: &str) -> bool {
    value.len() > 2
        && value.starts_with('-')
        && !value.starts_with("--")
        && value.chars().skip(1).all(|flag| allowed.contains(flag))
}

fn parse_diagnose_format(arguments: &[OsString]) -> Result<DiagnoseFormat, ExitFailure> {
    let mut format = None;
    for argument in arguments {
        match argument.to_str() {
            Some("--human") => set_format(&mut format, DiagnoseFormat::Human)?,
            Some("--json") => set_format(&mut format, DiagnoseFormat::Json)?,
            Some("--help") => {
                return Err(ExitFailure::new_stdout(
                    0,
                    "usage: rmux diagnose [--human|--json]",
                ));
            }
            Some(other) => {
                return Err(ExitFailure::new(
                    1,
                    format!("rmux diagnose: unknown argument '{other}'"),
                ));
            }
            None => {
                return Err(ExitFailure::new(
                    1,
                    "rmux diagnose: arguments must be valid UTF-8",
                ));
            }
        }
    }

    Ok(format.unwrap_or(DiagnoseFormat::Human))
}

fn set_format(
    current: &mut Option<DiagnoseFormat>,
    next: DiagnoseFormat,
) -> Result<(), ExitFailure> {
    if current.is_some_and(|current| current != next) {
        return Err(ExitFailure::new(
            1,
            "rmux diagnose: choose only one of --human or --json",
        ));
    }
    *current = Some(next);
    Ok(())
}

impl DiagnoseReport {
    fn collect(invocation: &DiagnoseInvocation) -> Result<Self, ExitFailure> {
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
        let config_paths = if invocation.config_files.is_empty() {
            default_config_paths()
        } else {
            invocation.config_files.clone()
        };
        let config_paths = config_paths
            .iter()
            .map(|path| redact_path(path))
            .collect::<Vec<_>>();
        let config_messages = collect_config_messages(&socket_path);
        let osc52 = if terminal_looks_clipboard_capable(&term, &term_program, &terminal_features) {
            "available-when-requested"
        } else {
            "not-advertised"
        };

        Ok(Self {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            os_name: std::env::consts::OS.to_owned(),
            os_arch: std::env::consts::ARCH.to_owned(),
            os_version: os_version(),
            terminal_host,
            term,
            term_program,
            shell: detected_shell(),
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
        })
    }

    fn render_human(&self) -> String {
        let mut output = String::new();
        output.push_str("rmux diagnose\n");
        output.push_str(&format!("version: {}\n", self.version));
        output.push_str(&format!("os: {} ({})\n", self.os_name, self.os_arch));
        output.push_str(&format!("os_version: {}\n", self.os_version));
        output.push_str(&format!("terminal_host: {}\n", self.terminal_host));
        output.push_str(&format!("term: {}\n", self.term));
        output.push_str(&format!("term_program: {}\n", self.term_program));
        output.push_str(&format!("shell: {}\n", self.shell));
        output.push_str(&format!("socket_path: {}\n", self.socket_path));
        output.push_str(&format!("config_mode: {}\n", self.config_mode));
        output.push_str("config_paths:\n");
        for path in &self.config_paths {
            output.push_str(&format!("  - {path}\n"));
        }
        output.push_str("config_messages:\n");
        if self.config_messages.is_empty() {
            output.push_str("  - none\n");
        } else {
            for message in &self.config_messages {
                output.push_str(&format!("  - {message}\n"));
            }
        }
        output.push_str("capabilities:\n");
        output.push_str(&format!("  conpty: {}\n", self.conpty));
        output.push_str(&format!("  osc52: {}\n", self.osc52));
        output.push_str(&format!(
            "  terminal_features: {}\n",
            render_feature_list(&self.terminal_features)
        ));
        output.push_str("privacy: environment values are summarized or redacted\n");
        output
    }

    fn render_json(&self) -> String {
        format!(
            concat!(
                "{{\n",
                "  \"version\": {},\n",
                "  \"os\": {{\"name\": {}, \"arch\": {}, \"version\": {}}},\n",
                "  \"terminal\": {{\"host\": {}, \"term\": {}, \"term_program\": {}}},\n",
                "  \"shell\": {},\n",
                "  \"socket_path\": {},\n",
                "  \"config\": {{\"mode\": {}, \"paths\": {}, \"messages\": {}}},\n",
                "  \"capabilities\": {{\"conpty\": {}, \"osc52\": {}, \"terminal_features\": {}}},\n",
                "  \"privacy\": {{\"environment_values\": \"summarized-or-redacted\"}}\n",
                "}}\n"
            ),
            json_string(&self.version),
            json_string(&self.os_name),
            json_string(&self.os_arch),
            json_string(&self.os_version),
            json_string(&self.terminal_host),
            json_string(&self.term),
            json_string(&self.term_program),
            json_string(&self.shell),
            json_string(&self.socket_path),
            json_string(&self.config_mode),
            json_array(&self.config_paths),
            json_array(&self.config_messages),
            json_string(&self.conpty),
            json_string(&self.osc52),
            json_array(&self.terminal_features),
        )
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
        .collect()
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
    detail == "unmatched }"
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

fn default_config_paths() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        windows_default_config_paths()
    }
    #[cfg(not(windows))]
    {
        unix_default_config_paths()
    }
}

#[cfg(not(windows))]
fn unix_default_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    push_unique_path(&mut paths, PathBuf::from("/etc/rmux.conf"));
    if let Some(home) = nonempty_env_os("HOME") {
        let home = PathBuf::from(home);
        push_unique_path(&mut paths, home.join(".rmux.conf"));
    }
    if let Some(xdg_config_home) = nonempty_env_os("XDG_CONFIG_HOME") {
        push_unique_path(
            &mut paths,
            PathBuf::from(xdg_config_home)
                .join("rmux")
                .join("rmux.conf"),
        );
    }
    if let Some(home) = nonempty_env_os("HOME") {
        let home = PathBuf::from(home);
        push_unique_path(
            &mut paths,
            home.join(".config").join("rmux").join("rmux.conf"),
        );
    }
    paths
}

#[cfg(windows)]
fn windows_default_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(xdg_config_home) = nonempty_env_os("XDG_CONFIG_HOME") {
        push_unique_path(
            &mut paths,
            PathBuf::from(xdg_config_home)
                .join("rmux")
                .join("rmux.conf"),
        );
    }
    if let Some(userprofile) = nonempty_env_os("USERPROFILE") {
        let userprofile = PathBuf::from(userprofile);
        push_unique_path(&mut paths, userprofile.join(".rmux.conf"));
    }
    if let Some(appdata) = nonempty_env_os("APPDATA") {
        push_unique_path(
            &mut paths,
            PathBuf::from(appdata).join("rmux").join("rmux.conf"),
        );
    }
    if let Some(config_file) = nonempty_env_os("RMUX_CONFIG_FILE") {
        push_unique_path(&mut paths, PathBuf::from(config_file));
    }
    paths
}

fn nonempty_env_os(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.contains(&path) {
        paths.push(path);
    }
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
    let mut output = String::from("[");
    for (index, value) in values.iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        output.push_str(&json_string(value));
    }
    output.push(']');
    output
}

fn json_string(value: &str) -> String {
    let mut output = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            ch if ch.is_control() => {
                output.push_str(&format!("\\u{:04x}", ch as u32));
            }
            ch => output.push(ch),
        }
    }
    output.push('"');
    output
}

fn write_stdout(output: &str) -> Result<i32, ExitFailure> {
    match io::stdout().lock().write_all(output.as_bytes()) {
        Ok(()) => Ok(0),
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(0),
        Err(error) => Err(ExitFailure::new(1, error.to_string())),
    }
}

#[cfg(test)]
#[path = "diagnose_tests.rs"]
mod tests;
