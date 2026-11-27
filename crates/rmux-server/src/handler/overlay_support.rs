use rmux_proto::RmuxError;

use super::scripting_support::{QueueCommandAction, QueueExecutionContext};
use super::RequestHandler;

mod admission;
mod buffer_editor;
mod clear;
mod commands;
mod completion;
#[cfg(all(test, unix))]
#[path = "../handler_overlay/completion_order_tests.rs"]
mod completion_order_tests;
#[cfg(all(test, unix))]
#[path = "../handler_overlay/completion_tests.rs"]
mod completion_tests;
#[cfg(all(test, unix))]
#[path = "../handler_overlay/exit_policy_tests.rs"]
mod exit_policy_tests;
mod identity;
#[cfg(test)]
#[path = "../handler_overlay/identity_tests.rs"]
mod identity_tests;
mod input_routing;
mod interactions;
mod interactions_popup_menu;
#[cfg(all(test, unix))]
#[path = "../handler_overlay/job_directory_tests.rs"]
mod job_directory_tests;
#[cfg(all(test, unix))]
#[path = "../handler_overlay/job_environment_tests.rs"]
mod job_environment_tests;
mod parse;
#[cfg(test)]
#[path = "../handler_overlay/parse_command_tests.rs"]
mod parse_command_tests;
#[cfg(test)]
#[path = "../handler_overlay/parse_environment_tests.rs"]
mod parse_environment_tests;
mod popup_install;
pub(super) use parse::ParsedOverlayCommand;
use parse::{parse_display_menu, parse_display_popup};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AttachedOverlayInput {
    Consumed,
    Reroute(Vec<u8>),
}

mod layout;
mod menu;
mod menu_command;
mod menu_outcome;
use menu::MenuOverlayItem;
#[cfg(all(test, unix))]
#[path = "../handler_overlay/inline_surface_tests.rs"]
mod inline_surface_tests;
mod mouse;
mod popup_io;
mod popup_job;
mod popup_lifecycle;
#[cfg(all(test, unix))]
#[path = "../handler_overlay/queued_continuity_tests.rs"]
mod queued_continuity_tests;
#[cfg(all(test, unix))]
#[path = "../handler_overlay/queued_destination_tests.rs"]
mod queued_destination_tests;
#[cfg(all(test, unix))]
#[path = "../handler_overlay/queued_drain_tests.rs"]
mod queued_drain_tests;
#[cfg(all(test, unix))]
#[path = "../handler_overlay/queued_focus_tests.rs"]
mod queued_focus_tests;
#[cfg(all(test, unix))]
#[path = "../handler_overlay/queued_input_tests.rs"]
mod queued_input_tests;
#[cfg(all(test, unix))]
#[path = "../handler_overlay/queued_paste_bound_tests.rs"]
mod queued_paste_bound_tests;
#[cfg(all(test, unix))]
#[path = "../handler_overlay/queued_rest_paste_tests.rs"]
mod queued_rest_paste_tests;
mod refresh;
mod resize;
mod scrollable;
pub(in crate::handler) use scrollable::AttachedHelpContext;
#[cfg(all(test, unix))]
#[path = "../handler_overlay/second_popup_tests.rs"]
mod second_popup_tests;
#[cfg(all(test, unix))]
#[path = "../handler_overlay/size_admission_tests.rs"]
mod size_admission_tests;
mod state;
pub(super) use state::{ClientOverlayState, PopupOverlayState};
mod support;
mod target;

impl RequestHandler {
    pub(super) fn parse_overlay_queue_command(
        command_name: &str,
        arguments: Vec<String>,
    ) -> Result<Option<ParsedOverlayCommand>, RmuxError> {
        match command_name {
            "display-menu" | "menu" => parse_display_menu(arguments)
                .map(|command| Some(ParsedOverlayCommand::Menu(command))),
            "display-popup" | "popup" => parse_display_popup(arguments)
                .map(|command| Some(ParsedOverlayCommand::Popup(command))),
            _ => Ok(None),
        }
    }

    pub(super) async fn execute_queued_overlay(
        &self,
        requester_pid: u32,
        command: ParsedOverlayCommand,
        context: &QueueExecutionContext,
    ) -> Result<QueueCommandAction, RmuxError> {
        match command {
            ParsedOverlayCommand::Menu(command) => {
                self.execute_queued_display_menu(requester_pid, command, context)
                    .await
            }
            ParsedOverlayCommand::Popup(command) => {
                self.execute_queued_display_popup(requester_pid, command, context)
                    .await
            }
        }
    }
}
