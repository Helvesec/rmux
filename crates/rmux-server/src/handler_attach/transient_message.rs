use super::state::{ActiveAttach, ActiveAttachIdentity};
use super::RequestHandler;
use crate::handler::pane_support::{
    decode_attached_terminal_control_after_append, TerminalResponseDecode,
};
use crate::handler::pane_support::{
    decode_pane_bound_terminal_string, PaneBoundTerminalStringDecode,
};
use crate::pane_io::{AttachControl, OverlayFrame};
use tokio::sync::oneshot;

#[cfg(test)]
const FOCUS_IN: &[u8] = b"\x1b[I";
#[cfg(test)]
const FOCUS_OUT: &[u8] = b"\x1b[O";
#[cfg(test)]
const LIGHT_THEME: &[u8] = b"\x1b[?997;2n";
#[cfg(test)]
const DARK_THEME: &[u8] = b"\x1b[?997;1n";
const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";
const MAX_IGNORED_CSI_PREFIX: usize = 4 * 1024;
const MAX_IGNORED_OSC_PREFIX: usize = rmux_proto::DEFAULT_MAX_FRAME_LENGTH;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) enum TransientMessageInputPolicy {
    DismissAndForward,
    IgnoreUntilExpiry,
}

#[derive(Debug)]
pub(in crate::handler) struct TransientMessageInputState {
    render_generation: u64,
    overlay_generation: u64,
    status_message: String,
    status_frame: Vec<u8>,
    clear_frame: Vec<u8>,
    policy: TransientMessageInputPolicy,
    ignored_sequence_prefix: Vec<u8>,
    ignoring_bracketed_paste: bool,
    ignored_terminal_string: Option<IgnoredTerminalString>,
    ignored_utf8: Option<IgnoredUtf8>,
    _expiry_cancel: Option<oneshot::Sender<()>>,
}

#[derive(Debug)]
struct IgnoredUtf8 {
    remaining: u8,
    next_min: u8,
    next_max: u8,
}

impl IgnoredUtf8 {
    fn from_lead(byte: u8) -> Option<Self> {
        let (remaining, next_min, next_max) = match byte {
            0xc2..=0xdf => (1, 0x80, 0xbf),
            0xe0 => (2, 0xa0, 0xbf),
            0xe1..=0xec | 0xee..=0xef => (2, 0x80, 0xbf),
            0xed => (2, 0x80, 0x9f),
            0xf0 => (3, 0x90, 0xbf),
            0xf1..=0xf3 => (3, 0x80, 0xbf),
            0xf4 => (3, 0x80, 0x8f),
            _ => return None,
        };
        Some(Self {
            remaining,
            next_min,
            next_max,
        })
    }

    fn consume(&mut self, byte: u8) -> bool {
        if !(self.next_min..=self.next_max).contains(&byte) {
            return false;
        }
        self.remaining -= 1;
        self.next_min = 0x80;
        self.next_max = 0xbf;
        true
    }
}

#[derive(Debug)]
struct IgnoredTerminalString {
    bell_terminated: bool,
    previous_was_escape: bool,
    utf8: Option<IgnoredUtf8>,
}

impl IgnoredTerminalString {
    fn from_prefix(prefix: &[u8]) -> Option<Self> {
        let bell_terminated = if prefix.starts_with(b"\x1b]") {
            true
        } else if prefix.starts_with(b"\x1bP")
            || prefix.starts_with(b"\x1bX")
            || prefix.starts_with(b"\x1b^")
            || prefix.starts_with(b"\x1b_")
        {
            false
        } else {
            match prefix.first().copied() {
                Some(0x9d) => true,
                Some(0x90 | 0x98 | 0x9e | 0x9f) => false,
                _ => return None,
            }
        };
        Some(Self {
            bell_terminated,
            previous_was_escape: prefix.last() == Some(&b'\x1b'),
            utf8: None,
        })
    }

    fn consume(&mut self, byte: u8) -> bool {
        if let Some(utf8) = self.utf8.as_mut() {
            if utf8.consume(byte) {
                if utf8.remaining == 0 {
                    self.utf8 = None;
                }
                self.previous_was_escape = false;
                return false;
            }
            self.utf8 = None;
        }
        if let Some(utf8) = IgnoredUtf8::from_lead(byte) {
            self.utf8 = Some(utf8);
            self.previous_was_escape = false;
            return false;
        }
        let complete = byte == 0x9c
            || (self.bell_terminated && byte == b'\x07')
            || (self.previous_was_escape && byte == b'\\');
        self.previous_was_escape = byte == b'\x1b';
        complete
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) struct TransientMessageRestoreGuard {
    overlay_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) struct TransientMessageRenderSnapshot {
    overlay_generation: u64,
    status_message: String,
}

impl TransientMessageRenderSnapshot {
    pub(super) fn status_message(&self) -> &str {
        &self.status_message
    }
}

impl TransientMessageRestoreGuard {
    pub(super) const fn new(overlay_generation: u64) -> Self {
        Self { overlay_generation }
    }

    pub(in crate::handler) fn matches(self, active: &ActiveAttach) -> bool {
        active.overlay_generation == self.overlay_generation && active.transient_message.is_none()
    }

    pub(in crate::handler) const fn advanced(self) -> Self {
        Self {
            overlay_generation: self.overlay_generation.saturating_add(1),
        }
    }
}

impl TransientMessageInputState {
    pub(super) fn new(
        render_generation: u64,
        overlay_generation: u64,
        status_message: String,
        status_frame: Vec<u8>,
        clear_frame: Vec<u8>,
        policy: TransientMessageInputPolicy,
        expiry_cancel: Option<oneshot::Sender<()>>,
    ) -> Self {
        Self {
            render_generation,
            overlay_generation,
            status_message,
            status_frame,
            clear_frame,
            policy,
            ignored_sequence_prefix: Vec::new(),
            ignoring_bracketed_paste: false,
            ignored_terminal_string: None,
            ignored_utf8: None,
            _expiry_cancel: expiry_cancel,
        }
    }

    pub(in crate::handler) const fn overlay_generation(&self) -> u64 {
        self.overlay_generation
    }

    fn inherit_pending_prefix(&mut self, mut prefix: Vec<u8>) {
        if prefix.starts_with(BRACKETED_PASTE_START) {
            prefix.drain(..BRACKETED_PASTE_START.len());
            self.ignoring_bracketed_paste = true;
        }
        self.ignored_sequence_prefix = prefix;
    }

    fn consume_ignored_input(&mut self, pending_input: &mut Vec<u8>, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut input = std::mem::take(pending_input);
        input.extend_from_slice(bytes);
        let mut controls = Vec::new();
        for byte in input {
            if let Some(terminal_string) = self.ignored_terminal_string.as_mut() {
                if terminal_string.consume(byte) {
                    self.ignored_terminal_string = None;
                }
                continue;
            }
            if self.ignoring_bracketed_paste {
                self.ignored_sequence_prefix.push(byte);
                if self.ignored_sequence_prefix == BRACKETED_PASTE_END {
                    self.ignored_sequence_prefix.clear();
                    self.ignoring_bracketed_paste = false;
                } else {
                    retain_longest_candidate_prefix(
                        &mut self.ignored_sequence_prefix,
                        &[BRACKETED_PASTE_END],
                    );
                }
                continue;
            }
            if let Some(utf8) = self.ignored_utf8.as_mut() {
                if utf8.consume(byte) {
                    if utf8.remaining == 0 {
                        self.ignored_utf8 = None;
                    }
                    continue;
                }
                self.ignored_utf8 = None;
            }
            if let Some(utf8) = IgnoredUtf8::from_lead(byte) {
                self.ignored_sequence_prefix.clear();
                self.ignored_utf8 = Some(utf8);
                continue;
            }
            let new_input_at = self.ignored_sequence_prefix.len();
            self.ignored_sequence_prefix.push(byte);

            if self.ignored_sequence_prefix == BRACKETED_PASTE_START {
                self.ignored_sequence_prefix.clear();
                self.ignoring_bracketed_paste = true;
                continue;
            }

            let overlong = (self.ignored_sequence_prefix.starts_with(b"\x1b[")
                && self.ignored_sequence_prefix.len() > MAX_IGNORED_CSI_PREFIX)
                || (self.ignored_sequence_prefix.starts_with(b"\x1b]")
                    && self.ignored_sequence_prefix.len() > MAX_IGNORED_OSC_PREFIX);
            if overlong {
                self.ignored_sequence_prefix.clear();
                continue;
            }

            match decode_attached_terminal_control_after_append(
                &self.ignored_sequence_prefix,
                false,
                new_input_at,
            ) {
                TerminalResponseDecode::Partial => {}
                TerminalResponseDecode::PaneBound { .. }
                | TerminalResponseDecode::PaletteResponse { .. }
                | TerminalResponseDecode::ClipboardResponse { .. }
                | TerminalResponseDecode::Matched { .. } => {
                    controls.push(std::mem::take(&mut self.ignored_sequence_prefix));
                }
                TerminalResponseDecode::NotResponse => {
                    match decode_pane_bound_terminal_string(&self.ignored_sequence_prefix) {
                        PaneBoundTerminalStringDecode::Matched { .. } => {
                            self.ignored_sequence_prefix.clear();
                        }
                        PaneBoundTerminalStringDecode::Partial => {
                            self.ignored_terminal_string =
                                IgnoredTerminalString::from_prefix(&self.ignored_sequence_prefix);
                            if self.ignored_terminal_string.is_some() {
                                self.ignored_sequence_prefix.clear();
                            } else {
                                retain_possible_csi_suffix(&mut self.ignored_sequence_prefix);
                            }
                        }
                        PaneBoundTerminalStringDecode::NotString => {
                            retain_possible_csi_suffix(&mut self.ignored_sequence_prefix);
                        }
                    }
                }
            }
        }
        controls
    }

    fn take_pending_terminal_prefix(&mut self) -> Vec<u8> {
        if self.ignoring_bracketed_paste {
            self.ignoring_bracketed_paste = false;
            let mut pending = BRACKETED_PASTE_START.to_vec();
            pending.append(&mut self.ignored_sequence_prefix);
            return pending;
        }
        if self.ignored_terminal_string.take().is_some() {
            self.ignored_sequence_prefix.clear();
            return Vec::new();
        }
        self.ignored_utf8 = None;
        std::mem::take(&mut self.ignored_sequence_prefix)
    }
}

pub(in crate::handler) fn transient_message_render_snapshot(
    active: &ActiveAttach,
) -> Option<TransientMessageRenderSnapshot> {
    active
        .transient_message
        .as_ref()
        .map(|message| TransientMessageRenderSnapshot {
            overlay_generation: message.overlay_generation,
            status_message: message.status_message.clone(),
        })
}

pub(in crate::handler) fn compose_transient_message_refresh(
    active: &mut ActiveAttach,
    snapshot: Option<&TransientMessageRenderSnapshot>,
    rendered_status: Option<Vec<u8>>,
    frame: &mut Vec<u8>,
) {
    match (active.transient_message.as_mut(), snapshot, rendered_status) {
        (None, _, _) => {}
        (Some(current), Some(snapshot), Some(rendered_status))
            if current.overlay_generation == snapshot.overlay_generation
                && current.status_message == snapshot.status_message =>
        {
            current.status_frame.clone_from(&rendered_status);
            frame.extend_from_slice(&rendered_status);
        }
        (Some(current), _, _) => {
            frame.extend_from_slice(&current.status_frame);
        }
    }
}

pub(in crate::handler) fn append_transient_message_frame(
    active: &ActiveAttach,
    frame: &mut Vec<u8>,
) {
    if let Some(message) = active.transient_message.as_ref() {
        frame.extend_from_slice(&message.status_frame);
    }
}

pub(in crate::handler) fn cancel_transient_message(active: &mut ActiveAttach) {
    let Some(mut message) = active.transient_message.take() else {
        return;
    };
    active
        .transient_terminal_prefix
        .extend(message.take_pending_terminal_prefix());
    if !active.transient_terminal_prefix.is_empty() {
        let _ = active.control_tx.send(AttachControl::InteractiveInput);
    }
}

pub(in crate::handler) enum TransientMessageInput {
    Inactive,
    Consumed,
    TerminalControls(Vec<Vec<u8>>),
    Dismissed(Vec<u8>),
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_ignored_transient_message_input_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> TransientMessageInput {
        self.handle_transient_message_input_for_identity_inner(identity, pending_input, bytes, true)
            .await
    }

    pub(crate) async fn take_transient_terminal_prefix_for_identity(
        &self,
        identity: ActiveAttachIdentity,
    ) -> Vec<u8> {
        let mut active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get_mut(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .map(|active| std::mem::take(&mut active.transient_terminal_prefix))
            .unwrap_or_default()
    }

    pub(in crate::handler) async fn handle_transient_message_input_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> TransientMessageInput {
        self.handle_transient_message_input_for_identity_inner(
            identity,
            pending_input,
            bytes,
            false,
        )
        .await
    }

    async fn handle_transient_message_input_for_identity_inner(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
        ignored_only: bool,
    ) -> TransientMessageInput {
        let mut active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach
            .by_pid
            .get_mut(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
        else {
            return TransientMessageInput::Inactive;
        };
        let Some(message) = active.transient_message.as_mut() else {
            return TransientMessageInput::Inactive;
        };
        if ignored_only && message.policy != TransientMessageInputPolicy::IgnoreUntilExpiry {
            return TransientMessageInput::Inactive;
        }
        if message.policy == TransientMessageInputPolicy::IgnoreUntilExpiry {
            let controls = message.consume_ignored_input(pending_input, bytes);
            return if controls.is_empty() {
                TransientMessageInput::Consumed
            } else {
                TransientMessageInput::TerminalControls(controls)
            };
        }
        pending_input.extend_from_slice(bytes);

        let message = active
            .transient_message
            .take()
            .expect("transient message was checked above");
        let session_name = active.session_name.clone();
        let session_id = active.session_id;
        let restore_guard = TransientMessageRestoreGuard::new(active.overlay_generation);
        let control_tx = active.control_tx.clone();
        let _ = control_tx.send(AttachControl::Overlay(OverlayFrame::new(
            message.clear_frame,
            message.render_generation,
            message.overlay_generation,
        )));
        drop(active_attach);
        self.restore_attached_client_after_transient(
            identity,
            &session_name,
            session_id,
            restore_guard,
        )
        .await;
        TransientMessageInput::Dismissed(std::mem::take(pending_input))
    }

    pub(in crate::handler) async fn expire_transient_message_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        overlay_generation: u64,
    ) {
        let mut active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach
            .by_pid
            .get_mut(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    && active
                        .transient_message
                        .as_ref()
                        .is_some_and(|message| message.overlay_generation() == overlay_generation)
            })
        else {
            return;
        };
        let mut message = active
            .transient_message
            .take()
            .expect("transient message identity was checked above");
        active
            .transient_terminal_prefix
            .extend(message.take_pending_terminal_prefix());
        let session_name = active.session_name.clone();
        let session_id = active.session_id;
        let restore_guard = TransientMessageRestoreGuard::new(active.overlay_generation);
        let control_tx = active.control_tx.clone();
        let _ = control_tx.send(AttachControl::Overlay(OverlayFrame::new(
            message.clear_frame,
            message.render_generation,
            message.overlay_generation,
        )));
        if !active.transient_terminal_prefix.is_empty() {
            let _ = control_tx.send(AttachControl::InteractiveInput);
        }
        drop(active_attach);
        self.restore_attached_client_after_transient(
            identity,
            &session_name,
            session_id,
            restore_guard,
        )
        .await;
    }
}

pub(super) fn arm_transient_message(
    active: &mut ActiveAttach,
    status_message: String,
    status_frame: Vec<u8>,
    clear_frame: Vec<u8>,
    policy: TransientMessageInputPolicy,
    cancellable_expiry: bool,
) -> Option<oneshot::Receiver<()>> {
    let (expiry_cancel, expiry_cancelled) = cancellable_expiry
        .then(oneshot::channel)
        .map_or((None, None), |(sender, receiver)| {
            (Some(sender), Some(receiver))
        });
    let mut inherited_prefix = std::mem::take(&mut active.transient_terminal_prefix);
    let mut inherited_ignored_state = None;
    if let Some(mut previous) = active.transient_message.take() {
        if policy == TransientMessageInputPolicy::IgnoreUntilExpiry
            && previous.policy == TransientMessageInputPolicy::IgnoreUntilExpiry
        {
            inherited_prefix.append(&mut previous.ignored_sequence_prefix);
            inherited_ignored_state = Some((
                previous.ignoring_bracketed_paste,
                previous.ignored_terminal_string.take(),
                previous.ignored_utf8.take(),
            ));
        } else {
            inherited_prefix.extend(previous.take_pending_terminal_prefix());
        }
    }
    let mut message = TransientMessageInputState::new(
        active.render_generation,
        active.overlay_generation,
        status_message,
        status_frame,
        clear_frame,
        policy,
        expiry_cancel,
    );
    if policy == TransientMessageInputPolicy::IgnoreUntilExpiry {
        message.inherit_pending_prefix(inherited_prefix);
        if let Some((ignoring_bracketed_paste, ignored_terminal_string, ignored_utf8)) =
            inherited_ignored_state
        {
            message.ignoring_bracketed_paste = ignoring_bracketed_paste;
            message.ignored_terminal_string = ignored_terminal_string;
            message.ignored_utf8 = ignored_utf8;
        }
    } else {
        active.transient_terminal_prefix = inherited_prefix;
    }
    active.transient_message = Some(message);
    expiry_cancelled
}

pub(super) fn active_attach_identity(
    attach_pid: u32,
    active: &ActiveAttach,
) -> ActiveAttachIdentity {
    ActiveAttachIdentity::new(attach_pid, active.id, active.session_id)
}

fn retain_longest_candidate_prefix(prefix: &mut Vec<u8>, candidates: &[&[u8]]) {
    let keep_from = (0..prefix.len())
        .find(|start| {
            candidates
                .iter()
                .any(|candidate| candidate.starts_with(&prefix[*start..]))
        })
        .unwrap_or(prefix.len());
    if keep_from > 0 {
        prefix.drain(..keep_from);
    }
}

fn retain_possible_csi_suffix(prefix: &mut Vec<u8>) {
    let keep_from = prefix
        .iter()
        .rposition(|byte| *byte == b'\x1b')
        .filter(|start| {
            let suffix = &prefix[*start..];
            suffix == b"\x1b"
                || suffix == b"\x1b["
                || (suffix.starts_with(b"\x1b[")
                    && suffix[2..].iter().all(|byte| (0x20..=0x3f).contains(byte)))
        });
    match keep_from {
        Some(start) if start > 0 => {
            prefix.drain(..start);
        }
        Some(_) => {}
        None => prefix.clear(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        TransientMessageInputPolicy, TransientMessageInputState, BRACKETED_PASTE_END,
        BRACKETED_PASTE_START, DARK_THEME, FOCUS_IN, FOCUS_OUT, LIGHT_THEME,
    };

    fn ignored_state() -> TransientMessageInputState {
        TransientMessageInputState::new(
            1,
            1,
            "ignored".to_owned(),
            Vec::new(),
            Vec::new(),
            TransientMessageInputPolicy::IgnoreUntilExpiry,
            None,
        )
    }

    #[test]
    fn ignored_input_streams_focus_and_theme_controls_across_reads() {
        let mut state = ignored_state();
        let mut pending = Vec::new();
        assert!(state
            .consume_ignored_input(&mut pending, b"x\x1b")
            .is_empty());
        assert!(state.consume_ignored_input(&mut pending, b"[").is_empty());
        assert_eq!(
            state.consume_ignored_input(&mut pending, b"Iy\x1b[O"),
            vec![FOCUS_IN.to_vec(), FOCUS_OUT.to_vec()]
        );
        assert_eq!(
            state.consume_ignored_input(&mut pending, DARK_THEME),
            vec![DARK_THEME.to_vec()]
        );
        assert_eq!(
            state.consume_ignored_input(&mut pending, LIGHT_THEME),
            vec![LIGHT_THEME.to_vec()]
        );

        let palette = b"\x1b]4;7;rgb:1111/2222/3333\x1b\\";
        assert!(state
            .consume_ignored_input(&mut pending, &palette[..palette.len() - 1])
            .is_empty());
        assert_eq!(
            state.consume_ignored_input(&mut pending, &palette[palette.len() - 1..]),
            vec![palette.to_vec()]
        );

        let clipboard = b"\x1b]52;c;YQ==\x07";
        assert_eq!(
            state.consume_ignored_input(&mut pending, clipboard),
            vec![clipboard.to_vec()]
        );
    }

    #[test]
    fn ignored_input_does_not_decode_controls_inside_bracketed_paste() {
        let mut state = ignored_state();
        let mut pending = Vec::new();
        assert!(state
            .consume_ignored_input(&mut pending, b"\x1b[200~paste\x1b[")
            .is_empty());
        assert!(state
            .consume_ignored_input(&mut pending, b"I\x1b[201~")
            .is_empty());
        assert_eq!(
            state.consume_ignored_input(&mut pending, FOCUS_OUT),
            vec![FOCUS_OUT.to_vec()]
        );
    }

    #[test]
    fn ignored_input_keeps_generic_terminal_strings_opaque() {
        let mut state = ignored_state();
        let mut pending = Vec::new();
        let nested_clipboard = b"\x1b]52;c;bmVzdGVk\x07";
        let mut dcs = b"\x1bP1+rprefix=".to_vec();
        dcs.extend_from_slice("\u{041c}".as_bytes());
        dcs.extend_from_slice(nested_clipboard);
        dcs.extend_from_slice(b"suffix\x1b\\");
        let split = dcs.len() / 2;

        assert!(state
            .consume_ignored_input(&mut pending, &dcs[..split])
            .is_empty());
        assert!(state
            .consume_ignored_input(&mut pending, &dcs[split..])
            .is_empty());
        assert!(state.ignored_terminal_string.is_none());
        assert!(state.ignored_sequence_prefix.is_empty());

        assert_eq!(
            state.consume_ignored_input(&mut pending, nested_clipboard),
            vec![nested_clipboard.to_vec()],
            "a real outer response after the DCS remains independently visible"
        );

        let unknown_osc = b"\x1b]133;untrusted\x07";
        assert!(state
            .consume_ignored_input(&mut pending, &unknown_osc[..5])
            .is_empty());
        assert!(state
            .consume_ignored_input(&mut pending, &unknown_osc[5..])
            .is_empty());
    }

    #[test]
    fn ignored_utf8_never_becomes_a_raw_c1_terminal_string() {
        let mut state = ignored_state();
        let mut pending = Vec::new();
        assert!(state
            .consume_ignored_input(&mut pending, "\u{041d}".as_bytes())
            .is_empty());

        let clipboard = b"\x1b]52;c;YQ==\x07";
        assert_eq!(
            state.consume_ignored_input(&mut pending, clipboard),
            vec![clipboard.to_vec()]
        );
    }

    #[test]
    fn ignored_partial_terminal_string_is_abandoned_at_expiry() {
        let mut state = ignored_state();
        let mut pending = Vec::new();
        assert!(state
            .consume_ignored_input(&mut pending, b"\x1bPignored-before-expiry\x1b")
            .is_empty());
        assert!(
            state.take_pending_terminal_prefix().is_empty(),
            "tmux discards a terminal-string prefix received while -N is active"
        );
        assert!(state.ignored_terminal_string.is_none());
    }

    #[test]
    fn ignored_open_paste_transfers_every_partial_closer_at_expiry() {
        for split in 0..BRACKETED_PASTE_END.len() {
            let mut state = ignored_state();
            let mut pending = Vec::new();
            let mut before_expiry = BRACKETED_PASTE_START.to_vec();
            before_expiry.extend_from_slice(b"ignored");
            before_expiry.extend_from_slice(&BRACKETED_PASTE_END[..split]);
            assert!(state
                .consume_ignored_input(&mut pending, &before_expiry)
                .is_empty());

            let mut expected = BRACKETED_PASTE_START.to_vec();
            expected.extend_from_slice(&BRACKETED_PASTE_END[..split]);
            assert_eq!(state.take_pending_terminal_prefix(), expected);
        }
    }

    #[test]
    fn rearmed_ignore_policy_restores_an_expired_open_paste_boundary() {
        for split in 0..BRACKETED_PASTE_END.len() {
            let mut state = ignored_state();
            let mut inherited = BRACKETED_PASTE_START.to_vec();
            inherited.extend_from_slice(&BRACKETED_PASTE_END[..split]);
            state.inherit_pending_prefix(inherited);

            assert!(state.ignoring_bracketed_paste);
            assert_eq!(state.ignored_sequence_prefix, BRACKETED_PASTE_END[..split]);
            let mut pending = Vec::new();
            assert!(state
                .consume_ignored_input(&mut pending, &BRACKETED_PASTE_END[split..],)
                .is_empty());
            assert!(!state.ignoring_bracketed_paste);
            assert!(state.take_pending_terminal_prefix().is_empty());
        }
    }

    #[test]
    fn ignored_input_drops_an_overlong_csi_and_recovers_for_the_next_control() {
        let mut state = ignored_state();
        let mut pending = Vec::new();
        let mut overlong = b"\x1b[".to_vec();
        overlong.extend(std::iter::repeat_n(b'1', super::MAX_IGNORED_CSI_PREFIX + 1));

        assert!(state
            .consume_ignored_input(&mut pending, &overlong)
            .is_empty());
        assert!(state.ignored_sequence_prefix.is_empty());
        assert_eq!(
            state.consume_ignored_input(&mut pending, FOCUS_IN),
            vec![FOCUS_IN.to_vec()]
        );
    }
}
