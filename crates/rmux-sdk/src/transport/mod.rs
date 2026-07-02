//! Crate-private Tokio transport actor for detached SDK RPC.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;

use rmux_proto::{encode_frame, FrameDecoder, Request, Response, SdkWaitId, SdkWaitOwnerId};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use crate::{Result, RmuxError};

mod failure;
mod pending;
mod state;

use failure::TransportFailure;
pub(crate) use pending::PendingResponse;
use pending::{PendingCall, PendingResponseAction};
use state::TransportState;
#[cfg(test)]
use state::{allocate_bounded_atomic_id, mix_sdk_wait_owner_id};

const ACTOR_QUEUE_CAPACITY: usize = 64;
const READ_BUFFER_SIZE: usize = 8192;
const TRANSPORT_SHUTDOWN_OPERATION: &str = "shut down rmux SDK transport";

#[derive(Clone)]
pub(crate) struct TransportClient {
    commands: mpsc::Sender<ActorMessage>,
    state: Arc<TransportState>,
}

impl TransportClient {
    pub(crate) fn spawn<S>(stream: S) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (commands, receiver) = mpsc::channel(ACTOR_QUEUE_CAPACITY);
        let state = Arc::new(TransportState::default());
        tokio::spawn(run_actor(stream, receiver, state.clone()));
        Self { commands, state }
    }

    pub(crate) async fn request(&self, request: Request) -> Result<Response> {
        let operation = request_operation(&request);
        if let Some(failure) = self.state.terminal_failure() {
            return Err(failure.to_error(&operation));
        }

        let (reply, response) = oneshot::channel();
        self.commands
            .send(ActorMessage::Request {
                request,
                operation: operation.clone(),
                reply,
            })
            .await
            .map_err(|_| self.closed_error(&operation))?;

        response.await.map_err(|_| self.closed_error(&operation))?
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

        armed_response
            .await
            .map_err(|_| self.closed_error(&operation))?
            .map_err(|failure| failure.to_error(&operation))?;

        Ok(PendingResponse::new(operation, response))
    }

    pub(crate) async fn shutdown(&self) -> Result<()> {
        if let Some(failure) = self.state.terminal_failure() {
            if failure.is_eof() {
                return Ok(());
            }
            return Err(failure.to_error(TRANSPORT_SHUTDOWN_OPERATION));
        }

        let (reply, response) = oneshot::channel();
        self.commands
            .send(ActorMessage::Shutdown { reply })
            .await
            .map_err(|_| self.closed_error(TRANSPORT_SHUTDOWN_OPERATION))?;

        response
            .await
            .map_err(|_| self.closed_error(TRANSPORT_SHUTDOWN_OPERATION))?
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

enum ActorMessage {
    Request {
        request: Request,
        operation: String,
        reply: oneshot::Sender<Result<Response>>,
    },
    ArmedRequest {
        request: Request,
        operation: String,
        reply: oneshot::Sender<Result<Response>>,
        armed: oneshot::Sender<core::result::Result<(), TransportFailure>>,
        wait_id: SdkWaitId,
    },
    BestEffort {
        request: Request,
    },
    Shutdown {
        reply: oneshot::Sender<Result<()>>,
    },
}

enum ActorEvent {
    Command(ActorMessage),
    CommandsClosed,
    Response(core::result::Result<Response, TransportFailure>),
}

async fn run_actor<S>(stream: S, commands: mpsc::Receiver<ActorMessage>, state: Arc<TransportState>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let (events, mut event_receiver) = mpsc::channel(ACTOR_QUEUE_CAPACITY * 2);
    let command_task = tokio::spawn(forward_commands(commands, events.clone()));
    let read_task = tokio::spawn(forward_responses(reader, events));
    let mut pending = VecDeque::new();
    let mut commands_closed = false;
    let mut shutdown_reply = None;

    while let Some(event) = event_receiver.recv().await {
        match event {
            ActorEvent::Command(message) => {
                if shutdown_reply.is_some() {
                    reject_command_after_shutdown(message);
                    continue;
                }

                match message {
                    ActorMessage::Request {
                        request,
                        operation,
                        reply,
                    } => {
                        let command_name = request.command_name();
                        let frame = match encode_request(&request) {
                            Ok(frame) => frame,
                            Err(failure) => {
                                let _ = reply.send(Err(failure.to_error(&operation)));
                                continue;
                            }
                        };
                        pending.push_back(PendingCall::reply(command_name, operation, reply));
                        if let Err(failure) = write_frame(&mut writer, &frame).await {
                            fail_transport(&mut pending, &state, failure);
                            break;
                        }
                    }
                    ActorMessage::ArmedRequest {
                        request,
                        operation,
                        reply,
                        armed,
                        wait_id,
                    } => {
                        let command_name = request.command_name();
                        let frame = match encode_request(&request) {
                            Ok(frame) => frame,
                            Err(failure) => {
                                let _ = reply.send(Err(failure.to_error(&operation)));
                                let _ = armed.send(Err(failure));
                                continue;
                            }
                        };
                        pending.push_back(PendingCall::armed_reply(
                            command_name,
                            operation.clone(),
                            reply,
                            armed,
                            wait_id,
                        ));
                        if let Err(failure) = write_frame(&mut writer, &frame).await {
                            fail_transport(&mut pending, &state, failure);
                            break;
                        }
                    }
                    ActorMessage::BestEffort { request } => {
                        let command_name = request.command_name();
                        let Ok(frame) = encode_request(&request) else {
                            continue;
                        };
                        pending.push_back(PendingCall::discard(
                            command_name,
                            request_operation(&request),
                        ));
                        if let Err(failure) = write_frame(&mut writer, &frame).await {
                            fail_transport(&mut pending, &state, failure);
                            break;
                        }
                    }
                    ActorMessage::Shutdown { reply } => {
                        match writer.shutdown().await.map_err(TransportFailure::io) {
                            Ok(()) => {
                                shutdown_reply = Some(reply);
                            }
                            Err(failure) => {
                                let _ =
                                    reply.send(Err(failure.to_error(TRANSPORT_SHUTDOWN_OPERATION)));
                                fail_transport(&mut pending, &state, failure);
                                break;
                            }
                        }
                    }
                }
            }
            ActorEvent::CommandsClosed => {
                commands_closed = true;
            }
            ActorEvent::Response(result) => match result {
                Ok(response) => {
                    let Some(mut pending_call) = pending.pop_front() else {
                        let failure = TransportFailure::unsolicited_response(&response);
                        fail_shutdown(&mut shutdown_reply, &failure);
                        fail_transport(&mut pending, &state, failure);
                        break;
                    };
                    match pending_call.accept_response(&response) {
                        Ok(PendingResponseAction::Complete) => {
                            pending_call.complete(response);
                        }
                        Ok(PendingResponseAction::KeepPending) => {
                            pending.push_front(pending_call);
                        }
                        Err(failure) => {
                            pending_call.fail(&failure);
                            fail_shutdown(&mut shutdown_reply, &failure);
                            fail_transport(&mut pending, &state, failure);
                            break;
                        }
                    }
                }
                Err(failure) => {
                    if shutdown_reply.is_some() && pending.is_empty() && failure.is_eof() {
                        complete_shutdown(&mut shutdown_reply);
                        break;
                    }

                    fail_shutdown(&mut shutdown_reply, &failure);
                    fail_transport(&mut pending, &state, failure);
                    break;
                }
            },
        }

        if commands_closed && pending.is_empty() && shutdown_reply.is_none() {
            let _ = writer.shutdown().await;
            break;
        }
    }

    command_task.abort();
    read_task.abort();
}

fn reject_command_after_shutdown(message: ActorMessage) {
    match message {
        ActorMessage::Request {
            operation, reply, ..
        } => {
            let failure = TransportFailure::actor_closed();
            let _ = reply.send(Err(failure.to_error(&operation)));
        }
        ActorMessage::ArmedRequest {
            operation,
            reply,
            armed,
            ..
        } => {
            let failure = TransportFailure::actor_closed();
            let _ = reply.send(Err(failure.to_error(&operation)));
            let _ = armed.send(Err(failure));
        }
        ActorMessage::BestEffort { .. } => {}
        ActorMessage::Shutdown { reply } => {
            let failure = TransportFailure::actor_closed();
            let _ = reply.send(Err(failure.to_error(TRANSPORT_SHUTDOWN_OPERATION)));
        }
    }
}

fn complete_shutdown(reply: &mut Option<oneshot::Sender<Result<()>>>) {
    if let Some(reply) = reply.take() {
        let _ = reply.send(Ok(()));
    }
}

fn fail_shutdown(reply: &mut Option<oneshot::Sender<Result<()>>>, failure: &TransportFailure) {
    if let Some(reply) = reply.take() {
        let _ = reply.send(Err(failure.to_error(TRANSPORT_SHUTDOWN_OPERATION)));
    }
}

async fn forward_commands(
    mut commands: mpsc::Receiver<ActorMessage>,
    events: mpsc::Sender<ActorEvent>,
) {
    while let Some(message) = commands.recv().await {
        if events.send(ActorEvent::Command(message)).await.is_err() {
            return;
        }
    }

    let _ = events.send(ActorEvent::CommandsClosed).await;
}

async fn forward_responses<R>(mut reader: R, events: mpsc::Sender<ActorEvent>)
where
    R: AsyncRead + Unpin,
{
    let mut decoder = FrameDecoder::new();
    loop {
        let result = read_response(&mut reader, &mut decoder).await;
        let stop = result.is_err();
        if events.send(ActorEvent::Response(result)).await.is_err() {
            return;
        }
        if stop {
            return;
        }
    }
}

fn encode_request(request: &Request) -> core::result::Result<Vec<u8>, TransportFailure> {
    encode_frame(request).map_err(TransportFailure::frame)
}

async fn write_frame<W>(writer: &mut W, frame: &[u8]) -> core::result::Result<(), TransportFailure>
where
    W: AsyncWrite + Unpin,
{
    writer
        .write_all(frame)
        .await
        .map_err(TransportFailure::io)?;
    writer.flush().await.map_err(TransportFailure::io)
}

async fn read_response<R>(
    reader: &mut R,
    decoder: &mut FrameDecoder,
) -> core::result::Result<Response, TransportFailure>
where
    R: AsyncRead + Unpin,
{
    let mut buffer = [0; READ_BUFFER_SIZE];
    loop {
        if let Some(response) = decoder
            .next_frame::<Response>()
            .map_err(TransportFailure::frame)?
        {
            return Ok(response);
        }

        let read = reader
            .read(&mut buffer)
            .await
            .map_err(TransportFailure::io)?;
        if read == 0 {
            return Err(TransportFailure::eof());
        }
        decoder.push_bytes(&buffer[..read]);
    }
}

fn fail_all(pending: &mut VecDeque<PendingCall>, failure: &TransportFailure) {
    while let Some(call) = pending.pop_front() {
        call.fail(failure);
    }
}

fn fail_transport(
    pending: &mut VecDeque<PendingCall>,
    state: &TransportState,
    failure: TransportFailure,
) {
    state.set_terminal_failure(failure.clone());
    fail_all(pending, &failure);
}

fn request_operation(request: &Request) -> String {
    format!(
        "complete `{}` request/response exchange with rmux daemon",
        request.command_name()
    )
}

fn sdk_wait_id_for_request(request: &Request) -> Option<SdkWaitId> {
    match request {
        Request::SdkWaitForOutput(request) => Some(request.wait_id),
        Request::SdkWaitForOutputRef(request) => Some(request.wait_id),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
