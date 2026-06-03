use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::PathBuf;

use regex::Regex;
use rmux_proto::RmuxError;
use serde::Deserialize;

include!(concat!(env!("OUT_DIR"), "/tunnel_presets.rs"));

const USER_PRESET_ENV: &str = "RMUX_TUNNEL_PRESET_DIR";
const DEFAULT_READY_TIMEOUT_SECS: u64 = 30;
const MAX_READY_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TunnelPreset {
    pub(super) name: String,
    pub(super) program: String,
    #[serde(default)]
    pub(super) args: Vec<String>,
    pub(super) url_pattern: String,
    #[serde(default)]
    pub(super) ready_pattern: Option<String>,
    #[serde(default)]
    pub(super) url_source: UrlSource,
    #[serde(default = "default_ready_timeout_secs")]
    pub(super) ready_timeout_secs: u64,
    #[serde(default)]
    pub(super) install_hint: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum UrlSource {
    #[default]
    Both,
    Stderr,
    Stdout,
}

impl UrlSource {
    pub(super) const fn accepts(self, source: ProcessOutput) -> bool {
        matches!(
            (self, source),
            (Self::Both, _)
                | (Self::Stderr, ProcessOutput::Stderr)
                | (Self::Stdout, ProcessOutput::Stdout)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProcessOutput {
    Stderr,
    Stdout,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum PresetSource {
    Embedded,
    File,
}

pub(super) fn load(name: &str) -> Result<TunnelPreset, RmuxError> {
    if !valid_name(name) {
        return Err(RmuxError::Server(
            "web-share tunnel provider names may contain only ASCII letters, digits, '-' and '_'"
                .to_owned(),
        ));
    }
    for path in preset_paths(name) {
        if path.is_file() {
            let content = fs::read_to_string(&path).map_err(|error| {
                RmuxError::Server(format!(
                    "failed to read web-share tunnel preset '{}': {error}",
                    path.display()
                ))
            })?;
            return parse(name, PresetSource::File, &content);
        }
    }
    if let Some((_, content)) = embedded()
        .iter()
        .find(|(preset_name, _)| *preset_name == name)
    {
        return parse(name, PresetSource::Embedded, content);
    }
    Err(RmuxError::Server(no_preset_message(name)))
}

#[cfg(test)]
pub(super) fn embedded() -> &'static [(&'static str, &'static str)] {
    SHIPPED_TUNNEL_PRESETS
}

#[cfg(not(test))]
fn embedded() -> &'static [(&'static str, &'static str)] {
    SHIPPED_TUNNEL_PRESETS
}

pub(super) fn parse(
    expected_name: &str,
    _source: PresetSource,
    content: &str,
) -> Result<TunnelPreset, RmuxError> {
    let preset: TunnelPreset = toml::from_str(content).map_err(|error| {
        RmuxError::Server(format!(
            "failed to parse web-share tunnel preset '{expected_name}': {error}"
        ))
    })?;
    if preset.name != expected_name {
        return Err(RmuxError::Server(format!(
            "web-share tunnel preset '{expected_name}' declares name '{}'",
            preset.name
        )));
    }
    validate(&preset)?;
    Ok(preset)
}

fn validate(preset: &TunnelPreset) -> Result<(), RmuxError> {
    if !valid_name(&preset.name) {
        return Err(RmuxError::Server(format!(
            "web-share tunnel preset '{}' has an invalid name",
            preset.name
        )));
    }
    if preset.program.trim().is_empty() || preset.program.contains('\0') {
        return Err(RmuxError::Server(format!(
            "web-share tunnel preset '{}' must define a program",
            preset.name
        )));
    }
    if preset.args.iter().any(|arg| arg.contains('\0')) {
        return Err(RmuxError::Server(format!(
            "web-share tunnel preset '{}' contains an invalid argument",
            preset.name
        )));
    }
    if preset.ready_timeout_secs == 0 || preset.ready_timeout_secs > MAX_READY_TIMEOUT_SECS {
        return Err(RmuxError::Server(format!(
            "web-share tunnel preset '{}' ready_timeout_secs must be between 1 and {MAX_READY_TIMEOUT_SECS}",
            preset.name
        )));
    }
    Regex::new(&preset.url_pattern).map_err(|error| {
        RmuxError::Server(format!(
            "web-share tunnel preset '{}' has an invalid url_pattern: {error}",
            preset.name
        ))
    })?;
    if let Some(pattern) = preset.ready_pattern.as_deref() {
        Regex::new(pattern).map_err(|error| {
            RmuxError::Server(format!(
                "web-share tunnel preset '{}' has an invalid ready_pattern: {error}",
                preset.name
            ))
        })?;
    }
    Ok(())
}

fn preset_paths(name: &str) -> Vec<PathBuf> {
    preset_dirs()
        .into_iter()
        .map(|dir| dir.join(format!("{name}.toml")))
        .collect()
}

fn preset_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = env::var_os(USER_PRESET_ENV) {
        dirs.push(PathBuf::from(dir));
    }
    if let Some(config_home) = env::var_os("XDG_CONFIG_HOME") {
        dirs.push(PathBuf::from(config_home).join("rmux/tunnels"));
    } else if let Some(home) = env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".config/rmux/tunnels"));
    }
    dirs.push(PathBuf::from("/usr/local/share/rmux/tunnels"));
    dirs.push(PathBuf::from("/usr/share/rmux/tunnels"));
    dirs
}

fn no_preset_message(name: &str) -> String {
    let names = available();
    let locations = preset_dirs()
        .into_iter()
        .map(|path| format!("  {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    let available = if names.is_empty() {
        "none".to_owned()
    } else {
        names.join(", ")
    };
    format!(
        "no web-share tunnel preset named '{name}'. Available: {available}. Preset locations:\n{locations}"
    )
}

fn available() -> Vec<String> {
    available_from(
        embedded()
            .iter()
            .map(|(name, content)| ((*name).to_owned(), *content)),
        preset_dirs(),
    )
}

pub(super) fn available_from(
    embedded: impl IntoIterator<Item = (String, &'static str)>,
    dirs: Vec<PathBuf>,
) -> Vec<String> {
    let mut names = embedded
        .into_iter()
        .map(|(name, _)| name)
        .collect::<BTreeSet<_>>();
    for dir in dirs {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "toml")
                {
                    if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                        if valid_name(stem) {
                            names.insert(stem.to_owned());
                        }
                    }
                }
            }
        }
    }
    names.into_iter().collect()
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

const fn default_ready_timeout_secs() -> u64 {
    DEFAULT_READY_TIMEOUT_SECS
}

#[cfg(test)]
mod tests {
    use super::{parse, PresetSource, UrlSource};

    #[test]
    fn parse_rejects_mismatched_name() {
        let error = parse(
            "expected",
            PresetSource::Embedded,
            r#"
name = "other"
program = "tool"
url_pattern = "https://example\\.test"
"#,
        )
        .expect_err("name mismatch rejected");
        assert!(error.to_string().contains("declares name"));
    }

    #[test]
    fn parse_uses_safe_defaults() {
        let preset = parse(
            "tool",
            PresetSource::Embedded,
            r#"
name = "tool"
program = "tool"
args = ["--port", "{port}"]
url_pattern = "https://example\\.test"
"#,
        )
        .expect("preset parses");
        assert_eq!(preset.url_source, UrlSource::Both);
        assert_eq!(preset.ready_timeout_secs, 30);
    }
}
