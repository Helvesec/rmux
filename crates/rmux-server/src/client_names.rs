//! Canonical client names.
//!
//! tmux names a client once and every surface reuses that one name:
//! `list-clients`, `#{client_name}`, `#{hook_client}` and the control-mode
//! `%client-session-changed` / `%client-detached` notifications.
//!
//! Frozen tmux 3.7b, measured 2026-07-25 on one server carrying two `-C`
//! clients (pids 74711/74712) and one PTY client on `/dev/ttys021`:
//!
//! ```text
//! $ tmux list-clients -F '#{client_pid} #{client_name}'
//! 74711 client-74711
//! 74712 client-74712
//! 78407 /dev/ttys021
//!
//! # control stream of the client that stayed behind
//! %client-session-changed client-74711 $1 beta
//! %client-session-changed /dev/ttys021 $1 beta
//! %client-detached client-74711
//! %client-detached /dev/ttys021
//!
//! # hook_client for client-attached / client-detached / client-session-changed
//! hook_client=client-74711
//! hook_client=/dev/ttys021
//! ```
//!
//! Producing those names in one place is what keeps the surfaces agreeing.

use std::path::PathBuf;

/// Returns the tty a PTY client is attached to, which is also its tmux name.
pub(crate) fn attached_client_tty_path(attach_pid: u32) -> Option<PathBuf> {
    rmux_os::process::fd_path(attach_pid, 0)
}

/// Returns the name tmux gives a PTY client: the tty path it attached from.
pub(crate) fn attached_client_name(attach_pid: u32) -> String {
    attached_client_tty_path(attach_pid)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| attach_pid.to_string())
}

/// Returns the name tmux gives a control-mode client: `client-<pid>`.
pub(crate) fn control_client_name(control_pid: u32) -> String {
    format!("client-{control_pid}")
}

/// Reports whether `client_name` names the control client `control_pid`.
///
/// tmux compares client pointers to decide whether a client's session move is
/// its own (`%session-changed`) or somebody else's (`%client-session-changed`).
/// The notification carries a name, so the comparison is made against the same
/// canonical spelling the name was produced with rather than by re-parsing an
/// identifier out of it: a PTY client's tty-path name can never be mistaken for
/// a control client.
pub(crate) fn is_control_client_name(client_name: &str, control_pid: u32) -> bool {
    client_name == control_client_name(control_pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_client_names_are_recognised_only_for_their_own_client() {
        assert_eq!(control_client_name(74_711), "client-74711");
        assert!(is_control_client_name(&control_client_name(74_711), 74_711));
        assert!(!is_control_client_name(
            &control_client_name(74_712),
            74_711
        ));
    }

    #[test]
    fn attached_client_names_are_never_read_as_control_client_names() {
        for name in ["/dev/ttys021", "74711", "client-", "client-74711x"] {
            assert!(
                !is_control_client_name(name, 74_711),
                "{name} must not be read as control client 74711"
            );
        }
    }
}
