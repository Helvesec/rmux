use rmux_proto::RmuxError;
use tokio::sync::oneshot;

use super::super::scripting_support::QueueExecutionContext;
use super::super::RequesterOrigin;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in super::super) enum PromptType {
    Command,
    Search,
    Target,
    WindowTarget,
}

impl PromptType {
    /// Canonical ordering mirrors tmux `PROMPT_TYPE_*` so iteration over every prompt
    /// type emits rows in the same order tmux prints them.
    pub(in super::super) const ALL: [Self; 4] = [
        Self::Command,
        Self::Search,
        Self::Target,
        Self::WindowTarget,
    ];

    pub(in super::super) fn parse(value: &str) -> Option<Self> {
        match value {
            "command" => Some(Self::Command),
            "search" => Some(Self::Search),
            "target" => Some(Self::Target),
            "window-target" => Some(Self::WindowTarget),
            _ => None,
        }
    }

    pub(super) fn index(self) -> usize {
        match self {
            Self::Command => 0,
            Self::Search => 1,
            Self::Target => 2,
            Self::WindowTarget => 3,
        }
    }

    pub(in super::super) fn label(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Search => "search",
            Self::Target => "target",
            Self::WindowTarget => "window-target",
        }
    }
}

#[derive(Debug, Clone)]
pub(in super::super) struct PromptField {
    pub(in super::super) prompt: String,
    pub(in super::super) input: String,
}

#[derive(Debug, Clone)]
pub(in super::super) struct CommandPromptPlan {
    pub(in super::super) origin: RequesterOrigin,
    pub(in super::super) target_client: Option<String>,
    pub(in super::super) context: QueueExecutionContext,
    pub(in super::super) fields: Vec<PromptField>,
    pub(in super::super) template: String,
    pub(in super::super) flags: u8,
    pub(in super::super) prompt_type: PromptType,
    pub(in super::super) background: bool,
    pub(in super::super) format_values: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub(in super::super) struct ConfirmBeforePlan {
    pub(in super::super) origin: RequesterOrigin,
    pub(in super::super) target_client: Option<String>,
    pub(in super::super) context: QueueExecutionContext,
    pub(in super::super) prompt: String,
    pub(in super::super) template: String,
    pub(in super::super) confirm_key: char,
    pub(in super::super) default_yes: bool,
    pub(in super::super) background: bool,
    pub(in super::super) format_values: Vec<(String, String)>,
}

#[derive(Debug)]
pub(in super::super) enum PromptStartOutcome {
    Immediate,
    Waiting(oneshot::Receiver<PromptQueueResult>),
}

#[derive(Debug)]
pub(in super::super) struct PromptQueueResult {
    pub(in super::super) inserted: Option<(
        rmux_core::command_parser::ParsedCommands,
        QueueExecutionContext,
    )>,
    pub(in super::super) error: Option<RmuxError>,
    pub(in super::super) responses: Option<Vec<String>>,
    pub(in super::super) origin: Option<RequesterOrigin>,
}

impl PromptQueueResult {
    pub(in super::super) const fn noop() -> Self {
        Self {
            inserted: None,
            error: None,
            responses: None,
            origin: None,
        }
    }

    pub(in super::super) const fn cancelled(origin: RequesterOrigin) -> Self {
        Self {
            inserted: None,
            error: None,
            responses: None,
            origin: Some(origin),
        }
    }
}
