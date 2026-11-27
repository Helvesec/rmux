//! Acquired keyboard events keep their semantics until the application sink.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::Ordering;

use rmux_core::kitty_keyboard::{AcquiredDecoder, AcquiredItem, Event, Key, Kind};
use rmux_proto::{PaneId, PaneTarget, SessionId, WindowId};

use super::super::pane_io_encoding::{
    encode_key_for_target, pane_id_for_input_target, prepare_pane_input_write,
    synchronized_input_targets, PaneInputLiveness,
};
use super::synchronized::{write_prepared_attached_pane_forwards, PreparedAttachedPaneForward};
use super::{io_other, ActiveAttachIdentity, AttachedKeyDispatch, ClientFlags, RequestHandler};
use crate::handler::attach_support::FrameCursor;
use crate::pane_terminals::HandlerState;

const HELD_LIMIT: usize = 256;

#[derive(Debug, Clone)]
pub(in crate::handler) struct HeldPane {
    session: SessionId,
    window: WindowId,
    pane: PaneId,
    generation: u64,
}

pub(in crate::handler) type HeldKeys = HashMap<Key, Vec<HeldPane>>;

pub(super) enum Input {
    Bytes(Vec<u8>),
    Event(Event),
    Pending(Vec<u8>),
}

/// Scan only at the input admission boundary, before bindings or modal input.
/// Foreign strings and paste bodies remain opaque to the portable decoder.
pub(super) fn split_input(pending: &[u8], bytes: &[u8]) -> Option<Vec<Input>> {
    if !pending.contains(&27) && !bytes.contains(&27) {
        return None;
    }
    let stream = [pending, bytes].concat();
    let mut decoder = AcquiredDecoder::new();
    let mut inputs = Vec::new();
    let mut changed = false;
    // Offset in `stream` of the next decoded item.
    let mut offset = 0;
    let mut emit = |item: AcquiredItem<'_>| match item {
        AcquiredItem::Other(bytes) => {
            append_bytes(&mut inputs, bytes);
            offset += bytes.len();
        }
        AcquiredItem::Event(event) => {
            let end = event_frame_end(&stream, offset);
            if is_escape_prefixed_legacy_key(&stream, offset, end) {
                append_bytes(&mut inputs, &stream[offset..end]);
            } else {
                changed = true;
                inputs.push(Input::Event(event));
            }
            offset = end;
        }
        AcquiredItem::Rejected(rejection) => {
            changed = true;
            offset += rejection.bytes;
        }
    };
    decoder.feed(&stream, &mut emit);
    let tail = decoder.retained();
    if decoder.has_pending_keyboard_frame() {
        changed = true;
        let mut pending = tail.to_vec();
        if decoder.pending_len() > tail.len() {
            // One sentinel retains the over-limit classification across reads,
            // while the complete candidate is discarded at its final byte.
            pending.push(b'0');
        }
        inputs.push(Input::Pending(pending));
    } else {
        append_bytes(&mut inputs, tail);
    }
    changed.then_some(inputs)
}

pub(super) fn keyboard_prefix(bytes: &[u8]) -> bool {
    // Timeout follows the same framing authority as admission. In particular,
    // a retained invalid candidate is not a legacy key or a paste delimiter.
    let mut decoder = AcquiredDecoder::new();
    decoder.feed(bytes, |_| {});
    decoder.has_pending_keyboard_frame()
}

/// An acquired event frame is `ESC [`, parameters, and its first final byte.
fn event_frame_end(stream: &[u8], start: usize) -> usize {
    stream
        .get(start + 2..)
        .and_then(|rest| rest.iter().position(|byte| (0x40..=0x7e).contains(byte)))
        .map_or(stream.len(), |final_index| start + 2 + final_index + 1)
}

/// A lone Escape just before a key frame is a Meta prefix typed by a legacy
/// terminal, never a Kitty event: the frame stays with its Escape, which ends
/// the preceding byte run, so that the legacy decoder reads both in order, as
/// tmux 3.7b does.
fn is_escape_prefixed_legacy_key(stream: &[u8], start: usize, end: usize) -> bool {
    let Some(escape) = start.checked_sub(1) else {
        return false;
    };
    stream[escape] == b'\x1b'
        && matches!(
            crate::key_table::decode_attached_key(&stream[escape..end], None),
            crate::key_table::AttachedKeyDecode::Matched { size, .. }
                if size == 1 || size == end - escape
        )
}

fn append_bytes(inputs: &mut Vec<Input>, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    if let Some(Input::Bytes(previous)) = inputs.last_mut() {
        previous.extend_from_slice(bytes);
    } else {
        inputs.push(Input::Bytes(bytes.to_vec()));
    }
}

/// Only project modifiers and identities that the legacy binding model owns.
fn binding_key(event: &Event) -> Option<rmux_core::KeyCode> {
    use rmux_core::{
        key_string_lookup_string, KEYC_CTRL, KEYC_IMPLIED_META, KEYC_META, KEYC_SHIFT,
    };
    if event.modifiers & !(7 | 64 | 128) != 0 {
        return None;
    }
    let mut shift_consumed = false;
    let mut key = match event.key {
        Key::Scalar(scalar) => {
            let shifted = (event.modifiers & 1 != 0 && !scalar.is_control())
                .then(|| {
                    event.shifted.or_else(|| {
                        // Keep explicit Ctrl+Shift chords without an alternate;
                        // ordinary ASCII letters have the existing legacy case.
                        (event.modifiers & 4 == 0 && scalar.is_ascii_lowercase())
                            .then(|| scalar.to_ascii_uppercase())
                    })
                })
                .flatten();
            shift_consumed = shifted.is_some();
            shifted.unwrap_or(scalar) as u64
        }
        Key::Function(function) => {
            let name = match function.name() {
                "ENTER" => "Enter",
                "TAB" => "Tab",
                "ESCAPE" => "Escape",
                "BACKSPACE" => "BSpace",
                "UP" => "Up",
                "DOWN" => "Down",
                "LEFT" => "Left",
                "RIGHT" => "Right",
                "HOME" => "Home",
                "END" => "End",
                "INSERT" => "IC",
                "DELETE" => "DC",
                "PAGE_UP" => "PPage",
                "PAGE_DOWN" => "NPage",
                name if name.starts_with('F') => name,
                _ => return None,
            };
            key_string_lookup_string(name)?
        }
        _ => return None,
    };
    // A supplied shifted character already includes Shift in the identity
    // used by legacy bindings. Never modify the original protocol event.
    if event.modifiers & 1 != 0 && !shift_consumed {
        if key == u64::from(b'\t') {
            key = key_string_lookup_string("BTab")?;
        } else {
            key |= KEYC_SHIFT;
        }
    }
    // Alt carried by the key's own sequence is implied Meta, as tmux 3.7b
    // decodes a modifier parameter (`tty_keys_extended_key`); only an Escape
    // typed before a key is an explicit Meta.
    if event.modifiers & 2 != 0 {
        key |= KEYC_META | KEYC_IMPLIED_META;
    }
    if event.modifiers & 4 != 0 {
        key |= KEYC_CTRL;
    }
    Some(key)
}

fn encode(state: &HandlerState, target: &PaneTarget, event: &Event) -> io::Result<Vec<u8>> {
    let flags = state
        .transcript_handle(target)
        .map_err(io_other)?
        .lock()
        .keyboard_flags();
    if flags & !16 != 0 {
        return Ok(rmux_core::kitty_keyboard::encode_event(event, flags).unwrap_or_default());
    }
    if event.kind == Kind::Release {
        return Ok(Vec::new());
    }
    if !event.text.values().is_empty() {
        return Ok(event.text.values().iter().collect::<String>().into_bytes());
    }
    binding_key(event)
        .map(|key| encode_key_for_target(state, target, key).map_err(io_other))
        .transpose()
        .map(|bytes| bytes.flatten().unwrap_or_default())
}

impl HeldPane {
    fn capture(state: &HandlerState, target: &PaneTarget) -> io::Result<Self> {
        let session = state
            .sessions
            .session(target.session_name())
            .ok_or_else(|| io_other("missing session"))?;
        let window = session
            .window_at(target.window_index())
            .ok_or_else(|| io_other("missing window"))?;
        Ok(Self {
            session: session.id(),
            window: window.id(),
            pane: pane_id_for_input_target(state, target).map_err(io_other)?,
            generation: state.pane_output_generation_for_target(
                target,
                pane_id_for_input_target(state, target).map_err(io_other)?,
            ),
        })
    }

    fn resolve(&self, state: &HandlerState) -> Option<PaneTarget> {
        let session = state.sessions.session_by_id(self.session)?;
        let target = session.windows().find_map(|(index, window)| {
            (window.id() == self.window)
                .then(|| {
                    window
                        .panes()
                        .iter()
                        .find(|pane| pane.id() == self.pane)
                        .map(|pane| {
                            PaneTarget::with_window(session.name().clone(), index, pane.index())
                        })
                })
                .flatten()
        })?;
        (state.pane_output_generation_for_target(&target, self.pane) == self.generation)
            .then_some(target)
    }
}

impl RequestHandler {
    pub(super) async fn handle_acquired_keyboard_event(
        &self,
        identity: ActiveAttachIdentity,
        event: Event,
        native: Option<rmux_proto::AttachedWindowsConsoleKey>,
        active_emit_cache: &mut super::live::ActiveClientEmitCache,
    ) -> io::Result<bool> {
        if self
            .attached_client_input_is_read_only_for_identity(identity)
            .await?
        {
            self.remember_keyboard_press(identity, event.key, Vec::new())
                .await;
            if event.kind == Kind::Press {
                self.consume_keyboard_event_as_legacy(identity, &event)
                    .await?;
            }
            return Ok(false);
        }
        if event.kind != Kind::Press {
            return self
                .forward_held_keyboard_event(identity, &event, native)
                .await;
        }
        let (target, session_id) = self
            .attached_input_target_identity(identity)
            .await
            .map_err(io_other)?;
        // Capture the route before a binding can select another pane. An empty
        // route also remembers a consumed press, suppressing its remainders.
        let dismissal = self
            .capture_key_dismissal(identity, &target, session_id)
            .await;
        let routes = {
            let state = self.state.lock().await;
            synchronized_input_targets(&state, &target)
                .map_err(io_other)?
                .iter()
                .map(|target| HeldPane::capture(&state, target))
                .collect::<io::Result<Vec<_>>>()?
        };
        self.remember_keyboard_press(identity, event.key, Vec::new())
            .await;
        let key = binding_key(&event);
        if self
            .attached_modal_surface_takes_input_for_identity(identity)
            .await
            || self
                .target_is_in_clock_mode(&target)
                .await
                .map_err(io_other)?
        {
            self.consume_keyboard_event_as_legacy(identity, &event)
                .await?;
            return Ok(false);
        }
        self.clear_attached_focus_alerts(identity).await;
        self.emit_attached_client_active_if_changed(identity, &target, active_emit_cache)
            .await;
        if let Some(key) = key {
            // Preserve an explicit Ctrl+Shift chord even when the optional
            // shifted identity also projects to a different legacy binding.
            // Encoding and held-key identity continue to use the original event.
            let primary_key = (event.modifiers & 5 == 5 && event.shifted.is_some())
                .then(|| {
                    binding_key(&Event {
                        shifted: None,
                        ..event.clone()
                    })
                })
                .flatten()
                .filter(|primary| *primary != key);
            if self
                .dispatch_attached_key_with_primary(
                    &target,
                    AttachedKeyDispatch {
                        attach_pid: identity.attach_pid(),
                        live_identity: Some(identity),
                        live_session_id: Some(session_id),
                        requester_pid: identity.attach_pid(),
                        current_target: Some(rmux_proto::Target::Pane(target.clone())),
                        mouse_target: None,
                        mouse_event: None,
                        key,
                        attached_live_input: true,
                    },
                    primary_key,
                )
                .await
                .map_err(io_other)?
            {
                return Ok(false);
            }
        } else if self
            .target_is_in_copy_mode(&target)
            .await
            .map_err(io_other)?
        {
            return Ok(false);
        }
        if let Some(intent) = dismissal {
            self.dismiss_kept_pane_by_key(intent).await?;
            return Ok(false);
        }
        self.remember_keyboard_press(identity, event.key, routes)
            .await;
        self.forward_held_keyboard_event(identity, &event, native)
            .await
    }

    /// Whether the escape time discards this retained keyboard frame. The
    /// prompt and mode-tree flush branches read it instead, as tmux 3.7b's
    /// `tty_keys_next` does once the timer has expired: Meta of its next
    /// character, then the rest as typed (`ESC [6;2` leaves `6;2`). Every
    /// other surface keeps the candidate quarantined.
    pub(super) async fn timed_out_keyboard_frame_is_quarantined(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &[u8],
    ) -> bool {
        if !keyboard_prefix(pending_input) {
            return false;
        }
        let read_by_a_key_reader = pending_input.first() == Some(&b'\x1b')
            && ((super::input_arrives_now(identity)
                && self.prompt_active_for_identity(identity).await)
                || self.mode_tree_active_for_identity(identity).await);
        !read_by_a_key_reader
    }

    async fn consume_keyboard_event_as_legacy(
        &self,
        identity: ActiveAttachIdentity,
        event: &Event,
    ) -> io::Result<()> {
        let key = binding_key(event);
        let bytes = if event.modifiers & 6 == 0 && !event.text.values().is_empty() {
            event.text.values().iter().collect::<String>().into_bytes()
        } else if let Some(key) = key {
            crate::input_keys::encode_key(0, crate::input_keys::ExtendedKeyFormat::Xterm, key)
                .unwrap_or_default()
        } else {
            event.text.values().iter().collect::<String>().into_bytes()
        };
        let mut pending = Vec::new();
        let mut cache = Default::default();
        Box::pin(self.handle_attached_keyboard_legacy_input(
            identity,
            &mut pending,
            if key == Some(crate::input_keys::SHIFT_ENTER) && bytes == b"\r" {
                super::live::LegacyInput::ShiftEnter
            } else {
                super::live::LegacyInput::Bytes(&bytes)
            },
            None,
            &mut FrameCursor::new(true),
            &mut cache,
        ))
        .await?;
        if !pending.is_empty() {
            // A decoded Escape is already a complete event, not the start of
            // a possibly fragmented legacy escape sequence.
            Box::pin(self.flush_attached_pending_escape_input_for_identity(identity, &mut pending))
                .await?;
        }
        Ok(())
    }

    async fn remember_keyboard_press(
        &self,
        identity: ActiveAttachIdentity,
        key: Key,
        routes: Vec<HeldPane>,
    ) {
        let mut attaches = self.active_attach.lock().await;
        if let Some(active) = attaches
            .by_pid
            .get_mut(&identity.attach_pid())
            .filter(|active| identity.matches_active(active))
        {
            if active.held_keyboard.len() >= HELD_LIMIT && !active.held_keyboard.contains_key(&key)
            {
                active.held_keyboard.clear();
            }
            active.held_keyboard.insert(key, routes);
        }
    }

    async fn forward_held_keyboard_event(
        &self,
        identity: ActiveAttachIdentity,
        event: &Event,
        native: Option<rmux_proto::AttachedWindowsConsoleKey>,
    ) -> io::Result<bool> {
        #[cfg(not(windows))]
        let _ = native;
        let prepared = {
            let mut state = self.state.lock().await;
            let mut attaches = self.active_attach.lock().await;
            let Some(active) = attaches
                .by_pid
                .get_mut(&identity.attach_pid())
                .filter(|active| {
                    identity.matches_active(active)
                        && !active.closing.load(Ordering::SeqCst)
                        && active.can_write
                        && !active.flags.contains(ClientFlags::READONLY)
                })
            else {
                return Ok(false);
            };
            let routes = if event.kind == Kind::Release {
                active.held_keyboard.remove(&event.key)
            } else {
                active.held_keyboard.get(&event.key).cloned()
            }
            .unwrap_or_default();
            let mut prepared = Vec::new();
            for route in routes {
                let Some(target) = route.resolve(&state) else {
                    continue;
                };
                let bytes = encode(&state, &target, event)?;
                #[cfg(windows)]
                if event.kind != Kind::Release {
                    if let (Some(native), Some(key)) = (native, binding_key(event)) {
                        use super::super::pane_io_encoding::{
                            prepare_pane_console_input_write,
                            should_emulate_windows_cmd_select_all,
                            should_route_windows_control_as_pty_bytes,
                            windows_console_input_for_attached_key,
                        };
                        let flags = state
                            .transcript_handle(&target)
                            .map_err(io_other)?
                            .lock()
                            .keyboard_flags();
                        if flags & !16 == 0
                            && !should_emulate_windows_cmd_select_all(&state, &target, key)
                            && !should_route_windows_control_as_pty_bytes(&state, &target, key)
                        {
                            let native = super::live::windows_console_key_event(native);
                            let action = windows_console_input_for_attached_key(
                                &state, &target, key, native,
                            );
                            let write = prepare_pane_console_input_write(
                                &mut state, &target, &bytes, action,
                            )
                            .map_err(io_other)?;
                            prepared.push(PreparedAttachedPaneForward::WindowsConsoleKey {
                                write,
                                action,
                            });
                            continue;
                        }
                    }
                }
                if !bytes.is_empty() {
                    let write = prepare_pane_input_write(
                        &mut state,
                        &target,
                        &bytes,
                        PaneInputLiveness::TolerateDead,
                    )
                    .map_err(io_other)?;
                    prepared.push(PreparedAttachedPaneForward::EncodedKey { write, bytes });
                }
            }
            prepared
        };
        let forwarded = !prepared.is_empty();
        write_prepared_attached_pane_forwards(prepared)
            .await
            .map_err(io_other)?;
        Ok(forwarded)
    }
}

#[cfg(test)]
mod tests {
    use super::{binding_key, split_input, Input};
    use crate::input_keys::{encode_key, ExtendedKeyFormat};
    use rmux_core::kitty_keyboard::parse_event_frame;

    #[test]
    fn escape_before_a_key_frame_stays_legacy_input_with_its_key() {
        for (pending, bytes) in [
            (b"".as_slice(), b"\x1b\x1b[15~".as_slice()),
            (b"", b"\x1b\x1b[A"),
            (b"", b"\x1b\x1b[3;5~"),
            (b"", b"\x1b\x1b[1;3A"),
            (b"", b"a\x1b\x1b[15~"),
            (b"\x1b\x1b[1", b"5~"),
        ] {
            assert!(
                split_input(pending, bytes).is_none(),
                "{pending:?} + {bytes:?} must reach the legacy decoder as one byte run"
            );
        }
    }

    #[test]
    fn key_frames_without_an_escape_prefix_stay_acquired_events() {
        for (bytes, before) in [
            (b"x\x1b[15~".as_slice(), b"x".as_slice()),
            (b"\x1ba\x1b[A", b"\x1ba"),
        ] {
            let inputs = split_input(&[], bytes).expect("a key frame is an event");
            assert!(
                matches!(inputs.as_slice(), [Input::Bytes(run), Input::Event(_)] if run == before),
                "{bytes:?}"
            );
        }
        let inputs = split_input(&[], b"\x1b\x1b[15~\x1b[A").expect("second frame is an event");
        assert!(matches!(
            inputs.as_slice(),
            [Input::Bytes(run), Input::Event(_)] if run == b"\x1b\x1b[15~"
        ));
    }

    /// Divergence without an oracle (tmux 3.7b has no Kitty keyboard
    /// protocol): Alt carried by the key's own frame is implied Meta, so a
    /// Kitty Alt+F5 keeps the embedded parameter instead of the Escape
    /// prefix tmux writes for an Escape typed before F5.
    #[test]
    fn frame_alt_is_implied_meta_and_keeps_the_embedded_parameter() {
        for (frame, expected) in [
            (b"\x1b[15;3~".as_slice(), b"\x1b[15;3~".as_slice()),
            (b"\x1b[3;3~", b"\x1b[3;3~"),
            (b"\x1b[1;3A", b"\x1b[1;3A"),
            (b"\x1b[97;3u", b"\x1ba"),
        ] {
            let event = parse_event_frame(frame).expect("Alt frame parses");
            let key = binding_key(&event).expect("legacy key identity");
            assert_eq!(
                encode_key(0, ExtendedKeyFormat::Xterm, key).as_deref(),
                Some(expected),
                "{frame:?}"
            );
        }
    }
}
