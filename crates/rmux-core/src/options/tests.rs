use std::{collections::HashSet, fs};

use super::{
    default_global_scope_for_option_name, registry, resolve_option_name, OptionStore,
    ShowOptionsMode,
};
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{
    OptionName, PaneTarget, RmuxError, ScopeSelector, SessionName, SetOptionMode, WindowTarget,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

#[path = "tests/store_resolution.rs"]
mod store_resolution;

#[path = "tests/show_validation.rs"]
mod show_validation;

#[path = "tests/registry_metadata.rs"]
mod registry_metadata;

#[path = "tests/user_options.rs"]
mod user_options;

#[path = "tests/mutation_unset.rs"]
mod mutation_unset;

#[path = "tests/effects_defaults.rs"]
mod effects_defaults;

#[path = "tests/scope_resolution.rs"]
mod scope_resolution;
