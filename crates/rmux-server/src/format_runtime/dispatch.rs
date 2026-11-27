// Generate coverage from the actual arms: an unavailable value is still routed.
macro_rules! runtime_dispatch {
    (
        $visibility:vis fn $name:ident(&$this:ident, $argument:ident: $argument_type:ty)
        => $key:expr;
        coverage $coverage:ident;
        { $($pattern:pat => $value:expr),* $(,)? }
    ) => {
        $visibility fn $name(&$this, $argument: $argument_type) -> Option<String> {
            use rmux_core::formats::FormatVariableKey::*;
            match $key {
                $($pattern => $value,)*
                _ => None,
            }
        }

        #[cfg(test)]
        pub(super) fn $coverage(key: rmux_core::formats::FormatVariableKey) -> bool {
            use rmux_core::formats::FormatVariableKey::*;
            matches!(key, $($pattern)|*)
        }
    };
}

pub(super) use runtime_dispatch;

// The same generated routes and the same coverage, for a dispatcher whose
// answer the caller can tell apart: a value it really has, or the empty answer
// this server has always given where it has none. An arm that says nothing
// still says nothing, and every arm that already answered `Option<String>` is
// carried through unchanged - only the arms that manufacture an empty value
// name that stop themselves.
macro_rules! runtime_dispatch_typed {
    (
        $visibility:vis fn $name:ident(&$this:ident, $argument:ident: $argument_type:ty)
        => $key:expr;
        coverage $coverage:ident;
        { $($pattern:pat => $value:expr),* $(,)? }
    ) => {
        $visibility fn $name(&$this, $argument: $argument_type) -> Option<RuntimeValue> {
            use rmux_core::formats::FormatVariableKey::*;
            match $key {
                $($pattern => IntoRuntimeValue::into_runtime_value($value),)*
                _ => None,
            }
        }

        #[cfg(test)]
        pub(super) fn $coverage(key: rmux_core::formats::FormatVariableKey) -> bool {
            use rmux_core::formats::FormatVariableKey::*;
            matches!(key, $($pattern)|*)
        }
    };
}

pub(super) use runtime_dispatch_typed;

#[cfg(test)]
#[path = "expression_whitespace_tests.rs"]
mod expression_whitespace_tests;

#[cfg(test)]
#[path = "expression_unicode_tests.rs"]
mod expression_unicode_tests;
