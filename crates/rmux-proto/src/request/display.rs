use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::Target;

use super::compat::{compat_next_element, required_next};

/// Duration of an attached `display-message` overlay, in milliseconds.
///
/// tmux accepts the full unsigned 32-bit range for `-d`; keeping that range in
/// the wire type prevents platform-sized integer differences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DisplayMessageDurationMillis(u32);

impl DisplayMessageDurationMillis {
    /// Creates a duration from its tmux-compatible millisecond value.
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the duration in milliseconds.
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Error returned while parsing a tmux `display-message -d` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayMessageDurationParseError {
    /// The value is not a base-ten integer.
    Invalid,
    /// The value is below zero.
    TooSmall,
    /// The value exceeds the unsigned 32-bit tmux range.
    TooLarge,
}

impl fmt::Display for DisplayMessageDurationParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Invalid => "delay invalid",
            Self::TooSmall => "delay too small",
            Self::TooLarge => "delay too large",
        })
    }
}

impl std::error::Error for DisplayMessageDurationParseError {}

impl FromStr for DisplayMessageDurationMillis {
    type Err = DisplayMessageDurationParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        // tmux's numeric parser accepts leading ASCII whitespace through
        // strtoull(3), but requires the conversion to consume the remainder.
        // In particular, `-d ' 1'` is valid while `-d '1 '` is not.
        let value = value.trim_start_matches(|character: char| character.is_ascii_whitespace());
        let (negative, digits) = match value.as_bytes().first() {
            Some(b'+') => (false, &value[1..]),
            Some(b'-') => (true, &value[1..]),
            Some(_) => (false, value),
            None => return Err(DisplayMessageDurationParseError::Invalid),
        };
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(DisplayMessageDurationParseError::Invalid);
        }
        let magnitude = digits.parse::<u128>().map_err(|_| {
            if negative {
                DisplayMessageDurationParseError::TooSmall
            } else {
                DisplayMessageDurationParseError::TooLarge
            }
        })?;
        if negative && magnitude != 0 {
            return Err(DisplayMessageDurationParseError::TooSmall);
        }
        let value =
            u32::try_from(magnitude).map_err(|_| DisplayMessageDurationParseError::TooLarge)?;
        Ok(Self(value))
    }
}

/// Request payload for `display-message`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisplayMessageRequest {
    /// The optional exact session, window, or pane target used as format context.
    pub target: Option<Target>,
    /// Whether to print the expanded message to stdout instead of displaying it.
    pub print: bool,
    /// The optional format string. When omitted, the tmux-compatible default is used.
    pub message: Option<String>,
    /// Whether target lookup failed under tmux `CANFAIL` rules and should render empty target fields.
    #[serde(default)]
    pub empty_target_context: bool,
}

impl<'de> Deserialize<'de> for DisplayMessageRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_struct(
            "DisplayMessageRequest",
            &["target", "print", "message", "empty_target_context"],
            DisplayMessageRequestVisitor,
        )
    }
}

struct DisplayMessageRequestVisitor;

impl<'de> Visitor<'de> for DisplayMessageRequestVisitor {
    type Value = DisplayMessageRequest;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a display-message request")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let target = required_next(&mut seq, 0, &self)?;
        let print = required_next(&mut seq, 1, &self)?;
        let message = required_next(&mut seq, 2, &self)?;
        let empty_target_context: bool = compat_next_element(&mut seq)?;

        Ok(DisplayMessageRequest {
            target,
            print,
            message,
            empty_target_context,
        })
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut target = None;
        let mut print = None;
        let mut message = None;
        let mut empty_target_context = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "target" => target = Some(map.next_value()?),
                "print" => print = Some(map.next_value()?),
                "message" => message = Some(map.next_value()?),
                "empty_target_context" => empty_target_context = Some(map.next_value()?),
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }

        Ok(DisplayMessageRequest {
            target: target.unwrap_or_default(),
            print: print.ok_or_else(|| de::Error::missing_field("print"))?,
            message: message.unwrap_or_default(),
            empty_target_context: empty_target_context.unwrap_or_default(),
        })
    }
}

/// Extended request payload for `display-message -c`.
///
/// This stays separate from [`DisplayMessageRequest`] so the original bincode
/// field order remains wire-compatible with older clients and daemons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisplayMessageExtRequest {
    /// The optional exact session, window, or pane target used as format context.
    pub target: Option<Target>,
    /// Whether to print the expanded message to stdout instead of displaying it.
    pub print: bool,
    /// The optional format string. When omitted, the tmux-compatible default is used.
    pub message: Option<String>,
    /// Optional target client used for client formats and overlay delivery.
    pub target_client: Option<String>,
    /// Whether target lookup failed under tmux `CANFAIL` rules and should render empty target fields.
    #[serde(default)]
    pub empty_target_context: bool,
    /// Optional attached-overlay duration supplied by `display-message -d`.
    #[serde(default)]
    pub duration_ms: Option<DisplayMessageDurationMillis>,
    /// Whether attached keyboard input is ignored until a positive duration expires.
    #[serde(default)]
    pub ignore_input: bool,
}

impl<'de> Deserialize<'de> for DisplayMessageExtRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_struct(
            "DisplayMessageExtRequest",
            &[
                "target",
                "print",
                "message",
                "target_client",
                "empty_target_context",
                "duration_ms",
                "ignore_input",
            ],
            DisplayMessageExtRequestVisitor,
        )
    }
}

struct DisplayMessageExtRequestVisitor;

impl<'de> Visitor<'de> for DisplayMessageExtRequestVisitor {
    type Value = DisplayMessageExtRequest;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a display-message extended request")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let target = required_next(&mut seq, 0, &self)?;
        let print = required_next(&mut seq, 1, &self)?;
        let message = required_next(&mut seq, 2, &self)?;
        let target_client = required_next(&mut seq, 3, &self)?;
        let empty_target_context: bool = compat_next_element(&mut seq)?;
        let duration_ms = compat_next_element(&mut seq)?;
        let ignore_input = compat_next_element(&mut seq)?;

        Ok(DisplayMessageExtRequest {
            target,
            print,
            message,
            target_client,
            empty_target_context,
            duration_ms,
            ignore_input,
        })
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut target = None;
        let mut print = None;
        let mut message = None;
        let mut target_client = None;
        let mut empty_target_context = None;
        let mut duration_ms = None;
        let mut ignore_input = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "target" => target = Some(map.next_value()?),
                "print" => print = Some(map.next_value()?),
                "message" => message = Some(map.next_value()?),
                "target_client" => target_client = Some(map.next_value()?),
                "empty_target_context" => empty_target_context = Some(map.next_value()?),
                "duration_ms" => duration_ms = Some(map.next_value()?),
                "ignore_input" => ignore_input = Some(map.next_value()?),
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }

        Ok(DisplayMessageExtRequest {
            target: target.unwrap_or_default(),
            print: print.ok_or_else(|| de::Error::missing_field("print"))?,
            message: message.unwrap_or_default(),
            target_client: target_client.unwrap_or_default(),
            empty_target_context: empty_target_context.unwrap_or_default(),
            duration_ms: duration_ms.unwrap_or_default(),
            ignore_input: ignore_input.unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod duration_tests {
    use super::{DisplayMessageDurationMillis, DisplayMessageDurationParseError as ParseError};

    #[test]
    fn duration_parser_matches_tmux_37b_integer_domain() {
        // Oracle: tmux 3.7b accepts signed decimal zero and u32::MAX, rejects
        // u32::MAX + 1 as "too large", negatives as "too small", and
        // fractional/non-decimal text as "invalid".
        for (value, expected) in [
            ("0", 0),
            ("-0", 0),
            ("+1", 1),
            (" 01", 1),
            ("\t01", 1),
            ("4294967295", u32::MAX),
        ] {
            assert_eq!(
                value
                    .parse::<DisplayMessageDurationMillis>()
                    .expect("valid tmux display delay")
                    .get(),
                expected
            );
        }
        assert_eq!(
            "-1".parse::<DisplayMessageDurationMillis>(),
            Err(ParseError::TooSmall)
        );
        assert_eq!(
            "4294967296".parse::<DisplayMessageDurationMillis>(),
            Err(ParseError::TooLarge)
        );
        for value in ["", "1 ", " 1 ", "1\t", "1.0", "0x10", "nope"] {
            assert_eq!(
                value.parse::<DisplayMessageDurationMillis>(),
                Err(ParseError::Invalid)
            );
        }
    }
}
