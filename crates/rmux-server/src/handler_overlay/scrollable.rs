use std::io;

use rmux_core::{key_code_lookup_bits, key_string_lookup_string, KeyCode};
use rmux_proto::{CommandOutput, RmuxError, SessionId, SessionName, Target};

use super::identity::OverlayIdentity;
use super::parse::{ParsedDisplayPopupCommand, PopupSizeSpec};
use super::state::ClientOverlayState;
use super::RequestHandler;
use crate::handler::attach_support::{ActiveAttach, ActiveAttachIdentity};

#[derive(Debug, Clone)]
pub(in crate::handler) struct ScrollablePopupText {
    lines: Vec<String>,
    offset: usize,
}

impl ScrollablePopupText {
    fn from_stdout(stdout: &[u8]) -> Self {
        let mut lines = stdout
            .split(|byte| *byte == b'\n')
            .map(|line| {
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                String::from_utf8_lossy(line).into_owned()
            })
            .collect::<Vec<_>>();
        if stdout.ends_with(b"\n") {
            let _ = lines.pop();
        }
        Self { lines, offset: 0 }
    }

    pub(in crate::handler) fn visible_lines(&self, rows: u16) -> Vec<String> {
        let rows = usize::from(rows);
        let start = self.offset.min(self.max_offset(rows));
        let end = start.saturating_add(rows).min(self.lines.len());
        self.lines[start..end].to_vec()
    }

    #[cfg(test)]
    pub(in crate::handler) fn offset(&self) -> usize {
        self.offset
    }

    #[cfg(test)]
    pub(in crate::handler) fn line_count(&self) -> usize {
        self.lines.len()
    }

    fn apply(&mut self, action: ScrollablePopupAction, rows: u16) -> bool {
        let rows = usize::from(rows).max(1);
        let max_offset = self.max_offset(rows);
        let current = self.offset.min(max_offset);
        let next = match action {
            ScrollablePopupAction::LineUp => current.saturating_sub(1),
            ScrollablePopupAction::LineDown => current.saturating_add(1).min(max_offset),
            ScrollablePopupAction::PageUp => current.saturating_sub(rows),
            ScrollablePopupAction::PageDown => current.saturating_add(rows).min(max_offset),
            ScrollablePopupAction::Home => 0,
            ScrollablePopupAction::End => max_offset,
            ScrollablePopupAction::Close | ScrollablePopupAction::Ignore => current,
        };
        self.offset = next;
        next != current
    }

    fn max_offset(&self, rows: usize) -> usize {
        self.lines.len().saturating_sub(rows)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollablePopupAction {
    LineUp,
    LineDown,
    PageUp,
    PageDown,
    Home,
    End,
    Close,
    Ignore,
}

#[derive(Debug, Clone, Copy)]
pub(in crate::handler) struct AttachedHelpContext<'a> {
    pub(in crate::handler) attach_pid: u32,
    pub(in crate::handler) expected_identity: Option<ActiveAttachIdentity>,
    pub(in crate::handler) expected_session_name: &'a SessionName,
    pub(in crate::handler) expected_session_id: SessionId,
    pub(in crate::handler) target: &'a Target,
}

fn scrollable_popup_action(key: KeyCode) -> ScrollablePopupAction {
    let key = key_code_lookup_bits(key);

    // Match exact normalized keys. In particular, modified cursor keys remain
    // inert instead of being silently treated as their unmodified navigation
    // counterparts. The attached decoder normalizes CSI, SS3 and CSI-u forms
    // before this modal surface sees them.
    if key_matches_any(key, &["Up", "k", "C-p"]) {
        ScrollablePopupAction::LineUp
    } else if key_matches_any(key, &["Down", "j", "C-n"]) {
        ScrollablePopupAction::LineDown
    } else if key_matches_any(key, &["PPage", "C-b", "M-v"]) {
        ScrollablePopupAction::PageUp
    } else if key_matches_any(key, &["NPage", "C-f", "C-v"]) {
        ScrollablePopupAction::PageDown
    } else if key_matches_any(key, &["Home", "g", "M-<"]) {
        ScrollablePopupAction::Home
    } else if key_matches_any(key, &["End", "G", "M->"]) {
        ScrollablePopupAction::End
    } else if key_matches_any(key, &["q", "Escape", "C-c"]) {
        ScrollablePopupAction::Close
    } else {
        ScrollablePopupAction::Ignore
    }
}

fn key_matches_any(key: KeyCode, names: &[&str]) -> bool {
    names.iter().any(|name| {
        key_string_lookup_string(name)
            .is_some_and(|candidate| key == key_code_lookup_bits(candidate))
    })
}

impl RequestHandler {
    pub(in crate::handler) async fn show_attached_key_help_popup(
        &self,
        context: AttachedHelpContext<'_>,
        output: &CommandOutput,
    ) -> Result<bool, RmuxError> {
        let AttachedHelpContext {
            attach_pid,
            expected_identity,
            expected_session_name,
            expected_session_id,
            target,
        } = context;
        if output.stdout().is_empty() || target.session_name() != expected_session_name {
            return Ok(false);
        }
        let attach_identity = {
            let active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get(&attach_pid) else {
                return Ok(false);
            };
            if !attached_help_surface_available(
                active,
                expected_identity,
                expected_session_name,
                expected_session_id,
            ) {
                return Ok(false);
            }
            active.identity(attach_pid)
        };

        let command = ParsedDisplayPopupCommand {
            target_client: None,
            target_pane: None,
            title: "list-keys -N (q/Esc close)".to_owned(),
            x: Some("C".to_owned()),
            y: Some("C".to_owned()),
            width: Some(PopupSizeSpec::Percent(90)),
            height: Some(PopupSizeSpec::Percent(80)),
            style: None,
            border_style: None,
            border_lines: None,
            close_existing: false,
            close_on_exit: false,
            close_on_zero_exit: false,
            close_any_key: false,
            no_job: true,
            start_directory: None,
            environment: Vec::new(),
            command: None,
        };
        let overlay_identity = {
            let mut state = self.state.lock().await;
            OverlayIdentity::capture(&mut state, attach_identity, target.clone())?
        };
        let mut popup = self
            .build_display_popup_state(attach_pid, command, target.clone(), overlay_identity)
            .await?;
        popup.scrollable_text = Some(ScrollablePopupText::from_stdout(output.stdout()));

        {
            let state = self.state.lock().await;
            let mut active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
                return Ok(false);
            };
            if !attached_help_surface_available(
                active,
                expected_identity,
                expected_session_name,
                expected_session_id,
            ) {
                return Ok(false);
            }
            if !popup
                .identity
                .matches(&state, active, &popup.current_target)
            {
                return Ok(false);
            }
            active.overlay_state_id = active.overlay_state_id.saturating_add(1);
            popup.id = active.overlay_state_id;
            active.overlay = Some(ClientOverlayState::Popup(Box::new(popup)));
        }

        self.refresh_interactive_overlay_for_optional_identity(attach_pid, expected_identity)
            .await?;
        Ok(true)
    }

    pub(in crate::handler) async fn handle_scrollable_popup_key_input(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        popup_id: u64,
        key: KeyCode,
    ) -> io::Result<bool> {
        let action = scrollable_popup_action(key);
        if action == ScrollablePopupAction::Close {
            self.clear_interactive_overlay_for_optional_identity_and_id(
                attach_pid, identity, popup_id, true,
            )
            .await
            .map_err(io::Error::other)?;
            return Ok(true);
        }

        let changed =
            {
                let mut active_attach = self.active_attach.lock().await;
                let Some(active) = active_attach.by_pid.get_mut(&attach_pid).filter(|active| {
                    identity.is_none_or(|identity| identity.matches_active(active))
                }) else {
                    return Ok(false);
                };
                let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_mut() else {
                    return Ok(false);
                };
                if popup.id != popup_id {
                    return Ok(false);
                }
                let rows = popup.content_size().rows;
                let Some(text) = popup.scrollable_text.as_mut() else {
                    return Ok(false);
                };
                text.apply(action, rows)
            };
        if changed {
            self.refresh_interactive_overlay_for_optional_identity(attach_pid, identity)
                .await
                .map_err(io::Error::other)?;
        }
        Ok(true)
    }
}

fn attached_help_surface_available(
    active: &ActiveAttach,
    expected_identity: Option<ActiveAttachIdentity>,
    expected_session_name: &SessionName,
    expected_session_id: SessionId,
) -> bool {
    expected_identity.is_none_or(|identity| {
        identity.matches_active_session(active, expected_session_name, expected_session_id)
    }) && &active.session_name == expected_session_name
        && active.session_id == expected_session_id
        && !active.suspended
        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
        && active.prompt.is_none()
        && active.mode_tree.is_none()
        && active.overlay.is_none()
        && active.display_panes.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key_table::{decode_attached_key, AttachedKeyDecode};

    fn decoded_action(sequence: &[u8]) -> ScrollablePopupAction {
        let AttachedKeyDecode::Matched { size, key } = decode_attached_key(sequence, None) else {
            panic!("expected a complete key sequence: {sequence:?}");
        };
        assert_eq!(size, sequence.len(), "complete key consumption");
        scrollable_popup_action(key)
    }

    #[test]
    fn static_text_preserves_all_lines_and_clamps_navigation() {
        let mut text = ScrollablePopupText::from_stdout(b"first\nsecond\nthird\nlast\n");
        assert_eq!(text.visible_lines(2), ["first", "second"]);
        assert!(text.apply(ScrollablePopupAction::End, 2));
        assert_eq!(text.visible_lines(2), ["third", "last"]);
        assert!(!text.apply(ScrollablePopupAction::LineDown, 2));
        assert!(text.apply(ScrollablePopupAction::Home, 2));
        assert_eq!(text.visible_lines(2), ["first", "second"]);
    }

    #[test]
    fn static_text_normalizes_terminal_navigation_families() {
        for sequence in [b"\x1b[A".as_slice(), b"\x1bOA"] {
            assert_eq!(decoded_action(sequence), ScrollablePopupAction::LineUp);
        }
        for sequence in [b"\x1b[B".as_slice(), b"\x1bOB"] {
            assert_eq!(decoded_action(sequence), ScrollablePopupAction::LineDown);
        }
        for sequence in [b"\x1b[H".as_slice(), b"\x1b[1~", b"\x1b[7~", b"\x1bOH"] {
            assert_eq!(decoded_action(sequence), ScrollablePopupAction::Home);
        }
        for sequence in [b"\x1b[F".as_slice(), b"\x1b[4~", b"\x1b[8~", b"\x1bOF"] {
            assert_eq!(decoded_action(sequence), ScrollablePopupAction::End);
        }
        assert_eq!(decoded_action(b"\x1b[5~"), ScrollablePopupAction::PageUp);
        assert_eq!(decoded_action(b"\x1b[6~"), ScrollablePopupAction::PageDown);
    }

    #[test]
    fn static_text_keeps_vi_emacs_close_and_modified_key_policy_explicit() {
        for (sequence, expected) in [
            (b"k".as_slice(), ScrollablePopupAction::LineUp),
            (b"j".as_slice(), ScrollablePopupAction::LineDown),
            (b"\x02".as_slice(), ScrollablePopupAction::PageUp),
            (b"\x06".as_slice(), ScrollablePopupAction::PageDown),
            (b"\x1bv".as_slice(), ScrollablePopupAction::PageUp),
            (b"\x1b<".as_slice(), ScrollablePopupAction::Home),
            (b"\x1b>".as_slice(), ScrollablePopupAction::End),
            (b"g".as_slice(), ScrollablePopupAction::Home),
            (b"G".as_slice(), ScrollablePopupAction::End),
            (b"q".as_slice(), ScrollablePopupAction::Close),
            (b"\x03".as_slice(), ScrollablePopupAction::Close),
            (b"x".as_slice(), ScrollablePopupAction::Ignore),
            (b"\x1b[C".as_slice(), ScrollablePopupAction::Ignore),
            (b"\x1b[1;2A".as_slice(), ScrollablePopupAction::Ignore),
        ] {
            assert_eq!(decoded_action(sequence), expected, "sequence {sequence:?}");
        }
        assert_eq!(
            scrollable_popup_action(KeyCode::from(b'\x1b')),
            ScrollablePopupAction::Close
        );
    }
}
