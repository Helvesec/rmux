use std::fmt;

use rmux_proto::{
    MoveWindowTarget, PaneTarget, SelectLayoutTarget, SessionName, SplitWindowTarget, Target,
    WindowTarget,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TargetSpec {
    raw: String,
    exact: Option<Target>,
}

impl TargetSpec {
    pub(crate) fn raw(&self) -> &str {
        &self.raw
    }

    pub(crate) fn exact(&self) -> Option<&Target> {
        self.exact.as_ref()
    }
}

impl fmt::Display for TargetSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.raw)
    }
}

impl PartialEq<SessionName> for TargetSpec {
    fn eq(&self, other: &SessionName) -> bool {
        matches!(self.exact(), Some(Target::Session(session_name)) if session_name == other)
    }
}

impl PartialEq<WindowTarget> for TargetSpec {
    fn eq(&self, other: &WindowTarget) -> bool {
        matches!(self.exact(), Some(Target::Window(target)) if target == other)
    }
}

impl PartialEq<PaneTarget> for TargetSpec {
    fn eq(&self, other: &PaneTarget) -> bool {
        matches!(self.exact(), Some(Target::Pane(target)) if target == other)
    }
}

impl PartialEq<Target> for TargetSpec {
    fn eq(&self, other: &Target) -> bool {
        self.exact().is_some_and(|target| target == other)
    }
}

impl PartialEq<MoveWindowTarget> for TargetSpec {
    fn eq(&self, other: &MoveWindowTarget) -> bool {
        match (self.exact(), other) {
            (Some(Target::Session(session_name)), MoveWindowTarget::Session(other)) => {
                session_name == other
            }
            (Some(Target::Window(target)), MoveWindowTarget::Window(other)) => target == other,
            _ => false,
        }
    }
}

impl PartialEq<SelectLayoutTarget> for TargetSpec {
    fn eq(&self, other: &SelectLayoutTarget) -> bool {
        match (self.exact(), other) {
            (Some(Target::Session(session_name)), SelectLayoutTarget::Session(other)) => {
                session_name == other
            }
            (Some(Target::Window(target)), SelectLayoutTarget::Window(other)) => target == other,
            _ => false,
        }
    }
}

impl PartialEq<SplitWindowTarget> for TargetSpec {
    fn eq(&self, other: &SplitWindowTarget) -> bool {
        match (self.exact(), other) {
            (Some(Target::Session(session_name)), SplitWindowTarget::Session(other)) => {
                session_name == other
            }
            (Some(Target::Pane(target)), SplitWindowTarget::Pane(other)) => target == other,
            _ => false,
        }
    }
}

pub(super) fn parse_session_name(value: &str) -> Result<SessionName, String> {
    SessionName::new(value.to_owned()).map_err(|error| error.to_string())
}

pub(crate) fn parse_target_spec(value: &str) -> Result<TargetSpec, String> {
    match Target::parse(value) {
        Ok(target) => Ok(TargetSpec {
            raw: value.to_owned(),
            exact: Some(target),
        }),
        Err(_) if is_runtime_resolved_target_shape(value) => Ok(TargetSpec {
            raw: value.to_owned(),
            exact: None,
        }),
        Err(error) => Err(error.to_string()),
    }
}

fn is_runtime_resolved_target_shape(value: &str) -> bool {
    !value.is_empty()
}

pub(super) fn parse_target(value: &str) -> Result<Target, String> {
    if let Some(session_name) = value.strip_suffix(':')
        && !session_name.is_empty() {
            return parse_session_name(session_name).map(Target::Session);
        }

    Target::parse(value).map_err(|error| error.to_string())
}
