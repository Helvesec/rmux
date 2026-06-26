use serde::de::{self, MapAccess, SeqAccess, Visitor};

use crate::request::compat::{compat_next_element, required_next};

use super::{
    LastPaneRequest, RespawnPaneRequest, SelectPaneAdjacentRequest, SelectPaneRequest,
    SplitWindowExtRequest,
};

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
        let detached = compat_next_element(&mut seq)?;
        let size: Option<String> = compat_next_element(&mut seq)?;
        let preserve_zoom = compat_next_element(&mut seq)?;
        let full_size = compat_next_element(&mut seq)?;
        let stdin_payload = compat_next_element(&mut seq)?;

        Ok(SplitWindowExtRequest {
            target,
            direction,
            before,
            environment,
            command,
            process_command,
            start_directory,
            keep_alive_on_exit,
            detached,
            size,
            preserve_zoom,
            full_size,
            stdin_payload,
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
        let mut detached = None;
        let mut size = None;
        let mut preserve_zoom = None;
        let mut full_size = None;
        let mut stdin_payload = None;

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
                "detached" => detached = Some(map.next_value()?),
                "size" => size = Some(map.next_value()?),
                "preserve_zoom" => preserve_zoom = Some(map.next_value()?),
                "full_size" => full_size = Some(map.next_value()?),
                "stdin_payload" => stdin_payload = Some(map.next_value()?),
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }

        Ok(SplitWindowExtRequest {
            target: target.ok_or_else(|| de::Error::missing_field("target"))?,
            direction: direction.ok_or_else(|| de::Error::missing_field("direction"))?,
            before: before.unwrap_or_default(),
            environment: environment.unwrap_or_default(),
            command: command.unwrap_or_default(),
            process_command: process_command.unwrap_or_default(),
            start_directory: start_directory.unwrap_or_default(),
            keep_alive_on_exit: keep_alive_on_exit.unwrap_or_default(),
            detached: detached.unwrap_or_default(),
            size: size.unwrap_or_default(),
            preserve_zoom: preserve_zoom.unwrap_or_default(),
            full_size: full_size.unwrap_or_default(),
            stdin_payload: stdin_payload.unwrap_or_default(),
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
            kill: kill.unwrap_or_default(),
            start_directory: start_directory.unwrap_or_default(),
            environment: environment.unwrap_or_default(),
            command: command.unwrap_or_default(),
            process_command: process_command.unwrap_or_default(),
        })
    }
}

pub(super) struct SelectPaneRequestVisitor;

impl<'de> Visitor<'de> for SelectPaneRequestVisitor {
    type Value = SelectPaneRequest;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a select-pane request")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let target = required_next(&mut seq, 0, &self)?;
        let title = required_next(&mut seq, 1, &self)?;
        let input_disabled = compat_next_element(&mut seq)?;
        let preserve_zoom = compat_next_element(&mut seq)?;
        let style = compat_next_element(&mut seq)?;

        Ok(SelectPaneRequest {
            target,
            title,
            style,
            input_disabled,
            preserve_zoom,
        })
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut target = None;
        let mut title = None;
        let mut style = None;
        let mut input_disabled = None;
        let mut preserve_zoom = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "target" => target = Some(map.next_value()?),
                "title" => title = Some(map.next_value()?),
                "style" => style = Some(map.next_value()?),
                "input_disabled" => input_disabled = Some(map.next_value()?),
                "preserve_zoom" => preserve_zoom = Some(map.next_value()?),
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }

        Ok(SelectPaneRequest {
            target: target.ok_or_else(|| de::Error::missing_field("target"))?,
            title: title.unwrap_or_default(),
            style: style.unwrap_or_default(),
            input_disabled: input_disabled.unwrap_or_default(),
            preserve_zoom: preserve_zoom.unwrap_or_default(),
        })
    }
}

pub(super) struct LastPaneRequestVisitor;

impl<'de> Visitor<'de> for LastPaneRequestVisitor {
    type Value = LastPaneRequest;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a last-pane request")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let target = required_next(&mut seq, 0, &self)?;
        let preserve_zoom = compat_next_element(&mut seq)?;
        let input_disabled = compat_next_element(&mut seq)?;

        Ok(LastPaneRequest {
            target,
            preserve_zoom,
            input_disabled,
        })
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut target = None;
        let mut preserve_zoom = None;
        let mut input_disabled = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "target" => target = Some(map.next_value()?),
                "preserve_zoom" => preserve_zoom = Some(map.next_value()?),
                "input_disabled" => input_disabled = Some(map.next_value()?),
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }

        Ok(LastPaneRequest {
            target: target.ok_or_else(|| de::Error::missing_field("target"))?,
            preserve_zoom: preserve_zoom.unwrap_or_default(),
            input_disabled: input_disabled.unwrap_or_default(),
        })
    }
}

pub(super) struct SelectPaneAdjacentRequestVisitor;

impl<'de> Visitor<'de> for SelectPaneAdjacentRequestVisitor {
    type Value = SelectPaneAdjacentRequest;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a directional select-pane request")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let target = required_next(&mut seq, 0, &self)?;
        let direction = required_next(&mut seq, 1, &self)?;
        let preserve_zoom = compat_next_element(&mut seq)?;

        Ok(SelectPaneAdjacentRequest {
            target,
            direction,
            preserve_zoom,
        })
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut target = None;
        let mut direction = None;
        let mut preserve_zoom = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "target" => target = Some(map.next_value()?),
                "direction" => direction = Some(map.next_value()?),
                "preserve_zoom" => preserve_zoom = Some(map.next_value()?),
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }

        Ok(SelectPaneAdjacentRequest {
            target: target.ok_or_else(|| de::Error::missing_field("target"))?,
            direction: direction.ok_or_else(|| de::Error::missing_field("direction"))?,
            preserve_zoom: preserve_zoom.unwrap_or_default(),
        })
    }
}
