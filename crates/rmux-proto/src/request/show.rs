use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{HookName, OptionScopeSelector, ScopeSelector};

use super::compat::{compat_next_element, required_next};

/// Request payload for `show-options`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShowOptionsRequest {
    /// The selected option scope.
    pub scope: OptionScopeSelector,
    /// Optional exact option name to display.
    #[serde(default)]
    pub name: Option<String>,
    /// Whether output should contain only values.
    pub value_only: bool,
    /// Whether inherited options should be included, matching `show-options -A`.
    #[serde(default)]
    pub include_inherited: bool,
    /// Whether option lookup errors should be suppressed.
    #[serde(default)]
    pub quiet: bool,
    /// Whether hook options should be included, matching `show-options -H`.
    #[serde(default)]
    pub include_hooks: bool,
}

impl<'de> Deserialize<'de> for ShowOptionsRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_struct(
            "ShowOptionsRequest",
            &[
                "scope",
                "name",
                "value_only",
                "include_inherited",
                "quiet",
                "include_hooks",
            ],
            ShowOptionsRequestVisitor,
        )
    }
}

struct ShowOptionsRequestVisitor;

impl<'de> Visitor<'de> for ShowOptionsRequestVisitor {
    type Value = ShowOptionsRequest;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a show-options request")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let scope = required_next(&mut seq, 0, &self)?;
        let name = required_next(&mut seq, 1, &self)?;
        let value_only = required_next(&mut seq, 2, &self)?;
        let include_inherited = compat_next_element(&mut seq)?;
        let quiet = compat_next_element(&mut seq)?;
        let include_hooks = compat_next_element(&mut seq)?;

        Ok(ShowOptionsRequest {
            scope,
            name,
            value_only,
            include_inherited,
            quiet,
            include_hooks,
        })
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut scope = None;
        let mut name = None;
        let mut value_only = None;
        let mut include_inherited = None;
        let mut quiet = None;
        let mut include_hooks = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "scope" => scope = Some(map.next_value()?),
                "name" => name = Some(map.next_value()?),
                "value_only" => value_only = Some(map.next_value()?),
                "include_inherited" => include_inherited = Some(map.next_value()?),
                "quiet" => quiet = Some(map.next_value()?),
                "include_hooks" => include_hooks = Some(map.next_value()?),
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }

        Ok(ShowOptionsRequest {
            scope: scope.ok_or_else(|| de::Error::missing_field("scope"))?,
            name: name.unwrap_or_default(),
            value_only: value_only.ok_or_else(|| de::Error::missing_field("value_only"))?,
            include_inherited: include_inherited.unwrap_or_default(),
            quiet: quiet.unwrap_or_default(),
            include_hooks: include_hooks.unwrap_or_default(),
        })
    }
}

/// Request payload for `show-environment`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShowEnvironmentRequest {
    /// The selected environment scope.
    pub scope: ScopeSelector,
    /// Optional exact variable to display.
    #[serde(default)]
    pub name: Option<String>,
    /// Whether only hidden variables should be displayed.
    #[serde(default)]
    pub hidden: bool,
    /// Whether output should use shell-export syntax.
    #[serde(default)]
    pub shell_format: bool,
}

/// Request payload for `show-hooks`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShowHooksRequest {
    /// The selected hook scope.
    pub scope: ScopeSelector,
    /// Whether the query should use the window hook table.
    pub window: bool,
    /// Whether the query should use the pane hook table.
    pub pane: bool,
    /// The optional specific hook to display.
    pub hook: Option<HookName>,
}
