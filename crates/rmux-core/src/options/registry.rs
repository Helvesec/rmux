use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{OptionName, RmuxError};

pub(crate) const SCOPE_SERVER: u8 = 0x1;
pub(crate) const SCOPE_SESSION: u8 = 0x2;
pub(crate) const SCOPE_WINDOW: u8 = 0x4;
pub(crate) const SCOPE_PANE: u8 = 0x8;

pub(crate) const SHOW_SERVER: u8 = 0x1;
pub(crate) const SHOW_SESSION: u8 = 0x2;
pub(crate) const SHOW_WINDOW: u8 = 0x4;
pub(crate) const SHOW_PANE: u8 = 0x8;

pub(crate) const EFFECT_NONE: OptionChangeMask = OptionChangeMask(0);
pub(crate) const EFFECT_RENDER: OptionChangeMask = OptionChangeMask(0x1);
pub(crate) const EFFECT_PANE_STYLE: OptionChangeMask = OptionChangeMask(0x2);
pub(crate) const EFFECT_TRANSCRIPT_LIMIT: OptionChangeMask = OptionChangeMask(0x4);
pub(crate) const EFFECT_STYLE_PARSE: OptionChangeMask = OptionChangeMask(0x8);
pub(crate) const EFFECT_TERMINAL_FEATURES: OptionChangeMask = OptionChangeMask(0x10);
pub(crate) const EFFECT_KEY_TABLE: OptionChangeMask = OptionChangeMask(0x20);
pub(crate) const EFFECT_USER_KEYS: OptionChangeMask = OptionChangeMask(0x40);
pub(crate) const EFFECT_CURSOR: OptionChangeMask = OptionChangeMask(0x80);
pub(crate) const EFFECT_FILL_CHARACTER: OptionChangeMask = OptionChangeMask(0x100);
pub(crate) const EFFECT_STATUS_TIMER: OptionChangeMask = OptionChangeMask(0x200);
pub(crate) const EFFECT_ALERTS: OptionChangeMask = OptionChangeMask(0x400);
pub(crate) const EFFECT_LAYOUT: OptionChangeMask = OptionChangeMask(0x800);
pub(crate) const EFFECT_SCROLLBAR_STYLE: OptionChangeMask = OptionChangeMask(0x1000);
pub(crate) const EFFECT_CODEPOINT_WIDTHS: OptionChangeMask = OptionChangeMask(0x2000);
pub(crate) const EFFECT_INPUT_BUFFER_SIZE: OptionChangeMask = OptionChangeMask(0x4000);
pub(crate) const EFFECT_PANE_COLOURS: OptionChangeMask = OptionChangeMask(0x8000);
pub(crate) const EFFECT_AUTOMATIC_RENAME: OptionChangeMask = OptionChangeMask(0x10000);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GlobalRoot {
    Server,
    Session,
    Window,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OptionValueType {
    String,
    Number { minimum: u32 },
    Key,
    Colour,
    Flag,
    Choice(&'static [&'static str]),
    Command,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DefaultValue {
    Scalar(&'static str),
    Array(&'static [&'static str]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptionChangeMask(u32);

impl OptionChangeMask {
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns whether the mask includes alert-timer side effects.
    #[must_use]
    pub const fn affects_alerts(self) -> bool {
        self.contains(EFFECT_ALERTS)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OptionMetadata {
    option: OptionName,
    name: &'static str,
    aliases: &'static [&'static str],
    show_mask: u8,
    scope_mask: u8,
    global_root: GlobalRoot,
    value_type: OptionValueType,
    default: DefaultValue,
    separator: &'static str,
    is_array: bool,
    effects: OptionChangeMask,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionQuery {
    name: String,
    metadata: Option<&'static OptionMetadata>,
    index: Option<u32>,
}

#[path = "table.rs"]
mod table;
use table::OPTIONS;

impl OptionMetadata {
    pub(crate) fn option(&self) -> OptionName {
        self.option
    }

    pub(crate) const fn name(&self) -> &'static str {
        self.name
    }

    pub(crate) const fn aliases(&self) -> &'static [&'static str] {
        self.aliases
    }

    pub(crate) const fn scope_mask(&self) -> u8 {
        self.scope_mask
    }

    pub(crate) const fn global_root(&self) -> GlobalRoot {
        self.global_root
    }

    pub(crate) const fn value_type(&self) -> OptionValueType {
        self.value_type
    }

    pub(crate) const fn default_value(&self) -> DefaultValue {
        self.default
    }

    pub(crate) const fn separator(&self) -> &'static str {
        self.separator
    }

    pub(crate) const fn is_array(&self) -> bool {
        self.is_array
    }

    pub(crate) const fn effects(&self) -> OptionChangeMask {
        self.effects
    }

    pub(crate) const fn visible_in(&self, scope: u8) -> bool {
        (self.show_mask & scope) != 0
    }

    pub(crate) const fn supports_scope(&self, scope: &OptionScopeSelector) -> bool {
        let mask = match scope {
            OptionScopeSelector::ServerGlobal => SCOPE_SERVER,
            OptionScopeSelector::SessionGlobal | OptionScopeSelector::Session(_) => SCOPE_SESSION,
            OptionScopeSelector::WindowGlobal | OptionScopeSelector::Window(_) => SCOPE_WINDOW,
            OptionScopeSelector::Pane(_) => SCOPE_PANE,
        };
        (self.scope_mask & mask) != 0
    }
}

impl OptionQuery {
    pub(crate) fn known(option: OptionName) -> Self {
        let metadata = option_metadata(option);
        Self {
            name: metadata.name().to_owned(),
            metadata: Some(metadata),
            index: None,
        }
    }

    pub(crate) fn metadata(&self) -> Option<&'static OptionMetadata> {
        self.metadata
    }

    pub(crate) fn canonical_name(&self) -> &str {
        &self.name
    }

    pub(crate) fn index(&self) -> Option<u32> {
        self.index
    }

    pub(crate) fn known_option(&self) -> Option<OptionName> {
        self.metadata.map(OptionMetadata::option)
    }

    pub fn is_user(&self) -> bool {
        self.metadata.is_none()
    }

    #[must_use]
    pub fn supports_scope(&self, scope: &OptionScopeSelector) -> bool {
        self.metadata
            .map(|metadata| metadata.supports_scope(scope))
            .unwrap_or(true)
    }

    pub(crate) fn is_array(&self) -> bool {
        self.metadata.is_some_and(OptionMetadata::is_array)
    }

    pub(crate) fn value_type(&self) -> OptionValueType {
        self.metadata
            .map(OptionMetadata::value_type)
            .unwrap_or(OptionValueType::String)
    }

    pub(crate) fn separator(&self) -> &'static str {
        self.metadata
            .map(OptionMetadata::separator)
            .unwrap_or_default()
    }

    pub(crate) fn default_value(&self) -> Option<DefaultValue> {
        self.metadata.map(OptionMetadata::default_value)
    }

    pub(crate) fn effects(&self) -> OptionChangeMask {
        self.metadata
            .map(OptionMetadata::effects)
            .unwrap_or(EFFECT_PANE_STYLE)
    }
}

/// Returns the known option registry.
pub(crate) fn registry() -> &'static [OptionMetadata] {
    OPTIONS
}

pub(crate) fn option_metadata(option: OptionName) -> &'static OptionMetadata {
    OPTIONS
        .iter()
        .find(|metadata| metadata.option == option)
        .expect("every OptionName variant must have registry metadata")
}

/// Resolves a known option name using tmux-style aliasing and prefix matching.
pub fn resolve_option_name(name: &str) -> Result<OptionQuery, RmuxError> {
    let (base_name, index) = split_array_index(name)?;

    if base_name.starts_with('@') {
        if index.is_some() {
            return Err(RmuxError::Server(format!(
                "user option does not support array indexes: {name}"
            )));
        }
        return Ok(OptionQuery {
            name: base_name.to_owned(),
            metadata: None,
            index: None,
        });
    }

    if let Some(metadata) = OPTIONS.iter().find(|metadata| metadata.name == base_name) {
        return Ok(OptionQuery {
            name: metadata.name().to_owned(),
            metadata: Some(metadata),
            index,
        });
    }

    if let Some(metadata) = OPTIONS
        .iter()
        .find(|metadata| metadata.aliases().contains(&base_name))
    {
        return Ok(OptionQuery {
            name: metadata.name().to_owned(),
            metadata: Some(metadata),
            index,
        });
    }

    // Prefix matching: check canonical names and aliases for unambiguous prefix.
    let mut matches: Vec<&OptionMetadata> = OPTIONS
        .iter()
        .filter(|metadata| {
            metadata.name().starts_with(base_name)
                || metadata
                    .aliases()
                    .iter()
                    .any(|alias| alias.starts_with(base_name))
        })
        .collect();
    matches.dedup_by_key(|metadata| metadata.name());
    match matches.as_slice() {
        [metadata] => Ok(OptionQuery {
            name: metadata.name().to_owned(),
            metadata: Some(metadata),
            index,
        }),
        [] => Err(RmuxError::Server(format!("invalid option: {base_name}"))),
        _ => Err(RmuxError::Server(format!("ambiguous option: {base_name}"))),
    }
}

/// Resolves a known option name using only exact canonical names and aliases.
pub(crate) fn resolve_exact_option_name(name: &str) -> Result<OptionQuery, RmuxError> {
    let (base_name, index) = split_array_index(name)?;

    if base_name.starts_with('@') {
        if index.is_some() {
            return Err(RmuxError::Server(format!(
                "user option does not support array indexes: {name}"
            )));
        }
        return Ok(OptionQuery {
            name: base_name.to_owned(),
            metadata: None,
            index: None,
        });
    }

    if let Some(metadata) = OPTIONS.iter().find(|metadata| metadata.name == base_name) {
        return Ok(OptionQuery {
            name: metadata.name().to_owned(),
            metadata: Some(metadata),
            index,
        });
    }

    if let Some(metadata) = OPTIONS
        .iter()
        .find(|metadata| metadata.aliases().contains(&base_name))
    {
        return Ok(OptionQuery {
            name: metadata.name().to_owned(),
            metadata: Some(metadata),
            index,
        });
    }

    Err(RmuxError::Server(format!("invalid option: {base_name}")))
}

/// Resolves a known option by name when the caller only accepts registered options.
#[must_use]
pub fn option_name_by_name(name: &str) -> Option<OptionName> {
    resolve_option_name(name).ok()?.known_option()
}

/// Returns whether a known option change may affect attached rendering.
#[must_use]
pub fn option_affects_rendering(option: OptionName) -> bool {
    let effects = option_metadata(option).effects();
    effects.contains(EFFECT_RENDER)
        || effects.contains(EFFECT_PANE_STYLE)
        || effects.contains(EFFECT_STYLE_PARSE)
        || effects.contains(EFFECT_CODEPOINT_WIDTHS)
        || effects.contains(EFFECT_CURSOR)
        || effects.contains(EFFECT_FILL_CHARACTER)
        || effects.contains(EFFECT_STATUS_TIMER)
        || effects.contains(EFFECT_LAYOUT)
        || effects.contains(EFFECT_SCROLLBAR_STYLE)
        || effects.contains(EFFECT_PANE_COLOURS)
}

/// Returns whether a known option change may affect alert runtime state.
#[must_use]
pub fn option_affects_alerts(option: OptionName) -> bool {
    option_metadata(option).effects().affects_alerts()
}

fn split_array_index(name: &str) -> Result<(&str, Option<u32>), RmuxError> {
    let Some(start) = name.rfind('[') else {
        if name.contains(']') {
            return Err(RmuxError::Server(format!(
                "invalid option index syntax: {name}"
            )));
        }
        return Ok((name, None));
    };

    if !name.ends_with(']') {
        return Err(RmuxError::Server(format!(
            "invalid option index syntax: {name}"
        )));
    }

    let base_name = &name[..start];
    let index_text = &name[start + 1..name.len() - 1];
    if base_name.is_empty() || index_text.is_empty() {
        return Err(RmuxError::Server(format!(
            "invalid option index syntax: {name}"
        )));
    }

    let index = index_text
        .parse::<u32>()
        .map_err(|_| RmuxError::Server(format!("invalid option index syntax: {name}")))?;
    Ok((base_name, Some(index)))
}
