//! Copy-mode command context: mode options, default shell and pane working directory.

use crate::copy_mode::{CopyModeCommandContext, ModeKeys};
use crate::pane_terminals::HandlerState;
use rmux_proto::{OptionName, PaneTarget};

pub(super) fn copy_mode_context(
    state: &HandlerState,
    target: &PaneTarget,
    refresh_screen: Option<rmux_core::Screen>,
    mouse: Option<crate::copy_mode::CopyModeMouseContext>,
) -> CopyModeCommandContext {
    let pane_profile = state
        .pane_profile_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .ok();
    #[cfg(unix)]
    let default_shell = "/bin/sh".to_owned();
    #[cfg(not(unix))]
    let default_shell = pane_profile
        .as_ref()
        .map(|profile| profile.shell().to_string_lossy().into_owned())
        .or_else(|| {
            state
                .options
                .resolve(Some(target.session_name()), OptionName::DefaultShell)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(process_default_shell);
    let pane_cwd = pane_profile.map(|profile| profile.cwd().to_path_buf());
    let working_directory = state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .and_then(|pane| state.pane_screen_state(target.session_name(), pane.id()))
        .and_then(|screen_state| working_directory_from_screen_path(&screen_state.path))
        .or(pane_cwd);
    let word_separators = state
        .options
        .resolve(Some(target.session_name()), OptionName::WordSeparators)
        .filter(|value| !value.is_empty())
        .unwrap_or(" -_@")
        .to_owned();

    CopyModeCommandContext {
        mode_keys: ModeKeys::parse(state.options.resolve_for_pane(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
            OptionName::ModeKeys,
        )),
        line_number_mode: crate::copy_mode::CopyModeLineNumberMode::parse(
            state.options.resolve_for_pane(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
                OptionName::CopyModeLineNumbers,
            ),
        ),
        wrap_search: state.options.resolve_for_pane(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
            OptionName::WrapSearch,
        ) != Some("off"),
        word_separators,
        default_shell,
        working_directory,
        refresh_screen,
        mouse,
    }
}

fn working_directory_from_screen_path(path: &str) -> Option<std::path::PathBuf> {
    if path.is_empty() {
        return None;
    }
    let Some(rest) = path.strip_prefix("file://") else {
        return Some(path.into());
    };
    let (host, raw_path) = match rest.split_once('/') {
        Some((host, tail)) => (host, format!("/{tail}")),
        None => return None,
    };
    if !file_url_host_is_local(host) {
        return None;
    }
    percent_decode_path(&raw_path).map(std::path::PathBuf::from)
}

fn file_url_host_is_local(host: &str) -> bool {
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let Some(local) = crate::host_name::local_hostname() else {
        return false;
    };
    host.eq_ignore_ascii_case(&local)
        || host
            .split('.')
            .next()
            .is_some_and(|short| short.eq_ignore_ascii_case(&local))
        || local
            .split('.')
            .next()
            .is_some_and(|short| host.eq_ignore_ascii_case(short))
}

fn percent_decode_path(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = bytes.get(index + 1).and_then(|byte| hex_value(*byte))?;
            let low = bytes.get(index + 2).and_then(|byte| hex_value(*byte))?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    let decoded = String::from_utf8(decoded).ok()?;
    Some(normalize_file_url_path_for_platform(decoded))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(windows)]
fn normalize_file_url_path_for_platform(mut path: String) -> String {
    let bytes = path.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        path.remove(0);
    }
    path
}

#[cfg(not(windows))]
fn normalize_file_url_path_for_platform(path: String) -> String {
    path
}

#[cfg(windows)]
fn process_default_shell() -> String {
    std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_owned())
}

#[cfg(not(any(unix, windows)))]
fn process_default_shell() -> String {
    "sh".to_owned()
}
