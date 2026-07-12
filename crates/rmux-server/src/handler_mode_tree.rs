use rmux_core::command_parser::ParsedCommand;
use rmux_proto::RmuxError;

use super::RequestHandler;

#[path = "handler_mode_tree/actions.rs"]
mod mode_tree_actions;
#[path = "handler_mode_tree/build.rs"]
mod mode_tree_build;
#[path = "handler_mode_tree/client_actions.rs"]
mod mode_tree_client_actions;
#[path = "handler_mode_tree/customize_build.rs"]
mod mode_tree_customize_build;
#[path = "handler_mode_tree/filter.rs"]
mod mode_tree_filter;
#[path = "handler_mode_tree/input.rs"]
mod mode_tree_input;
#[path = "handler_mode_tree/model.rs"]
mod mode_tree_model;
#[path = "handler_mode_tree/order.rs"]
mod mode_tree_order;
#[path = "handler_mode_tree/parse.rs"]
mod mode_tree_parse;
#[path = "handler_mode_tree/preview.rs"]
mod mode_tree_preview;
#[path = "handler_mode_tree/prompt.rs"]
mod mode_tree_prompt;
#[path = "handler_mode_tree/render.rs"]
mod mode_tree_render;
#[path = "handler_mode_tree/runtime.rs"]
mod mode_tree_runtime;
#[path = "handler_mode_tree/selection.rs"]
mod mode_tree_selection;
#[path = "handler_mode_tree/sort.rs"]
mod mode_tree_sort;
#[path = "handler_mode_tree/tree_build.rs"]
mod mode_tree_tree_build;
#[path = "handler_mode_tree/tree_kill.rs"]
mod mode_tree_tree_kill;

use self::mode_tree_model::ModeTreeKind;
pub(super) use self::mode_tree_model::{ModeTreeClientState, ParsedModeTreeCommand};
use self::mode_tree_preview::{mode_tree_preview_lines, preview_lines_for_target};

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
#[path = "handler_mode_tree/tests.rs"]
mod mode_tree_tests;
