use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::Target;

use super::compat::{compat_next_element, required_next};

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

        Ok(DisplayMessageExtRequest {
            target,
            print,
            message,
            target_client,
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
        let mut target_client = None;
        let mut empty_target_context = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "target" => target = Some(map.next_value()?),
                "print" => print = Some(map.next_value()?),
                "message" => message = Some(map.next_value()?),
                "target_client" => target_client = Some(map.next_value()?),
                "empty_target_context" => empty_target_context = Some(map.next_value()?),
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
        })
    }
}
