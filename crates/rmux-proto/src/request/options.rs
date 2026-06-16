use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    HookLifecycle, HookName, OptionName, OptionScopeSelector, ScopeSelector, SetOptionMode,
};

use super::compat::{compat_next_element, required_next};

/// The supported `set-environment` mutation modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SetEnvironmentMode {
    /// Store or replace a concrete value.
    Set,
    /// Leave a tombstone entry in place of a value.
    Clear,
    /// Remove the entry entirely.
    Unset,
}

/// Request payload for `set-option`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetOptionRequest {
    /// The selected mutation scope.
    pub scope: ScopeSelector,
    /// The supported option name.
    pub option: OptionName,
    /// The raw option value.
    pub value: String,
    /// Whether the mutation replaces or appends.
    pub mode: SetOptionMode,
}

/// Request payload for `set-option` using an open option name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetOptionByNameRequest {
    /// The selected mutation scope.
    pub scope: OptionScopeSelector,
    /// The raw option name, including optional array index syntax.
    pub name: String,
    /// The raw option value. `None` applies tmux-style toggle or unset semantics.
    pub value: Option<String>,
    /// Whether the mutation replaces or appends.
    pub mode: SetOptionMode,
    /// Rejects the mutation when the target entry is already explicitly set.
    pub only_if_unset: bool,
    /// Removes the targeted option entry instead of setting it.
    pub unset: bool,
    /// Unsets pane-local overrides beneath a targeted window before unsetting it.
    pub unset_pane_overrides: bool,
    /// Whether the value should be format-expanded before storage.
    #[serde(default)]
    pub format: bool,
    /// Optional target used to evaluate the format-expanded value.
    #[serde(default)]
    pub format_target: Option<crate::Target>,
}

impl<'de> Deserialize<'de> for SetOptionByNameRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_struct(
            "SetOptionByNameRequest",
            &[
                "scope",
                "name",
                "value",
                "mode",
                "only_if_unset",
                "unset",
                "unset_pane_overrides",
                "format",
                "format_target",
            ],
            SetOptionByNameRequestVisitor,
        )
    }
}

struct SetOptionByNameRequestVisitor;

impl<'de> Visitor<'de> for SetOptionByNameRequestVisitor {
    type Value = SetOptionByNameRequest;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a set-option-by-name request")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let scope = required_next(&mut seq, 0, &self)?;
        let name = required_next(&mut seq, 1, &self)?;
        let value = required_next(&mut seq, 2, &self)?;
        let mode = required_next(&mut seq, 3, &self)?;
        let only_if_unset = required_next(&mut seq, 4, &self)?;
        let unset = required_next(&mut seq, 5, &self)?;
        let unset_pane_overrides = required_next(&mut seq, 6, &self)?;
        let format = compat_next_element(&mut seq)?;
        let format_target = compat_next_element(&mut seq)?;

        Ok(SetOptionByNameRequest {
            scope,
            name,
            value,
            mode,
            only_if_unset,
            unset,
            unset_pane_overrides,
            format,
            format_target,
        })
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut scope = None;
        let mut name = None;
        let mut value = None;
        let mut mode = None;
        let mut only_if_unset = None;
        let mut unset = None;
        let mut unset_pane_overrides = None;
        let mut format = None;
        let mut format_target = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "scope" => scope = Some(map.next_value()?),
                "name" => name = Some(map.next_value()?),
                "value" => value = Some(map.next_value()?),
                "mode" => mode = Some(map.next_value()?),
                "only_if_unset" => only_if_unset = Some(map.next_value()?),
                "unset" => unset = Some(map.next_value()?),
                "unset_pane_overrides" => unset_pane_overrides = Some(map.next_value()?),
                "format" => format = Some(map.next_value()?),
                "format_target" => format_target = Some(map.next_value()?),
                _ => {
                    let _ = map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }

        Ok(SetOptionByNameRequest {
            scope: scope.ok_or_else(|| serde::de::Error::missing_field("scope"))?,
            name: name.ok_or_else(|| serde::de::Error::missing_field("name"))?,
            value: value.ok_or_else(|| serde::de::Error::missing_field("value"))?,
            mode: mode.ok_or_else(|| serde::de::Error::missing_field("mode"))?,
            only_if_unset: only_if_unset
                .ok_or_else(|| serde::de::Error::missing_field("only_if_unset"))?,
            unset: unset.ok_or_else(|| serde::de::Error::missing_field("unset"))?,
            unset_pane_overrides: unset_pane_overrides
                .ok_or_else(|| serde::de::Error::missing_field("unset_pane_overrides"))?,
            format: format.unwrap_or(false),
            format_target: format_target.unwrap_or(None),
        })
    }
}

/// Request payload for `set-environment`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetEnvironmentRequest {
    /// The selected mutation scope.
    pub scope: ScopeSelector,
    /// The environment variable name.
    pub name: String,
    /// The environment variable value.
    pub value: String,
    /// Optional tmux-style mutation mode. `None` preserves legacy set semantics.
    #[serde(default)]
    pub mode: Option<SetEnvironmentMode>,
    /// Whether the stored entry should be hidden from normal display and child inheritance.
    #[serde(default)]
    pub hidden: bool,
    /// Whether the value should be format-expanded before storage.
    #[serde(default)]
    pub format: bool,
}

/// Request payload for `set-hook`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetHookRequest {
    /// The selected mutation scope.
    pub scope: ScopeSelector,
    /// The supported hook name.
    pub hook: HookName,
    /// The shell command string executed by the server.
    pub command: String,
    /// The hook lifecycle semantics.
    pub lifecycle: HookLifecycle,
}

/// Extended request payload for `set-hook`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetHookMutationRequest {
    /// The selected mutation scope.
    pub scope: ScopeSelector,
    /// The supported hook name.
    pub hook: HookName,
    /// The optional shell command string executed by the server.
    pub command: Option<String>,
    /// The hook lifecycle semantics.
    pub lifecycle: HookLifecycle,
    /// Whether the mutation should append to the next free array slot.
    pub append: bool,
    /// Whether the mutation should remove the hook instead of setting it.
    pub unset: bool,
    /// Whether the hook should fire immediately without storing the mutation.
    pub run_immediately: bool,
    /// The optional explicit array index.
    pub index: Option<u32>,
}
