//! Parser states and transition tables matching tmux `input.c:390–507`.

use std::sync::LazyLock;

/// The 17 parser states matching tmux exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputState {
    /// Ground state — normal printable character processing.
    Ground,
    /// ESC received, waiting for sequence identifier.
    EscEnter,
    /// ESC intermediate character(s) collected.
    EscIntermediate,
    /// CSI (`ESC [`) received, waiting for parameters.
    CsiEnter,
    /// CSI parameter bytes being collected.
    CsiParameter,
    /// CSI intermediate character(s) collected.
    CsiIntermediate,
    /// CSI with invalid parameter — absorb until final byte.
    CsiIgnore,
    /// DCS (`ESC P`) received, waiting for parameters.
    DcsEnter,
    /// DCS parameter bytes being collected.
    DcsParameter,
    /// DCS intermediate character(s) collected.
    DcsIntermediate,
    /// DCS passthrough data handler.
    DcsHandler,
    /// ESC received inside DCS passthrough.
    DcsEscape,
    /// DCS with invalid parameter — absorb until ST.
    DcsIgnore,
    /// OSC string being collected.
    OscString,
    /// APC string being collected.
    ApcString,
    /// Screen rename (`ESC k`) string being collected.
    RenameString,
    /// Unknown ESC-initiated sequence — absorb until ST.
    ConsumeSt,
}

/// Handler action to execute for a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Handler {
    None,
    Print,
    C0Dispatch,
    EscDispatch,
    CsiDispatch,
    DcsDispatch,
    Intermediate,
    Parameter,
    Input,
    TopBitSet,
    EndBel,
}

/// A single entry in a state transition table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TransitionEntry {
    pub first: u8,
    pub last: u8,
    pub handler: Handler,
    pub next_state: Option<InputState>,
}

/// Resolved transition from a table lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Transition {
    pub handler: Handler,
    pub next_state: Option<InputState>,
}

impl InputState {
    pub(crate) const COUNT: usize = 17;

    pub(crate) const ALL: [Self; Self::COUNT] = [
        Self::Ground,
        Self::EscEnter,
        Self::EscIntermediate,
        Self::CsiEnter,
        Self::CsiParameter,
        Self::CsiIntermediate,
        Self::CsiIgnore,
        Self::DcsEnter,
        Self::DcsParameter,
        Self::DcsIntermediate,
        Self::DcsHandler,
        Self::DcsEscape,
        Self::DcsIgnore,
        Self::OscString,
        Self::ApcString,
        Self::RenameString,
        Self::ConsumeSt,
    ];

    /// Returns the transition table for this state.
    pub(crate) fn transition_table(self) -> &'static [TransitionEntry] {
        match self {
            Self::Ground => GROUND_TABLE,
            Self::EscEnter => ESC_ENTER_TABLE,
            Self::EscIntermediate => ESC_INTERMEDIATE_TABLE,
            Self::CsiEnter => CSI_ENTER_TABLE,
            Self::CsiParameter => CSI_PARAMETER_TABLE,
            Self::CsiIntermediate => CSI_INTERMEDIATE_TABLE,
            Self::CsiIgnore => CSI_IGNORE_TABLE,
            Self::DcsEnter => DCS_ENTER_TABLE,
            Self::DcsParameter => DCS_PARAMETER_TABLE,
            Self::DcsIntermediate => DCS_INTERMEDIATE_TABLE,
            Self::DcsHandler => DCS_HANDLER_TABLE,
            Self::DcsEscape => DCS_ESCAPE_TABLE,
            Self::DcsIgnore => DCS_IGNORE_TABLE,
            Self::OscString => OSC_STRING_TABLE,
            Self::ApcString => APC_STRING_TABLE,
            Self::RenameString => RENAME_STRING_TABLE,
            Self::ConsumeSt => CONSUME_ST_TABLE,
        }
    }

    pub(crate) fn transition_for_byte(self, byte: u8) -> Transition {
        TRANSITION_LUT[self.index()][byte as usize]
    }

    const fn index(self) -> usize {
        match self {
            Self::Ground => 0,
            Self::EscEnter => 1,
            Self::EscIntermediate => 2,
            Self::CsiEnter => 3,
            Self::CsiParameter => 4,
            Self::CsiIntermediate => 5,
            Self::CsiIgnore => 6,
            Self::DcsEnter => 7,
            Self::DcsParameter => 8,
            Self::DcsIntermediate => 9,
            Self::DcsHandler => 10,
            Self::DcsEscape => 11,
            Self::DcsIgnore => 12,
            Self::OscString => 13,
            Self::ApcString => 14,
            Self::RenameString => 15,
            Self::ConsumeSt => 16,
        }
    }
}

static TRANSITION_LUT: LazyLock<[[Transition; 256]; InputState::COUNT]> =
    LazyLock::new(build_transition_lut);

fn build_transition_lut() -> [[Transition; 256]; InputState::COUNT] {
    let fallback = Transition {
        handler: Handler::None,
        next_state: None,
    };
    let mut lut = [[fallback; 256]; InputState::COUNT];

    for state in InputState::ALL {
        let row = &mut lut[state.index()];
        for entry in state.transition_table() {
            for byte in entry.first..=entry.last {
                row[byte as usize] = Transition {
                    handler: entry.handler,
                    next_state: entry.next_state,
                };
            }
        }
    }

    lut
}

// Shorthand.
use Handler as H;
use InputState as S;

const fn e(
    first: u8,
    last: u8,
    handler: Handler,
    next_state: Option<InputState>,
) -> TransitionEntry {
    TransitionEntry {
        first,
        last,
        handler,
        next_state,
    }
}

// ─── ground ────────────────────────────────────────────────────────
static GROUND_TABLE: &[TransitionEntry] = &[
    e(0x18, 0x18, H::C0Dispatch, Some(S::Ground)),
    e(0x1a, 0x1a, H::C0Dispatch, Some(S::Ground)),
    e(0x1b, 0x1b, H::None, Some(S::EscEnter)),
    e(0x00, 0x17, H::C0Dispatch, None),
    e(0x19, 0x19, H::C0Dispatch, None),
    e(0x1c, 0x1f, H::C0Dispatch, None),
    e(0x20, 0x7e, H::Print, None),
    e(0x7f, 0x7f, H::None, None),
    e(0x80, 0xff, H::TopBitSet, None),
];

// ─── esc_enter ─────────────────────────────────────────────────────
static ESC_ENTER_TABLE: &[TransitionEntry] = &[
    e(0x18, 0x18, H::C0Dispatch, Some(S::Ground)),
    e(0x1a, 0x1a, H::C0Dispatch, Some(S::Ground)),
    e(0x1b, 0x1b, H::None, Some(S::EscEnter)),
    e(0x00, 0x17, H::C0Dispatch, None),
    e(0x19, 0x19, H::C0Dispatch, None),
    e(0x1c, 0x1f, H::C0Dispatch, None),
    e(0x20, 0x2f, H::Intermediate, Some(S::EscIntermediate)),
    e(0x30, 0x4f, H::EscDispatch, Some(S::Ground)),
    e(0x50, 0x50, H::None, Some(S::DcsEnter)),
    e(0x51, 0x57, H::EscDispatch, Some(S::Ground)),
    e(0x58, 0x58, H::None, Some(S::ConsumeSt)),
    e(0x59, 0x5a, H::EscDispatch, Some(S::Ground)),
    e(0x5b, 0x5b, H::None, Some(S::CsiEnter)),
    e(0x5c, 0x5c, H::EscDispatch, Some(S::Ground)),
    e(0x5d, 0x5d, H::None, Some(S::OscString)),
    e(0x5e, 0x5e, H::None, Some(S::ConsumeSt)),
    e(0x5f, 0x5f, H::None, Some(S::ApcString)),
    e(0x60, 0x6a, H::EscDispatch, Some(S::Ground)),
    e(0x6b, 0x6b, H::None, Some(S::RenameString)),
    e(0x6c, 0x7e, H::EscDispatch, Some(S::Ground)),
    e(0x7f, 0xff, H::None, None),
];

// ─── esc_intermediate ──────────────────────────────────────────────
static ESC_INTERMEDIATE_TABLE: &[TransitionEntry] = &[
    e(0x18, 0x18, H::C0Dispatch, Some(S::Ground)),
    e(0x1a, 0x1a, H::C0Dispatch, Some(S::Ground)),
    e(0x1b, 0x1b, H::None, Some(S::EscEnter)),
    e(0x00, 0x17, H::C0Dispatch, None),
    e(0x19, 0x19, H::C0Dispatch, None),
    e(0x1c, 0x1f, H::C0Dispatch, None),
    e(0x20, 0x2f, H::Intermediate, None),
    e(0x30, 0x7e, H::EscDispatch, Some(S::Ground)),
    e(0x7f, 0xff, H::None, None),
];

// ─── csi_enter ─────────────────────────────────────────────────────
static CSI_ENTER_TABLE: &[TransitionEntry] = &[
    e(0x18, 0x18, H::C0Dispatch, Some(S::Ground)),
    e(0x1a, 0x1a, H::C0Dispatch, Some(S::Ground)),
    e(0x1b, 0x1b, H::None, Some(S::EscEnter)),
    e(0x00, 0x17, H::C0Dispatch, None),
    e(0x19, 0x19, H::C0Dispatch, None),
    e(0x1c, 0x1f, H::C0Dispatch, None),
    e(0x20, 0x2f, H::Intermediate, Some(S::CsiIntermediate)),
    e(0x30, 0x39, H::Parameter, Some(S::CsiParameter)),
    e(0x3a, 0x3a, H::Parameter, Some(S::CsiParameter)),
    e(0x3b, 0x3b, H::Parameter, Some(S::CsiParameter)),
    e(0x3c, 0x3f, H::Intermediate, Some(S::CsiParameter)),
    e(0x40, 0x7e, H::CsiDispatch, Some(S::Ground)),
    e(0x7f, 0xff, H::None, None),
];

// ─── csi_parameter ─────────────────────────────────────────────────
static CSI_PARAMETER_TABLE: &[TransitionEntry] = &[
    e(0x18, 0x18, H::C0Dispatch, Some(S::Ground)),
    e(0x1a, 0x1a, H::C0Dispatch, Some(S::Ground)),
    e(0x1b, 0x1b, H::None, Some(S::EscEnter)),
    e(0x00, 0x17, H::C0Dispatch, None),
    e(0x19, 0x19, H::C0Dispatch, None),
    e(0x1c, 0x1f, H::C0Dispatch, None),
    e(0x20, 0x2f, H::Intermediate, Some(S::CsiIntermediate)),
    e(0x30, 0x39, H::Parameter, None),
    e(0x3a, 0x3a, H::Parameter, None),
    e(0x3b, 0x3b, H::Parameter, None),
    e(0x3c, 0x3f, H::None, Some(S::CsiIgnore)),
    e(0x40, 0x7e, H::CsiDispatch, Some(S::Ground)),
    e(0x7f, 0xff, H::None, None),
];

// ─── csi_intermediate ──────────────────────────────────────────────
static CSI_INTERMEDIATE_TABLE: &[TransitionEntry] = &[
    e(0x18, 0x18, H::C0Dispatch, Some(S::Ground)),
    e(0x1a, 0x1a, H::C0Dispatch, Some(S::Ground)),
    e(0x1b, 0x1b, H::None, Some(S::EscEnter)),
    e(0x00, 0x17, H::C0Dispatch, None),
    e(0x19, 0x19, H::C0Dispatch, None),
    e(0x1c, 0x1f, H::C0Dispatch, None),
    e(0x20, 0x2f, H::Intermediate, None),
    e(0x30, 0x3f, H::None, Some(S::CsiIgnore)),
    e(0x40, 0x7e, H::CsiDispatch, Some(S::Ground)),
    e(0x7f, 0xff, H::None, None),
];

// ─── csi_ignore ────────────────────────────────────────────────────
static CSI_IGNORE_TABLE: &[TransitionEntry] = &[
    e(0x18, 0x18, H::C0Dispatch, Some(S::Ground)),
    e(0x1a, 0x1a, H::C0Dispatch, Some(S::Ground)),
    e(0x1b, 0x1b, H::None, Some(S::EscEnter)),
    e(0x00, 0x17, H::C0Dispatch, None),
    e(0x19, 0x19, H::C0Dispatch, None),
    e(0x1c, 0x1f, H::C0Dispatch, None),
    e(0x20, 0x3f, H::None, None),
    e(0x40, 0x7e, H::None, Some(S::Ground)),
    e(0x7f, 0xff, H::None, None),
];

// ─── dcs_enter ─────────────────────────────────────────────────────
static DCS_ENTER_TABLE: &[TransitionEntry] = &[
    e(0x18, 0x18, H::C0Dispatch, Some(S::Ground)),
    e(0x1a, 0x1a, H::C0Dispatch, Some(S::Ground)),
    e(0x1b, 0x1b, H::None, Some(S::EscEnter)),
    e(0x00, 0x17, H::None, None),
    e(0x19, 0x19, H::None, None),
    e(0x1c, 0x1f, H::None, None),
    e(0x20, 0x2f, H::Intermediate, Some(S::DcsIntermediate)),
    e(0x30, 0x39, H::Parameter, Some(S::DcsParameter)),
    e(0x3a, 0x3a, H::None, Some(S::DcsIgnore)),
    e(0x3b, 0x3b, H::Parameter, Some(S::DcsParameter)),
    e(0x3c, 0x3f, H::Intermediate, Some(S::DcsParameter)),
    e(0x40, 0x7e, H::Input, Some(S::DcsHandler)),
    e(0x7f, 0xff, H::None, None),
];

// ─── dcs_parameter ─────────────────────────────────────────────────
static DCS_PARAMETER_TABLE: &[TransitionEntry] = &[
    e(0x18, 0x18, H::C0Dispatch, Some(S::Ground)),
    e(0x1a, 0x1a, H::C0Dispatch, Some(S::Ground)),
    e(0x1b, 0x1b, H::None, Some(S::EscEnter)),
    e(0x00, 0x17, H::None, None),
    e(0x19, 0x19, H::None, None),
    e(0x1c, 0x1f, H::None, None),
    e(0x20, 0x2f, H::Intermediate, Some(S::DcsIntermediate)),
    e(0x30, 0x39, H::Parameter, None),
    e(0x3a, 0x3a, H::None, Some(S::DcsIgnore)),
    e(0x3b, 0x3b, H::Parameter, None),
    e(0x3c, 0x3f, H::None, Some(S::DcsIgnore)),
    e(0x40, 0x7e, H::Input, Some(S::DcsHandler)),
    e(0x7f, 0xff, H::None, None),
];

// ─── dcs_intermediate ──────────────────────────────────────────────
static DCS_INTERMEDIATE_TABLE: &[TransitionEntry] = &[
    e(0x18, 0x18, H::C0Dispatch, Some(S::Ground)),
    e(0x1a, 0x1a, H::C0Dispatch, Some(S::Ground)),
    e(0x1b, 0x1b, H::None, Some(S::EscEnter)),
    e(0x00, 0x17, H::None, None),
    e(0x19, 0x19, H::None, None),
    e(0x1c, 0x1f, H::None, None),
    e(0x20, 0x2f, H::Intermediate, None),
    e(0x30, 0x3f, H::None, Some(S::DcsIgnore)),
    e(0x40, 0x7e, H::Input, Some(S::DcsHandler)),
    e(0x7f, 0xff, H::None, None),
];

// ─── dcs_handler ───────────────────────────────────────────────────
// NOTE: No INPUT_STATE_ANYWHERE — this is a deliberate tmux deviation.
static DCS_HANDLER_TABLE: &[TransitionEntry] = &[
    e(0x00, 0x1a, H::Input, None),
    e(0x1b, 0x1b, H::None, Some(S::DcsEscape)),
    e(0x1c, 0xff, H::Input, None),
];

// ─── dcs_escape ────────────────────────────────────────────────────
// NOTE: No INPUT_STATE_ANYWHERE — deliberate tmux deviation.
static DCS_ESCAPE_TABLE: &[TransitionEntry] = &[
    e(0x00, 0x5b, H::Input, Some(S::DcsHandler)),
    e(0x5c, 0x5c, H::DcsDispatch, Some(S::Ground)),
    e(0x5d, 0xff, H::Input, Some(S::DcsHandler)),
];

// ─── dcs_ignore ────────────────────────────────────────────────────
static DCS_IGNORE_TABLE: &[TransitionEntry] = &[
    e(0x18, 0x18, H::C0Dispatch, Some(S::Ground)),
    e(0x1a, 0x1a, H::C0Dispatch, Some(S::Ground)),
    e(0x1b, 0x1b, H::None, Some(S::EscEnter)),
    e(0x00, 0x17, H::None, None),
    e(0x19, 0x19, H::None, None),
    e(0x1c, 0x1f, H::None, None),
    e(0x20, 0xff, H::None, None),
];

// ─── osc_string ────────────────────────────────────────────────────
static OSC_STRING_TABLE: &[TransitionEntry] = &[
    e(0x18, 0x18, H::C0Dispatch, Some(S::Ground)),
    e(0x1a, 0x1a, H::C0Dispatch, Some(S::Ground)),
    e(0x1b, 0x1b, H::None, Some(S::EscEnter)),
    e(0x00, 0x06, H::None, None),
    e(0x07, 0x07, H::EndBel, Some(S::Ground)),
    e(0x08, 0x17, H::None, None),
    e(0x19, 0x19, H::None, None),
    e(0x1c, 0x1f, H::None, None),
    e(0x20, 0xff, H::Input, None),
];

// ─── apc_string ────────────────────────────────────────────────────
static APC_STRING_TABLE: &[TransitionEntry] = &[
    e(0x18, 0x18, H::C0Dispatch, Some(S::Ground)),
    e(0x1a, 0x1a, H::C0Dispatch, Some(S::Ground)),
    e(0x1b, 0x1b, H::None, Some(S::EscEnter)),
    e(0x00, 0x17, H::None, None),
    e(0x19, 0x19, H::None, None),
    e(0x1c, 0x1f, H::None, None),
    e(0x20, 0xff, H::Input, None),
];

// ─── rename_string ─────────────────────────────────────────────────
static RENAME_STRING_TABLE: &[TransitionEntry] = &[
    e(0x18, 0x18, H::C0Dispatch, Some(S::Ground)),
    e(0x1a, 0x1a, H::C0Dispatch, Some(S::Ground)),
    e(0x1b, 0x1b, H::None, Some(S::EscEnter)),
    e(0x00, 0x17, H::None, None),
    e(0x19, 0x19, H::None, None),
    e(0x1c, 0x1f, H::None, None),
    e(0x20, 0xff, H::Input, None),
];

// ─── consume_st ────────────────────────────────────────────────────
static CONSUME_ST_TABLE: &[TransitionEntry] = &[
    e(0x18, 0x18, H::C0Dispatch, Some(S::Ground)),
    e(0x1a, 0x1a, H::C0Dispatch, Some(S::Ground)),
    e(0x1b, 0x1b, H::None, Some(S::EscEnter)),
    e(0x00, 0x17, H::None, None),
    e(0x19, 0x19, H::None, None),
    e(0x1c, 0x1f, H::None, None),
    e(0x20, 0xff, H::None, None),
];
