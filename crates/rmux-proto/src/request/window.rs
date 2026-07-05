use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::path::PathBuf;

use crate::{ProcessCommand, SessionName, WindowTarget};

use super::compat::compat_next_element;

/// Request payload for `new-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NewWindowRequest {
    /// The exact target session name.
    pub target: SessionName,
    /// The optional explicit window name.
    pub name: Option<String>,
    /// Whether the newly created window should remain inactive.
    pub detached: bool,
    /// Optional per-spawn environment overrides in `NAME=VALUE` form.
    #[serde(default)]
    pub environment: Option<Vec<String>>,
    /// Optional shell command argv. A single argument is executed via `$SHELL -c`.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Optional working-directory override.
    #[serde(default)]
    pub start_directory: Option<PathBuf>,
    /// Optional destination window index from `new-window -t session:index`.
    #[serde(default)]
    pub target_window_index: Option<u32>,
    /// Whether an occupied destination index should be opened by shifting windows upward.
    #[serde(default)]
    pub insert_at_target: bool,
    /// Explicit process launch mode for the new window's initial pane.
    #[serde(default)]
    pub process_command: Option<ProcessCommand>,
}

/// Request payload for `kill-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillWindowRequest {
    /// The exact target window.
    pub target: WindowTarget,
    /// Whether all other windows in the session should be removed instead.
    pub kill_all_others: bool,
}

/// Request payload for `select-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectWindowRequest {
    /// The exact target window.
    pub target: WindowTarget,
}

/// Request payload for `rename-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameWindowRequest {
    /// The exact target window.
    pub target: WindowTarget,
    /// The new window name.
    pub name: String,
}

/// Request payload for `next-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextWindowRequest {
    /// The exact target session name.
    pub target: SessionName,
    /// Whether only alerted windows should be considered.
    #[serde(default)]
    pub alerts_only: bool,
}

/// Request payload for `previous-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviousWindowRequest {
    /// The exact target session name.
    pub target: SessionName,
    /// Whether only alerted windows should be considered.
    #[serde(default)]
    pub alerts_only: bool,
}

/// Request payload for `last-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastWindowRequest {
    /// The exact target session name.
    pub target: SessionName,
}

/// Request payload for `list-windows`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListWindowsRequest {
    /// The exact target session name.
    pub target: SessionName,
    /// An optional server-side compatibility format template.
    pub format: Option<String>,
    /// Optional filter expression.
    #[serde(default)]
    pub filter: Option<String>,
    /// Optional sort-order token.
    #[serde(default)]
    pub sort_order: Option<String>,
    /// Whether the selected sort order should be reversed.
    #[serde(default)]
    pub reversed: bool,
}

/// Request payload for `link-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkWindowRequest {
    /// The source window slot.
    pub source: WindowTarget,
    /// The destination window slot.
    pub target: WindowTarget,
    /// Whether to insert after the target slot (`-a`).
    #[serde(default)]
    pub after: bool,
    /// Whether to insert before the target slot (`-b`).
    #[serde(default)]
    pub before: bool,
    /// Whether an occupied destination should be replaced (`-k`).
    #[serde(default)]
    pub kill_destination: bool,
    /// Whether the destination session should keep its current active window (`-d`).
    #[serde(default)]
    pub detached: bool,
}

/// Target forms accepted by `move-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MoveWindowTarget {
    /// Applies to the addressed session during `move-window -r`.
    Session(SessionName),
    /// Applies to the addressed destination window slot.
    Window(WindowTarget),
}

/// Request payload for `move-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MoveWindowRequest {
    /// The optional source window being moved when not reindexing.
    pub source: Option<WindowTarget>,
    /// The destination window slot or reindex target session.
    pub target: MoveWindowTarget,
    /// Whether the session should be reindexed instead of moving one window.
    pub renumber: bool,
    /// Whether an occupied destination should be replaced.
    pub kill_destination: bool,
    /// Whether the destination session should keep its current active window.
    pub detached: bool,
    /// Whether to insert after the target slot (`-a`).
    #[serde(default)]
    pub after: bool,
    /// Whether to insert before the target slot (`-b`).
    #[serde(default)]
    pub before: bool,
}

/// Request payload for `swap-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwapWindowRequest {
    /// The source window slot.
    pub source: WindowTarget,
    /// The destination window slot.
    pub target: WindowTarget,
    /// Whether the swapped destination slots should become active after the swap.
    pub detached: bool,
}

/// The supported pane rotation directions for `rotate-window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RotateWindowDirection {
    /// Move the last pane to the head.
    Down,
    /// Move the first pane to the tail.
    Up,
}

/// Request payload for `rotate-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotateWindowRequest {
    /// The addressed window.
    pub target: WindowTarget,
    /// The requested rotation direction.
    pub direction: RotateWindowDirection,
    /// Whether to save and restore zoom state around the rotation (`-Z`).
    #[serde(default)]
    pub restore_zoom: bool,
}

/// Request payload for `resize-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResizeWindowRequest {
    /// The addressed window.
    pub target: WindowTarget,
    /// Optional explicit width (`-x`).
    pub width: Option<u16>,
    /// Optional explicit height (`-y`).
    pub height: Option<u16>,
    /// Relative adjustment (from `-D`, `-U`, `-L`, `-R`).
    #[serde(default)]
    pub adjustment: Option<ResizeWindowAdjustment>,
}

/// Directional relative-size adjustment for `resize-window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResizeWindowAdjustment {
    /// Shrink height (`-U`).
    Up(u16),
    /// Grow height (`-D`).
    Down(u16),
    /// Shrink width (`-L`).
    Left(u16),
    /// Grow width (`-R`).
    Right(u16),
    /// Resize to the largest attached session containing the window (`-A`).
    LargestLinkedSession,
    /// Resize to the smallest attached session containing the window (`-a`).
    SmallestLinkedSession,
}

/// Request payload for `respawn-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RespawnWindowRequest {
    /// The addressed window.
    pub target: WindowTarget,
    /// Whether to kill existing panes even when they are still running (`-k`).
    #[serde(default)]
    pub kill: bool,
    /// Optional per-spawn environment overrides in `NAME=VALUE` form.
    #[serde(default)]
    pub environment: Option<Vec<String>>,
    /// Optional shell command argv. A single argument is executed via `$SHELL -c`.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Optional working-directory override.
    #[serde(default)]
    pub start_directory: Option<PathBuf>,
}

impl<'de> Deserialize<'de> for NewWindowRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_struct(
            "NewWindowRequest",
            &[
                "target",
                "name",
                "detached",
                "environment",
                "command",
                "start_directory",
                "target_window_index",
                "insert_at_target",
                "process_command",
            ],
            NewWindowRequestVisitor,
        )
    }
}

impl<'de> Deserialize<'de> for RespawnWindowRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_struct(
            "RespawnWindowRequest",
            &[
                "target",
                "kill",
                "environment",
                "command",
                "start_directory",
            ],
            RespawnWindowRequestVisitor,
        )
    }
}

impl<'de> Deserialize<'de> for MoveWindowRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_struct(
            "MoveWindowRequest",
            &[
                "source",
                "target",
                "renumber",
                "kill_destination",
                "detached",
                "after",
                "before",
            ],
            MoveWindowRequestVisitor,
        )
    }
}

struct NewWindowRequestVisitor;

struct MoveWindowRequestVisitor;

impl<'de> Visitor<'de> for NewWindowRequestVisitor {
    type Value = NewWindowRequest;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a new-window request")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let target = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(0, &self))?;
        let name = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(1, &self))?;
        let detached = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(2, &self))?;
        let environment = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(3, &self))?;
        let command = compat_next_element(&mut seq)?;
        let start_directory = compat_next_element(&mut seq)?;
        let target_window_index = compat_next_element(&mut seq)?;
        let insert_at_target = compat_next_element(&mut seq)?;
        let process_command = compat_next_element(&mut seq)?;

        Ok(NewWindowRequest {
            target,
            name,
            detached,
            environment,
            command,
            start_directory,
            target_window_index,
            insert_at_target,
            process_command,
        })
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut target = None;
        let mut name = None;
        let mut detached = None;
        let mut environment = None;
        let mut command = None;
        let mut process_command = None;
        let mut start_directory = None;
        let mut target_window_index = None;
        let mut insert_at_target = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "target" => target = Some(map.next_value()?),
                "name" => name = Some(map.next_value()?),
                "detached" => detached = Some(map.next_value()?),
                "environment" => environment = Some(map.next_value()?),
                "command" => command = Some(map.next_value()?),
                "process_command" => process_command = Some(map.next_value()?),
                "start_directory" => start_directory = Some(map.next_value()?),
                "target_window_index" => target_window_index = Some(map.next_value()?),
                "insert_at_target" => insert_at_target = Some(map.next_value()?),
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }

        Ok(NewWindowRequest {
            target: target.ok_or_else(|| de::Error::missing_field("target"))?,
            name: name.unwrap_or_default(),
            detached: detached.ok_or_else(|| de::Error::missing_field("detached"))?,
            environment: environment.unwrap_or_default(),
            command: command.unwrap_or_default(),
            process_command: process_command.unwrap_or_default(),
            start_directory: start_directory.unwrap_or_default(),
            target_window_index: target_window_index.unwrap_or_default(),
            insert_at_target: insert_at_target.unwrap_or_default(),
        })
    }
}

impl<'de> Visitor<'de> for MoveWindowRequestVisitor {
    type Value = MoveWindowRequest;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a move-window request")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let source = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(0, &self))?;
        let target = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(1, &self))?;
        let renumber = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(2, &self))?;
        let kill_destination = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(3, &self))?;
        let detached = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(4, &self))?;
        let after = compat_next_element(&mut seq)?;
        let before = compat_next_element(&mut seq)?;

        Ok(MoveWindowRequest {
            source,
            target,
            renumber,
            kill_destination,
            detached,
            after,
            before,
        })
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut source = None;
        let mut target = None;
        let mut renumber = None;
        let mut kill_destination = None;
        let mut detached = None;
        let mut after = None;
        let mut before = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "source" => source = Some(map.next_value()?),
                "target" => target = Some(map.next_value()?),
                "renumber" => renumber = Some(map.next_value()?),
                "kill_destination" => kill_destination = Some(map.next_value()?),
                "detached" => detached = Some(map.next_value()?),
                "after" => after = Some(map.next_value()?),
                "before" => before = Some(map.next_value()?),
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }

        Ok(MoveWindowRequest {
            source: source.unwrap_or_default(),
            target: target.ok_or_else(|| de::Error::missing_field("target"))?,
            renumber: renumber.ok_or_else(|| de::Error::missing_field("renumber"))?,
            kill_destination: kill_destination
                .ok_or_else(|| de::Error::missing_field("kill_destination"))?,
            detached: detached.ok_or_else(|| de::Error::missing_field("detached"))?,
            after: after.unwrap_or_default(),
            before: before.unwrap_or_default(),
        })
    }
}

struct RespawnWindowRequestVisitor;

impl<'de> Visitor<'de> for RespawnWindowRequestVisitor {
    type Value = RespawnWindowRequest;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a respawn-window request")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let target = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(0, &self))?;
        let kill = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(1, &self))?;
        let environment = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(2, &self))?;
        let command = compat_next_element(&mut seq)?;
        let start_directory = compat_next_element(&mut seq)?;

        Ok(RespawnWindowRequest {
            target,
            kill,
            environment,
            command,
            start_directory,
        })
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut target = None;
        let mut kill = None;
        let mut environment = None;
        let mut command = None;
        let mut start_directory = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "target" => target = Some(map.next_value()?),
                "kill" => kill = Some(map.next_value()?),
                "environment" => environment = Some(map.next_value()?),
                "command" => command = Some(map.next_value()?),
                "start_directory" => start_directory = Some(map.next_value()?),
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }

        Ok(RespawnWindowRequest {
            target: target.ok_or_else(|| de::Error::missing_field("target"))?,
            kill: kill.unwrap_or_default(),
            environment: environment.unwrap_or_default(),
            command: command.unwrap_or_default(),
            start_directory: start_directory.unwrap_or_default(),
        })
    }
}
