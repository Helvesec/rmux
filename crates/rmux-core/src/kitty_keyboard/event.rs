use std::fmt;

use super::functions::FUNCTIONS;

/// Maximum accepted size of one complete Kitty keyboard frame.
pub const FRAME_LIMIT: usize = 4096;
/// Maximum number of Unicode scalar values in associated text.
pub const TEXT_LIMIT: usize = 256;
/// Maximum number of entries in either screen's keyboard flag stack.
pub const STACK_LIMIT: usize = 16;

/// Failure returned by the portable Kitty keyboard codec or state model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// An acquired frame was discarded before a terminating byte arrived.
    Incomplete,
    /// A C0 control interrupted an acquired frame before it terminated.
    Interrupted,
    /// A complete frame exceeds [`FRAME_LIMIT`].
    FrameLimit,
    /// A decimal field exceeds the `u32` range.
    DecimalOverflow,
    /// A decimal field contains a non-decimal byte.
    Decimal,
    /// A mandatory key or decimal field is absent.
    MissingKey,
    /// A numeric value is not a Unicode scalar value.
    Scalar,
    /// A frame contains too many fields or subfields.
    Fields,
    /// A wire modifier value is outside `1..=256`.
    ModifierRange,
    /// An event kind is outside `1..=3`.
    EventKind,
    /// A shifted identity was supplied without the Shift modifier.
    ShiftRequired,
    /// Associated text contains a C0, C1, or DEL control scalar.
    TextControl,
    /// Associated text exceeds [`TEXT_LIMIT`].
    TextLimit,
    /// The bytes are not one supported complete frame.
    InvalidFrame,
    /// A key form does not identify the declared semantic key.
    UnknownIdentity,
    /// A supplied keyboard stack exceeds [`STACK_LIMIT`].
    StackLimit,
    /// Legacy text output was requested without known associated text.
    UnavailableText,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Incomplete => "incomplete acquired Kitty keyboard frame",
            Self::Interrupted => "acquired Kitty keyboard frame was interrupted",
            Self::FrameLimit => "Kitty keyboard frame exceeds the size limit",
            Self::DecimalOverflow => "Kitty keyboard decimal field overflows u32",
            Self::Decimal => "Kitty keyboard decimal field is invalid",
            Self::MissingKey => "Kitty keyboard frame is missing a required field",
            Self::Scalar => "Kitty keyboard value is not a Unicode scalar",
            Self::Fields => "Kitty keyboard frame has invalid fields",
            Self::ModifierRange => "Kitty keyboard modifier is out of range",
            Self::EventKind => "Kitty keyboard event kind is out of range",
            Self::ShiftRequired => "shifted key identity requires the Shift modifier",
            Self::TextControl => "Kitty keyboard associated text contains a control",
            Self::TextLimit => "Kitty keyboard associated text exceeds the limit",
            Self::InvalidFrame => "invalid complete Kitty keyboard frame",
            Self::UnknownIdentity => "unknown or inconsistent Kitty key identity",
            Self::StackLimit => "Kitty keyboard stack exceeds the limit",
            Self::UnavailableText => "associated key text is unavailable",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for Error {}

/// A table-backed Kitty functional key identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Function(u8);

impl Function {
    /// Looks up a functional key by its canonical uppercase name.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        FUNCTIONS
            .iter()
            .position(|(candidate, _)| *candidate == name)
            .map(|index| Self(index as u8))
    }

    /// Returns the canonical uppercase name of this functional key.
    #[must_use]
    pub fn name(self) -> &'static str {
        FUNCTIONS[usize::from(self.0)].0
    }

    pub(super) fn preferred(self) -> (u32, u8) {
        FUNCTIONS[usize::from(self.0)].1[0]
    }

    pub(super) fn from_form(number: u32, final_byte: u8) -> Option<Self> {
        FUNCTIONS
            .iter()
            .position(|(_, forms)| forms.contains(&(number, final_byte)))
            .map(|index| Self(index as u8))
    }
}

/// Semantic key identity carried by a Kitty keyboard event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// Text is available but no physical key identity is available.
    TextOnly,
    /// A Unicode scalar key identity outside the functional-key table.
    Scalar(char),
    /// A known table-backed functional key.
    Function(Function),
    /// An unassigned private-use scalar retained as a numeric identity.
    UnassignedPua(char),
}

impl Key {
    pub(super) fn from_form(number: u32, final_byte: u8) -> Result<Self, Error> {
        if let Some(function) = Function::from_form(number, final_byte) {
            return Ok(Self::Function(function));
        }
        if final_byte != b'u' {
            return Err(Error::UnknownIdentity);
        }
        let scalar = char::from_u32(number).ok_or(Error::Scalar)?;
        Ok(match number {
            0 => Self::TextOnly,
            0xe000..=0xf8ff => Self::UnassignedPua(scalar),
            _ => Self::Scalar(scalar),
        })
    }

    pub(super) fn preferred(self) -> (u32, u8) {
        match self {
            Self::TextOnly => (0, b'u'),
            Self::Scalar(scalar) | Self::UnassignedPua(scalar) => (scalar as u32, b'u'),
            Self::Function(function) => function.preferred(),
        }
    }

    pub(super) fn is_c0_exception(self) -> bool {
        matches!(self.preferred(), (9 | 13 | 127, b'u'))
    }
}

/// Physical event kind reported by the Kitty protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Initial key press.
    Press = 1,
    /// Key repeat while held.
    Repeat = 2,
    /// Key release.
    Release = 3,
}

impl TryFrom<u32> for Kind {
    type Error = Error;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Press),
            2 => Ok(Self::Repeat),
            3 => Ok(Self::Release),
            _ => Err(Error::EventKind),
        }
    }
}

/// Associated text and whether the producer made it available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Text {
    /// The wire or producer did not provide associated text.
    Unavailable,
    /// The producer supplied these scalars, possibly an explicitly empty list.
    Known(Vec<char>),
}

impl Text {
    /// Builds validated known text without truncation.
    pub fn known(values: Vec<char>) -> Result<Self, Error> {
        validate_text(&values)?;
        Ok(Self::Known(values))
    }

    /// Returns the supplied scalar values, or an empty slice when unavailable.
    #[must_use]
    pub fn values(&self) -> &[char] {
        match self {
            Self::Unavailable => &[],
            Self::Known(values) => values,
        }
    }

    /// Returns whether associated text was supplied by the producer or wire.
    #[must_use]
    pub const fn is_available(&self) -> bool {
        matches!(self, Self::Known(_))
    }
}

/// A complete semantic Kitty keyboard event.
///
/// This is not a native acquisition record or an RMUX routing envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Unshifted semantic key identity.
    pub key: Key,
    /// Eight-bit modifier mask before the wire's `+1` transformation.
    pub modifiers: u8,
    /// Press, repeat, or release kind.
    pub kind: Kind,
    /// Optional shifted key identity.
    pub shifted: Option<char>,
    /// Optional base-layout key identity.
    pub base: Option<char>,
    /// Associated text and its availability.
    pub text: Text,
}

impl Event {
    /// Validates cross-field, text, and exact table-backed identity invariants.
    pub fn validate(&self) -> Result<(), Error> {
        if self.shifted.is_some() && self.modifiers & 1 == 0 {
            return Err(Error::ShiftRequired);
        }
        validate_text(self.text.values())?;

        let (number, final_byte) = self.key.preferred();
        if Key::from_form(number, final_byte)? != self.key {
            return Err(Error::UnknownIdentity);
        }
        Ok(())
    }
}

fn validate_text(values: &[char]) -> Result<(), Error> {
    if values.len() > TEXT_LIMIT {
        return Err(Error::TextLimit);
    }
    if values.iter().any(|scalar| scalar.is_control()) {
        return Err(Error::TextControl);
    }
    Ok(())
}
