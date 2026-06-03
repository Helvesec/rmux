#[derive(Debug, Clone)]
pub(super) struct CommandAlias {
    name: String,
    value: String,
}

impl CommandAlias {
    pub(super) fn builtin() -> impl Iterator<Item = Self> {
        DEFAULT_COMMAND_ALIASES
            .iter()
            .map(|(name, value)| Self::from_parts(name, value))
    }

    pub(super) fn parse(definition: impl Into<String>) -> Option<Self> {
        let definition = definition.into();
        let (name, value) = definition.split_once('=')?;
        Some(Self {
            name: name.to_owned(),
            value: value.to_owned(),
        })
    }

    fn from_parts(name: &str, value: &str) -> Self {
        Self {
            name: name.to_owned(),
            value: value.to_owned(),
        }
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn value(&self) -> &str {
        &self.value
    }
}

const DEFAULT_COMMAND_ALIASES: &[(&str, &str)] = &[
    ("split-pane", "split-window"),
    ("splitp", "split-window"),
    ("server-info", "show-messages -JT"),
    ("info", "show-messages -JT"),
    ("choose-window", "choose-tree -w"),
    ("choose-session", "choose-tree -s"),
];
