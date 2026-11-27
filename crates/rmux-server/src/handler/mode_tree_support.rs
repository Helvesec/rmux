use rmux_core::command_parser::ParsedCommand;
use rmux_proto::RmuxError;

use super::RequestHandler;

#[derive(Debug)]
pub(in crate::handler) enum ModeTreeInputError {
    Fatal(RmuxError),
    UserCommandAfterModeExit(RmuxError),
}

impl ModeTreeInputError {
    pub(in crate::handler) fn into_rmux_error(self) -> RmuxError {
        match self {
            Self::Fatal(error) | Self::UserCommandAfterModeExit(error) => error,
        }
    }
}

impl From<RmuxError> for ModeTreeInputError {
    fn from(error: RmuxError) -> Self {
        Self::Fatal(error)
    }
}

mod mode_tree_actions;
mod mode_tree_buffer_actions;
mod mode_tree_build;
mod mode_tree_client_actions;
mod mode_tree_customize_build;
mod mode_tree_dismissal;
mod mode_tree_filter;
mod mode_tree_geometry;
mod mode_tree_input;
mod mode_tree_model;
mod mode_tree_order;
mod mode_tree_origin;
mod mode_tree_parse;
mod mode_tree_preview;
mod mode_tree_prompt;
mod mode_tree_render;
mod mode_tree_runtime;
mod mode_tree_runtime_identity;
mod mode_tree_selection;
mod mode_tree_sort;
#[cfg(test)]
#[path = "../handler_mode_tree/test_support.rs"]
mod mode_tree_test_support;
mod mode_tree_tree_build;
mod mode_tree_tree_kill;

mod mode_tree_buffer_editor;
use self::mode_tree_model::ModeTreeKind;
pub(in crate::handler) use self::mode_tree_model::{ModeTreeActionIdentity, ModeTreePaneIdentity};
pub(super) use self::mode_tree_model::{ModeTreeClientState, ParsedModeTreeCommand};
use self::mode_tree_preview::{mode_tree_preview_lines, preview_lines_for_target};
pub(in crate::handler) use mode_tree_buffer_editor::BufferEditRequest;

const MODE_TREE_HELP: &str =
    "mode-tree: Enter accept  q close  f filter  C-s/C-r search  t tag  C-t tag-all  v preview";
const DEFAULT_KEY_FORMAT: &str =
    "#{?#{e|<:#{line},10},#{line},#{?#{e|<:#{line},36},M-#{a:#{e|+:97,#{e|-:#{line},10}}},}}";
const CHOOSE_TREE_DEFAULT_TEMPLATE: &str = "switch-client -Zt '%%'";
const CHOOSE_BUFFER_DEFAULT_TEMPLATE: &str = "paste-buffer -p -b '%%'";
const CHOOSE_CLIENT_DEFAULT_TEMPLATE: &str = "detach-client -t '%%'";
const SAFE_PROMPT_TEMPLATE: &str = "display-message -p -- '%%'";

impl RequestHandler {
    pub(super) fn parse_mode_tree_queue_command(
        command: ParsedCommand,
    ) -> Result<Option<ParsedModeTreeCommand>, RmuxError> {
        mode_tree_parse::parse_mode_tree_queue_command(command)
    }
}

fn default_template(kind: ModeTreeKind) -> Option<String> {
    match kind {
        ModeTreeKind::Tree => Some(CHOOSE_TREE_DEFAULT_TEMPLATE.to_owned()),
        ModeTreeKind::Buffer => Some(CHOOSE_BUFFER_DEFAULT_TEMPLATE.to_owned()),
        ModeTreeKind::Client => Some(CHOOSE_CLIENT_DEFAULT_TEMPLATE.to_owned()),
        ModeTreeKind::Customize => None,
    }
}

#[cfg(test)]
#[path = "../handler_mode_tree/tests.rs"]
mod mode_tree_tests;
