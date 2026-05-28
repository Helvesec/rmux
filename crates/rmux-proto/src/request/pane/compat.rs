use serde::de::{self, MapAccess, SeqAccess, Visitor};

use crate::request::compat::{compat_next_element, required_next};

use super::{RespawnPaneRequest, SplitWindowExtRequest};

pub(super) struct SplitWindowExtRequestVisitor;

impl<'de> Visitor<'de> for SplitWindowExtRequestVisitor {
    type Value = SplitWindowExtRequest;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a split-window extended request")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let target = required_next(&mut seq, 0, &self)?;
        let direction = required_next(&mut seq, 1, &self)?;
        let before = required_next(&mut seq, 2, &self)?;
        let environment = required_next(&mut seq, 3, &self)?;
        let command = compat_next_element(&mut seq)?;
        let process_command = compat_next_element(&mut seq)?;
        let start_directory = compat_next_element(&mut seq)?;
        let keep_alive_on_exit = compat_next_element(&mut seq)?;

        Ok(SplitWindowExtRequest {
            target,
            direction,
            before,
            environment,
            command,
            process_command,
            start_directory,
            keep_alive_on_exit,
        })
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut target = None;
        let mut direction = None;
        let mut before = None;
        let mut environment = None;
        let mut command = None;
        let mut process_command = None;
        let mut start_directory = None;
        let mut keep_alive_on_exit = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "target" => target = Some(map.next_value()?),
                "direction" => direction = Some(map.next_value()?),
                "before" => before = Some(map.next_value()?),
                "environment" => environment = Some(map.next_value()?),
                "command" => command = Some(map.next_value()?),
                "process_command" => process_command = Some(map.next_value()?),
                "start_directory" => start_directory = Some(map.next_value()?),
                "keep_alive_on_exit" => keep_alive_on_exit = Some(map.next_value()?),
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }

        Ok(SplitWindowExtRequest {
            target: target.ok_or_else(|| de::Error::missing_field("target"))?,
            direction: direction.ok_or_else(|| de::Error::missing_field("direction"))?,
            before: before.ok_or_else(|| de::Error::missing_field("before"))?,
            environment: environment.ok_or_else(|| de::Error::missing_field("environment"))?,
            command: command.unwrap_or_default(),
            process_command: process_command.unwrap_or_default(),
            start_directory: start_directory.unwrap_or_default(),
            keep_alive_on_exit: keep_alive_on_exit.unwrap_or_default(),
        })
    }
}

pub(super) struct RespawnPaneRequestVisitor;

impl<'de> Visitor<'de> for RespawnPaneRequestVisitor {
    type Value = RespawnPaneRequest;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a respawn-pane request")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let target = required_next(&mut seq, 0, &self)?;
        let kill = required_next(&mut seq, 1, &self)?;
        let start_directory = required_next(&mut seq, 2, &self)?;
        let environment = required_next(&mut seq, 3, &self)?;
        let command = required_next(&mut seq, 4, &self)?;
        let process_command = compat_next_element(&mut seq)?;

        Ok(RespawnPaneRequest {
            target,
            kill,
            start_directory,
            environment,
            command,
            process_command,
        })
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut target = None;
        let mut kill = None;
        let mut start_directory = None;
        let mut environment = None;
        let mut command = None;
        let mut process_command = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "target" => target = Some(map.next_value()?),
                "kill" => kill = Some(map.next_value()?),
                "start_directory" => start_directory = Some(map.next_value()?),
                "environment" => environment = Some(map.next_value()?),
                "command" => command = Some(map.next_value()?),
                "process_command" => process_command = Some(map.next_value()?),
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }

        Ok(RespawnPaneRequest {
            target: target.ok_or_else(|| de::Error::missing_field("target"))?,
            kill: kill.ok_or_else(|| de::Error::missing_field("kill"))?,
            start_directory: start_directory
                .ok_or_else(|| de::Error::missing_field("start_directory"))?,
            environment: environment.ok_or_else(|| de::Error::missing_field("environment"))?,
            command: command.ok_or_else(|| de::Error::missing_field("command"))?,
            process_command: process_command.unwrap_or_default(),
        })
    }
}
