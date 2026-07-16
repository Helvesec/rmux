use std::pin::Pin;
use std::task::{Context, Poll};

use rmux_proto::{Response, SdkWaitId};
use tokio::sync::oneshot;

use super::failure::TransportFailure;
use crate::Result;

pub(crate) struct PendingResponse {
    operation: String,
    response: oneshot::Receiver<Result<Response>>,
}

impl PendingResponse {
    pub(super) fn new(operation: String, response: oneshot::Receiver<Result<Response>>) -> Self {
        Self {
            operation,
            response,
        }
    }
}

impl std::future::Future for PendingResponse {
    type Output = Result<Response>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.response).poll(cx) {
            Poll::Ready(Ok(result)) => Poll::Ready(result),
            Poll::Ready(Err(_)) => Poll::Ready(Err(
                TransportFailure::actor_closed().to_error(&self.operation)
            )),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub(super) struct PendingCall {
    command_name: &'static str,
    operation: String,
    reply: Option<oneshot::Sender<Result<Response>>>,
    armed: Option<oneshot::Sender<core::result::Result<(), TransportFailure>>>,
    armed_wait_id: Option<SdkWaitId>,
}

impl PendingCall {
    pub(super) fn reply(
        command_name: &'static str,
        operation: String,
        reply: oneshot::Sender<Result<Response>>,
    ) -> Self {
        Self {
            command_name,
            operation,
            reply: Some(reply),
            armed: None,
            armed_wait_id: None,
        }
    }

    pub(super) fn armed_reply(
        command_name: &'static str,
        operation: String,
        reply: oneshot::Sender<Result<Response>>,
        armed: oneshot::Sender<core::result::Result<(), TransportFailure>>,
        wait_id: SdkWaitId,
    ) -> Self {
        Self {
            command_name,
            operation,
            reply: Some(reply),
            armed: Some(armed),
            armed_wait_id: Some(wait_id),
        }
    }

    pub(super) fn discard(command_name: &'static str, operation: String) -> Self {
        Self {
            command_name,
            operation,
            reply: None,
            armed: None,
            armed_wait_id: None,
        }
    }

    pub(super) fn validate_response(
        &self,
        response: &Response,
    ) -> core::result::Result<(), TransportFailure> {
        if response.is_error() {
            return Ok(());
        }

        let actual = response.command_name();
        if self.command_name == actual {
            return Ok(());
        }

        // The pane-output cursor endpoint is the one daemon RPC that resolves
        // to two distinct response variants: a regular cursor batch
        // (`pane-output-cursor`) or a lag notice (`pane-output-lag`) when the
        // server-side receiver detected a sequence gap. Both are valid
        // replies for the same `pane-output-cursor` request.
        if self.command_name == "pane-output-cursor" && actual == "pane-output-lag" {
            return Ok(());
        }
        if self.command_name == "pane-state-cursor" && actual == "pane-state-lag" {
            return Ok(());
        }

        Err(TransportFailure::mismatched_response(
            self.command_name,
            actual,
        ))
    }

    pub(super) fn accept_response(
        &mut self,
        response: &Response,
    ) -> core::result::Result<PendingResponseAction, TransportFailure> {
        if self.accept_armed_response(response)? {
            return Ok(PendingResponseAction::KeepPending);
        }
        self.validate_response(response)?;
        Ok(PendingResponseAction::Complete)
    }

    pub(super) fn complete(mut self, response: Response) {
        self.send_armed_success_if_pending();
        if let Some(reply) = self.reply {
            let _ = reply.send(response_to_result(response));
        }
    }

    pub(super) fn fail(mut self, failure: &TransportFailure) {
        self.send_armed_failure_if_pending(failure);
        if let Some(reply) = self.reply {
            let error = failure.to_error_for_command(&self.operation, self.command_name);
            let _ = reply.send(Err(error));
        }
    }

    fn accept_armed_response(
        &mut self,
        response: &Response,
    ) -> core::result::Result<bool, TransportFailure> {
        let Some(expected_wait_id) = self.armed_wait_id else {
            return Ok(false);
        };
        let Response::CancelSdkWait(response) = response else {
            return Ok(false);
        };
        if !response.is_armed_ack_for(expected_wait_id) {
            if !response.removed {
                return Err(TransportFailure::invalid_data(format!(
                    "rmux daemon sent SDK wait armed ack id {} for pending wait id {}",
                    response.wait_id.as_u64(),
                    expected_wait_id.as_u64()
                )));
            }
            return Ok(false);
        }
        self.armed_wait_id = None;
        self.send_armed_success_if_pending();
        Ok(true)
    }

    fn send_armed_success_if_pending(&mut self) {
        if let Some(armed) = self.armed.take() {
            let _ = armed.send(Ok(()));
        }
    }

    fn send_armed_failure_if_pending(&mut self, failure: &TransportFailure) {
        if let Some(armed) = self.armed.take() {
            let _ = armed.send(Err(failure.clone()));
        }
    }
}

pub(super) enum PendingResponseAction {
    Complete,
    KeepPending,
}

fn response_to_result(response: Response) -> Result<Response> {
    match response {
        Response::Error(error) => Err(error.into()),
        response => Ok(response),
    }
}
