use rmux_proto::{
    DisplayMessageDurationMillis, DisplayMessageExtRequest, DisplayMessageRequest, Request,
    Response, ShowMessagesRequest, Target,
};

use crate::{connection::Connection, ClientError};

impl Connection {
    /// Sends a `display-message` request over the detached RPC channel.
    pub fn display_message(
        &mut self,
        target: Option<Target>,
        print: bool,
        message: Option<String>,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::DisplayMessage(DisplayMessageRequest {
            target,
            print,
            message,
            empty_target_context: false,
        }))
    }

    /// Sends a `display-message -c` request over the detached RPC channel.
    pub fn display_message_ext(
        &mut self,
        target: Option<Target>,
        print: bool,
        message: Option<String>,
        target_client: Option<String>,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::DisplayMessageExt(Box::new(
            DisplayMessageExtRequest {
                target,
                print,
                message,
                target_client,
                empty_target_context: false,
                duration_ms: None,
                ignore_input: false,
            },
        )))
    }

    /// Sends `display-message` with attached-overlay timing and input policy.
    pub fn display_message_with_options(
        &mut self,
        target: Option<Target>,
        print: bool,
        message: Option<String>,
        target_client: Option<String>,
        duration_ms: Option<DisplayMessageDurationMillis>,
        ignore_input: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::DisplayMessageExt(Box::new(
            DisplayMessageExtRequest {
                target,
                print,
                message,
                target_client,
                empty_target_context: false,
                duration_ms,
                ignore_input,
            },
        )))
    }

    /// Sends a `show-messages` request over the detached RPC channel.
    pub fn show_messages(
        &mut self,
        jobs: bool,
        terminals: bool,
        target_client: Option<String>,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::ShowMessages(ShowMessagesRequest {
            jobs,
            terminals,
            target_client,
        }))
    }
}
