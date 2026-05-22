//! Inert input vocabulary for SDK consumers.
//!
//! This module is the public SDK home for the structured key event DTOs
//! that callers exchange with `rmux-client`/`rmux-server` style attach
//! integrations. The types here are deliberately framework-agnostic value
//! objects: they do not own a terminal, do not subscribe to keyboard
//! sources, and never sleep on a clock.
//!
//! `rmux-sdk` users obtain the entire input vocabulary through the
//! `rmux_sdk` re-exports without depending on `rmux-core`,
//! `rmux-server`, `rmux-client`, or `rmux-pty`. The detach chord detector
//! is deterministic by construction: every state transition is driven by
//! caller-supplied [`std::time::Instant`] timestamps, so unit tests can
//! exercise prefix-held, mismatch-forward, chord-success, and timeout
//! behaviour without sleeping or touching a real keyboard.
//!
//! When the optional `crossterm` SDK feature is enabled, the module
//! gains lossless conversions from `crossterm::event::KeyEvent` /
//! `KeyCode` / `KeyModifiers` so SDK consumers can adapt a
//! crossterm-driven input loop without leaking that dependency through
//! the default workspace build.

use std::time::{Duration, Instant};

use serde::{Deserialize, Deserializer, Serialize};

/// Modifier flags that may accompany an SDK [`KeyEvent`].
///
/// The bitfield mirrors the modifiers that survive tmux-compatible attach
/// translation (Shift, Control, Alt, Super, Hyper, Meta) without adopting
/// any single host library's encoding. Unknown bits are rejected at
/// construction time so deserialized values cannot smuggle reserved bits
/// through the SDK boundary.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct KeyModifiers {
    bits: u8,
}

impl<'de> Deserialize<'de> for KeyModifiers {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bits = u8::deserialize(deserializer)?;
        Self::from_bits(bits).ok_or_else(|| {
            serde::de::Error::custom(format_args!(
                "KeyModifiers value {bits:#010b} sets bits outside the valid mask {:#010b}",
                Self::VALID_MASK
            ))
        })
    }
}

impl KeyModifiers {
    /// No modifiers held.
    pub const NONE: Self = Self { bits: 0 };
    /// Shift modifier flag.
    pub const SHIFT: Self = Self { bits: 0b0000_0001 };
    /// Control modifier flag.
    pub const CONTROL: Self = Self { bits: 0b0000_0010 };
    /// Alt (Option on macOS) modifier flag.
    pub const ALT: Self = Self { bits: 0b0000_0100 };
    /// Super (Command/Windows) modifier flag.
    pub const SUPER: Self = Self { bits: 0b0000_1000 };
    /// Hyper modifier flag.
    pub const HYPER: Self = Self { bits: 0b0001_0000 };
    /// Meta modifier flag.
    pub const META: Self = Self { bits: 0b0010_0000 };

    const VALID_MASK: u8 = 0b0011_1111;

    /// Returns the empty modifier set.
    #[must_use]
    pub const fn empty() -> Self {
        Self::NONE
    }

    /// Returns the raw bitfield representation.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.bits
    }

    /// Constructs modifiers from a bitfield, rejecting reserved bits.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Option<Self> {
        if (bits & !Self::VALID_MASK) == 0 {
            Some(Self { bits })
        } else {
            None
        }
    }

    /// Constructs modifiers from a bitfield, dropping any reserved bits.
    #[must_use]
    pub const fn from_bits_truncate(bits: u8) -> Self {
        Self {
            bits: bits & Self::VALID_MASK,
        }
    }

    /// Returns `true` when this set is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.bits == 0
    }

    /// Returns `true` when every bit in `other` is also set in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.bits & other.bits) == other.bits
    }

    /// Returns the union of `self` and `other`.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self {
            bits: self.bits | other.bits,
        }
    }

    /// Returns the intersection of `self` and `other`.
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self {
            bits: self.bits & other.bits,
        }
    }

    /// Returns the symmetric difference of `self` and `other`.
    #[must_use]
    pub const fn symmetric_difference(self, other: Self) -> Self {
        Self {
            bits: self.bits ^ other.bits,
        }
    }
}

impl std::ops::BitOr for KeyModifiers {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl std::ops::BitAnd for KeyModifiers {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self {
        self.intersection(rhs)
    }
}

impl std::ops::BitXor for KeyModifiers {
    type Output = Self;

    fn bitxor(self, rhs: Self) -> Self {
        self.symmetric_difference(rhs)
    }
}

/// Structured key code carried by an SDK [`KeyEvent`].
///
/// The variants cover the keys the SDK promises to forward across the
/// attach boundary. Variants that depend on platform-specific keyboard
/// enhancements (media keys, scroll lock, lock-state reporting) are
/// intentionally collapsed into the generic [`KeyCode::Char`] /
/// [`KeyCode::F`] surface so SDK users do not branch on host
/// idiosyncrasies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum KeyCode {
    /// Unicode character key (lower-case form unless Shift is set).
    Char(char),
    /// Function key, F1..=F35.
    F(u8),
    /// Backspace key.
    Backspace,
    /// Enter / Return key.
    Enter,
    /// Left arrow key.
    Left,
    /// Right arrow key.
    Right,
    /// Up arrow key.
    Up,
    /// Down arrow key.
    Down,
    /// Home key.
    Home,
    /// End key.
    End,
    /// Page up key.
    PageUp,
    /// Page down key.
    PageDown,
    /// Tab key.
    Tab,
    /// Shift+Tab / back-tab key.
    BackTab,
    /// Delete key.
    Delete,
    /// Insert key.
    Insert,
    /// Escape key.
    Esc,
}

/// SDK-facing key event DTO.
///
/// `KeyEvent` is intentionally inert: constructing one does not arm a
/// detector, push a frame, or open a daemon connection. SDK consumers
/// build these from their own keyboard source (or via the optional
/// `crossterm` feature) and feed them into helpers like
/// [`DetachDetector::feed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyEvent {
    /// Logical key code.
    pub code: KeyCode,
    /// Active modifier flags when the key was reported.
    pub modifiers: KeyModifiers,
}

impl KeyEvent {
    /// Constructs an event from a code and modifier set.
    #[must_use]
    pub const fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    /// Constructs a modifier-free event from a code.
    #[must_use]
    pub const fn bare(code: KeyCode) -> Self {
        Self::new(code, KeyModifiers::NONE)
    }

    /// Constructs a `Ctrl+`-modified character event.
    #[must_use]
    pub const fn ctrl(ch: char) -> Self {
        Self::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
    }
}

/// Two-key sequence that requests a client detach.
///
/// `prefix` is the leader key (typically `Ctrl+B`) and `detach` is the
/// follow-up key (typically `d`). Equality semantics for both fields are
/// the SDK [`KeyEvent`] equality, so an event matches a slot only when
/// both code and modifier set agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DetachChord {
    /// Leader key event that arms the detector.
    pub prefix: KeyEvent,
    /// Follow-up key event that triggers detach when seen after the prefix.
    pub detach: KeyEvent,
}

impl DetachChord {
    /// The tmux-default `Ctrl+B`, `d` chord.
    #[must_use]
    pub const fn tmux_default() -> Self {
        Self {
            prefix: KeyEvent::ctrl('b'),
            detach: KeyEvent::bare(KeyCode::Char('d')),
        }
    }

    /// Constructs a chord from explicit prefix/detach events.
    #[must_use]
    pub const fn new(prefix: KeyEvent, detach: KeyEvent) -> Self {
        Self { prefix, detach }
    }
}

/// Outcome of a single [`DetachDetector::feed`] or
/// [`DetachDetector::tick`] call.
///
/// `Forward` carries the events the host should forward to the attached
/// pane. `Armed` indicates the detector has consumed the prefix and is
/// waiting for the follow-up key inside the timeout window.
/// `DetachRequested` indicates the chord matched and the host should
/// invoke its own explicit detach action; the detector itself never
/// performs side effects on the host's behalf.
///
/// `DetachRequested` is purely a signal. The detector returns the
/// chord-completion verdict; the host owns whether (and how) to actually
/// detach the attached client. A host that ignores `DetachRequested`
/// observes no further state from the detector — the detector has
/// already returned to idle and is ready for a fresh chord cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetachOutcome {
    /// Forward this exact list of events to the attached pane.
    Forward(Vec<KeyEvent>),
    /// Detector swallowed the prefix and is waiting for the follow-up.
    Armed,
    /// Chord matched; host should perform the detach action.
    DetachRequested,
}

/// Internal detector state, kept private so the only mutation paths are
/// the public `feed`/`tick`/`reset` methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetectorState {
    Idle,
    PrefixHeld { since: Instant },
}

/// Deterministic detach-chord detector.
///
/// `feed` and `tick` accept caller-supplied timestamps so unit tests can
/// drive every state transition without sleeping. The detector is purely
/// a state machine: it never spawns threads, never reads from a terminal,
/// and never owns a clock of its own.
///
/// # Contract
///
/// The detector's behaviour is fully specified by the following rules:
///
/// 1. **Strict code+modifier equality.** A key matches the chord's
///    `prefix` (or `detach`) slot only when both [`KeyCode`] and the
///    full [`KeyModifiers`] bitfield are byte-for-byte equal to the
///    configured event. `Ctrl+B` does not match `Ctrl+Shift+B`.
/// 2. **Prefix swallowing.** While idle, observing the prefix transitions
///    the detector to `PrefixHeld` and returns
///    [`DetachOutcome::Armed`]; the prefix is consumed and is *not*
///    forwarded to the pane until the timeout lapses or a mismatch is
///    seen.
/// 3. **Chord completion.** While `PrefixHeld`, observing the detach
///    follow-up returns [`DetachOutcome::DetachRequested`] and the
///    detector returns to idle without forwarding anything.
/// 4. **Mismatch forwarding.** While `PrefixHeld`, observing any other
///    event (including the prefix again) returns
///    `DetachOutcome::Forward(vec![prefix, event])` in that order, and
///    the detector returns to idle.
/// 5. **Timeout flushing.** A `feed` or [`tick`](Self::tick) call where
///    `now.saturating_duration_since(prefix_arrival) >= timeout` flushes
///    the held prefix as `Forward(vec![prefix])` and returns the
///    detector to idle. For [`feed`](Self::feed), the new event is then
///    processed against the now-idle detector and any extra forwarded
///    events are appended after the flushed prefix.
/// 6. **Zero-timeout edge case.** A `Duration::ZERO` timeout means any
///    observation strictly after the prefix is treated as expired
///    (`>=` is the comparison): the detector flushes the prefix and
///    forwards the new event without ever firing the chord. Hosts that
///    want chord behaviour must configure a non-zero timeout.
/// 7. **Equal prefix/detach edge case.** If a chord is configured with
///    `prefix == detach`, the detach branch is checked first while
///    `PrefixHeld`, so pressing the shared key twice quickly enough
///    returns `DetachRequested`.
/// 8. **Reusability.** The detector is fully reusable after every
///    terminal outcome: hosts may keep a single detector across
///    sessions or runs. After `DetachRequested`, the detector is idle
///    and a subsequent `tick` returns `Forward(vec![])`.
#[derive(Debug, Clone)]
pub struct DetachDetector {
    chord: DetachChord,
    timeout: Duration,
    state: DetectorState,
}

impl DetachDetector {
    /// Default chord-completion window matching tmux's interactive feel.
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(1_000);

    /// Constructs a detector for the given chord with [`Self::DEFAULT_TIMEOUT`].
    #[must_use]
    pub const fn new(chord: DetachChord) -> Self {
        Self::with_timeout(chord, Self::DEFAULT_TIMEOUT)
    }

    /// Constructs a detector with an explicit timeout window.
    #[must_use]
    pub const fn with_timeout(chord: DetachChord, timeout: Duration) -> Self {
        Self {
            chord,
            timeout,
            state: DetectorState::Idle,
        }
    }

    /// Returns the chord this detector matches.
    #[must_use]
    pub const fn chord(&self) -> &DetachChord {
        &self.chord
    }

    /// Returns the configured chord-completion timeout.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Returns `true` while the detector has consumed the prefix and is
    /// waiting for the follow-up key.
    #[must_use]
    pub const fn is_prefix_armed(&self) -> bool {
        matches!(self.state, DetectorState::PrefixHeld { .. })
    }

    /// Resets the detector back to idle without forwarding anything.
    pub fn reset(&mut self) {
        self.state = DetectorState::Idle;
    }

    /// Feeds an event into the detector and returns the outcome.
    ///
    /// `now` is the timestamp at which the event is observed. Tests pass
    /// a deterministic `Instant` so timeout edges can be exercised
    /// precisely. The detector never blocks and never reads `Instant::now()`
    /// internally.
    #[must_use]
    pub fn feed(&mut self, event: KeyEvent, now: Instant) -> DetachOutcome {
        if let DetectorState::PrefixHeld { since } = self.state
            && now.saturating_duration_since(since) >= self.timeout {
                self.state = DetectorState::Idle;
                let mut forwarded = vec![self.chord.prefix];
                match self.process_idle(event, now) {
                    DetachOutcome::Forward(extra) => forwarded.extend(extra),
                    // `process_idle` re-armed on the new prefix and produced
                    // no extra output; the caller still observes the flushed
                    // expired prefix.
                    DetachOutcome::Armed => {}
                    DetachOutcome::DetachRequested => {
                        unreachable!("process_idle never returns DetachRequested from idle state",)
                    }
                }
                return DetachOutcome::Forward(forwarded);
            }

        match self.state {
            DetectorState::Idle => self.process_idle(event, now),
            DetectorState::PrefixHeld { .. } => self.process_prefix_held(event),
        }
    }

    /// Advances the detector's clock without consuming an input event.
    ///
    /// Hosts call this when they receive a non-key wakeup (poll loop tick,
    /// resize event, etc.) so the detector can release a held prefix once
    /// the timeout has lapsed. Returns `Forward(vec![prefix])` when the
    /// timeout has elapsed; otherwise returns the current state.
    #[must_use]
    pub fn tick(&mut self, now: Instant) -> DetachOutcome {
        match self.state {
            DetectorState::Idle => DetachOutcome::Forward(Vec::new()),
            DetectorState::PrefixHeld { since } => {
                if now.saturating_duration_since(since) >= self.timeout {
                    self.state = DetectorState::Idle;
                    DetachOutcome::Forward(vec![self.chord.prefix])
                } else {
                    DetachOutcome::Armed
                }
            }
        }
    }

    fn process_idle(&mut self, event: KeyEvent, now: Instant) -> DetachOutcome {
        if event == self.chord.prefix {
            self.state = DetectorState::PrefixHeld { since: now };
            DetachOutcome::Armed
        } else {
            DetachOutcome::Forward(vec![event])
        }
    }

    fn process_prefix_held(&mut self, event: KeyEvent) -> DetachOutcome {
        if event == self.chord.detach {
            self.state = DetectorState::Idle;
            return DetachOutcome::DetachRequested;
        }
        self.state = DetectorState::Idle;
        DetachOutcome::Forward(vec![self.chord.prefix, event])
    }
}

/// Errors produced when converting a foreign key event into the SDK
/// vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum KeyConversionError {
    /// The foreign event used a key code variant that the SDK does not
    /// model (for example a media key when no enhancement flags were
    /// negotiated).
    UnsupportedKeyCode(&'static str),
    /// The foreign event used modifier bits the SDK does not model.
    UnsupportedModifier(&'static str),
    /// The foreign event reported a key release/repeat the SDK ignores.
    NonPressEvent,
}

impl std::fmt::Display for KeyConversionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedKeyCode(name) => {
                write!(f, "unsupported foreign key code: {name}")
            }
            Self::UnsupportedModifier(name) => {
                write!(f, "unsupported foreign modifier: {name}")
            }
            Self::NonPressEvent => f.write_str("foreign event was not a key press"),
        }
    }
}

impl std::error::Error for KeyConversionError {}

#[cfg(feature = "crossterm")]
mod crossterm_compat;
