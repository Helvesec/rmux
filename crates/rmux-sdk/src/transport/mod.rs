//! Crate-private Tokio transport actor for detached SDK RPC.

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use rmux_proto::{Request, Response, SdkWaitId, SdkWaitOwnerId};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};

use crate::{Result, RmuxError};

mod actor;
mod cancellation;
mod deadline;
mod failure;
mod pending;
mod state;

use actor::{request_operation, run_actor, sdk_wait_id_for_request, ActorMessage};
use cancellation::OrderedResponseGuard;
pub(crate) use deadline::OperationDeadline;
use failure::TransportFailure;
pub(crate) use pending::PendingResponse;
use state::TransportState;
#[cfg(test)]
use state::{allocate_bounded_atomic_id, mix_sdk_wait_owner_id};

const ACTOR_QUEUE_CAPACITY: usize = 64;
const TRANSPORT_SHUTDOWN_OPERATION: &str = "shut down rmux SDK transport";

#[derive(Clone)]
pub(crate) struct TransportClient {
    commands: mpsc::Sender<ActorMessage>,
    actor: tokio::task::AbortHandle,
    state: Arc<TransportState>,
    default_timeout: Option<Duration>,
    operation_deadline: Option<OperationDeadline>,
    #[cfg(test)]
    fixture_transport: bool,
}

impl TransportClient {
    pub(crate) fn spawn<S>(stream: S) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (commands, receiver) = mpsc::channel(ACTOR_QUEUE_CAPACITY);
        let state = Arc::new(TransportState::default());
        let actor = tokio::spawn(run_actor(stream, receiver, state.clone())).abort_handle();
        Self {
            commands,
            actor,
            state,
            default_timeout: None,
            operation_deadline: None,
            #[cfg(test)]
            fixture_transport: false,
        }
    }

    /// Returns a reusable client whose individual requests use `timeout`.
    /// Any prior operation scope is deliberately cleared.
    pub(crate) fn with_default_timeout(&self, timeout: Option<Duration>) -> Self {
        let mut client = self.clone();
        client.default_timeout = timeout;
        client.operation_deadline = None;
        client
    }

    /// Clears an operation scope while preserving the reusable timeout.
    pub(crate) fn reusable(&self) -> Self {
        self.with_default_timeout(self.default_timeout)
    }

    /// Returns a clone scoped to one already-started public operation.
    pub(crate) fn with_operation_deadline(&self, deadline: OperationDeadline) -> Self {
        let mut client = self.clone();
        client.operation_deadline = Some(deadline);
        client
    }

    /// Starts a fresh operation from this reusable client's configured default.
    pub(crate) fn begin_operation(&self) -> Self {
        self.operation_deadline.map_or_else(
            || self.with_operation_deadline(OperationDeadline::from_timeout(self.default_timeout)),
            |_| self.clone(),
        )
    }

    pub(crate) const fn operation_deadline(&self) -> Option<OperationDeadline> {
        self.operation_deadline
    }

    #[cfg(test)]
    pub(crate) fn into_fixture_transport(mut self) -> Self {
        self.fixture_transport = true;
        self
    }

    #[cfg(test)]
    pub(crate) const fn is_fixture_transport(&self) -> bool {
        self.fixture_transport
    }

    pub(crate) async fn request(&self, request: Request) -> Result<Response> {
        let operation = request_operation(&request);
        if let Some(failure) = self.state.terminal_failure() {
            return Err(failure.to_error(&operation));
        }

        let (reply, response) = oneshot::channel();
        let mut cancellation = OrderedResponseGuard::new(self);
        let result = self
            .run_with_deadline(&operation, async {
                self.commands
                    .send(ActorMessage::Request {
                        request,
                        operation: operation.clone(),
                        reply,
                    })
                    .await
                    .map_err(|_| self.closed_error(&operation))?;
                cancellation.arm();

                response.await.map_err(|_| self.closed_error(&operation))?
            })
            .await;
        cancellation.disarm();
        result
    }

    pub(crate) async fn armed_request(&self, request: Request) -> Result<PendingResponse> {
        let operation = request_operation(&request);
        if let Some(failure) = self.state.terminal_failure() {
            return Err(failure.to_error(&operation));
        }

        let (reply, response) = oneshot::channel();
        let (armed, armed_response) = oneshot::channel();
        let wait_id = sdk_wait_id_for_request(&request).ok_or_else(|| {
            TransportFailure::invalid_data("armed transport requests must be SDK wait requests")
                .to_error(&operation)
        })?;
        // The operation deadline covers dispatch through the daemon's armed
        // acknowledgement. The returned wait owns its separate match timeout.
        let mut cancellation = OrderedResponseGuard::new(self);
        let armed_result = self
            .run_with_deadline(&operation, async {
                self.commands
                    .send(ActorMessage::ArmedRequest {
                        request,
                        operation: operation.clone(),
                        reply,
                        armed,
                        wait_id,
                    })
                    .await
                    .map_err(|_| self.closed_error(&operation))?;
                cancellation.arm();

                armed_response
                    .await
                    .map_err(|_| self.closed_error(&operation))?
                    .map_err(|failure| failure.to_error(&operation))
            })
            .await;

        match armed_result {
            Ok(()) => Ok(PendingResponse::new(operation, response, cancellation)),
            Err(error) => {
                cancellation.disarm();
                Err(error)
            }
        }
    }

    pub(crate) async fn shutdown(&self) -> Result<()> {
        if let Some(failure) = self.state.terminal_failure() {
            if failure.is_eof() {
                return Ok(());
            }
            return Err(failure.to_error(TRANSPORT_SHUTDOWN_OPERATION));
        }

        let (reply, response) = oneshot::channel();
        let mut cancellation = OrderedResponseGuard::new(self);
        let result = self
            .run_with_deadline(TRANSPORT_SHUTDOWN_OPERATION, async {
                self.commands
                    .send(ActorMessage::Shutdown { reply })
                    .await
                    .map_err(|_| self.closed_error(TRANSPORT_SHUTDOWN_OPERATION))?;
                cancellation.arm();

                response
                    .await
                    .map_err(|_| self.closed_error(TRANSPORT_SHUTDOWN_OPERATION))?
            })
            .await;
        cancellation.disarm();
        result
    }

    /// Immediately tears down a dedicated transport without waiting for any
    /// outstanding long-poll response.
    pub(crate) fn abort(&self) {
        if self.state.terminal_failure().is_some() {
            return;
        }
        self.abort_with(TransportFailure::actor_closed());
    }

    fn try_send_best_effort(&self, request: Request) {
        if self.state.terminal_failure().is_some() {
            return;
        }

        let _ = self.commands.try_send(ActorMessage::BestEffort { request });
    }

    pub(crate) fn sdk_wait_owner_id(&self) -> SdkWaitOwnerId {
        self.state.sdk_wait_owner_id()
    }

    pub(crate) async fn cached_capabilities(&self) -> Option<Arc<[String]>> {
        self.state.cached_capabilities().await
    }

    pub(crate) async fn cache_capabilities(&self, capabilities: Vec<String>) -> Arc<[String]> {
        self.state.cache_capabilities(capabilities).await
    }

    pub(crate) fn allocate_sdk_wait_id(&self) -> SdkWaitId {
        self.state.allocate_sdk_wait_id()
    }

    fn closed_error(&self, operation: &str) -> RmuxError {
        self.state
            .terminal_failure()
            .unwrap_or_else(TransportFailure::actor_closed)
            .to_error(operation)
    }

    async fn run_with_deadline<F, T>(&self, operation: &str, future: F) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        let deadline = self
            .operation_deadline
            .unwrap_or_else(|| OperationDeadline::from_timeout(self.default_timeout));
        let Some(remaining) = deadline.remaining_timeout() else {
            return future.await;
        };

        match tokio::time::timeout(remaining, future).await {
            Ok(result) => result,
            Err(_) => {
                let requested = deadline.requested_timeout().unwrap_or(remaining);
                let failure = TransportFailure::timed_out(requested);
                self.abort_with(failure.clone());
                Err(failure.to_error(operation))
            }
        }
    }

    fn abort_with(&self, failure: TransportFailure) {
        self.state.set_terminal_failure(failure.clone());
        self.actor.abort();
    }
}

impl fmt::Debug for TransportClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportClient")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
pub(crate) struct DropGuard {
    action: DropAction,
}

impl DropGuard {
    pub(crate) fn noop() -> Self {
        Self {
            action: DropAction::None,
        }
    }

    pub(crate) fn best_effort(client: TransportClient, request: Request) -> Self {
        Self {
            action: DropAction::BestEffort {
                client,
                request: Some(Box::new(request)),
            },
        }
    }

    pub(crate) fn disarm(&mut self) {
        self.action = DropAction::None;
    }

    pub(crate) fn trigger(&mut self) {
        if let DropAction::BestEffort { client, request } = &mut self.action {
            if let Some(request) = request.take() {
                client.try_send_best_effort(*request);
            }
        }
        self.action = DropAction::None;
    }
}

impl Drop for DropGuard {
    fn drop(&mut self) {
        self.trigger();
    }
}

#[derive(Debug, Default)]
enum DropAction {
    #[default]
    None,
    BestEffort {
        client: TransportClient,
        request: Option<Box<Request>>,
    },
}

#[cfg(test)]
mod tests;
