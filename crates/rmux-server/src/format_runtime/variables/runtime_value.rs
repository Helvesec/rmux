//! Typed answers of the runtime lookup tiers.

/// What one tier of the runtime lookup really answered.
///
/// This server has always rendered some known names as the empty string where
/// it has no value for them: a client name with no client, a session group
/// where there is no group, a configuration text it was never given. Those
/// empty answers are kept exactly as they are, but they are not values, and the
/// difference matters to `#{b:…}` and `#{d:…}`, which the engine measured runs
/// only on a value it really found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RuntimeValue {
    /// A value this tier really has, even when it holds nothing.
    Found(String),
    /// The empty answer this server has always given where it has none. It is
    /// terminal: no later tier - an option, the environment - speaks for that
    /// name, exactly as none did before.
    CompatibilityEmpty,
}

impl RuntimeValue {
    /// What the public lookup has always rendered: the value, or the empty
    /// string for the legacy stop.
    pub(super) fn into_public(self) -> String {
        match self {
            Self::Found(value) => value,
            Self::CompatibilityEmpty => String::new(),
        }
    }

    /// What a path modifier may read: only a value that is really there.
    pub(super) fn into_found(self) -> Option<String> {
        match self {
            Self::Found(value) => Some(value),
            Self::CompatibilityEmpty => None,
        }
    }
}

/// Carries a dispatcher arm's answer into [`RuntimeValue`].
///
/// Every arm that already answered `Option<String>` keeps its body byte for
/// byte and becomes a found value; only the arms that manufacture an empty
/// answer name the stop themselves, by answering `Option<RuntimeValue>`.
pub(super) trait IntoRuntimeValue {
    fn into_runtime_value(self) -> Option<RuntimeValue>;
}

impl IntoRuntimeValue for Option<String> {
    fn into_runtime_value(self) -> Option<RuntimeValue> {
        self.map(RuntimeValue::Found)
    }
}

impl IntoRuntimeValue for Option<RuntimeValue> {
    fn into_runtime_value(self) -> Option<RuntimeValue> {
        self
    }
}
