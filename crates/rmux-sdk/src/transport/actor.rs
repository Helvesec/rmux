use std::collections::VecDeque;
use std::io;
use std::ops::ControlFlow;
use std::sync::Arc;

use rmux_proto::{encode_frame, FrameDecoder, Request, Response, SdkWaitId};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use crate::Result;

use super::pending::{PendingCall, PendingResponseAction};
use super::{TransportFailure, TransportState, ACTOR_QUEUE_CAPACITY, TRANSPORT_SHUTDOWN_OPERATION};

const READ_BUFFER_SIZE: usize = 8192;

pub(super) enum ActorMessage {
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

/// Request frames the actor buffers before it stops taking commands. Further
/// callers wait in the bounded command queue, so a burst of concurrent callers
/// cannot pin unbounded memory, and response reads never wait for that queue.
const OUTGOING_FRAME_LIMIT: usize = ACTOR_QUEUE_CAPACITY;

/// Runs one ordered connection as a full-duplex loop.
///
/// The daemon serves a detached connection serially: it stops reading
/// requests while its response write is blocked. The actor must therefore
/// keep draining responses while a request write is in progress. Every await
/// in the loop is a cancel-safe `select!` branch; no handler awaits I/O, except
/// the final write-side shutdown once every buffered frame is written.
pub(super) async fn run_actor<S>(
    stream: S,
    mut commands: mpsc::Receiver<ActorMessage>,
    state: Arc<TransportState>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let (responses, mut response_receiver) = mpsc::channel(ACTOR_QUEUE_CAPACITY);
    let _reader = AbortOnDrop(tokio::spawn(forward_responses(reader, responses)));
    let mut actor = ActorLoop::new(state);
    let mut commands_open = true;

    loop {
        let step = tokio::select! {
            biased;
            response = response_receiver.recv() => {
                actor.handle_response(response.unwrap_or_else(|| Err(TransportFailure::eof())))
            }
            progress = actor.outgoing.advance(&mut writer), if actor.outgoing.has_work() => {
                actor.handle_write_progress(progress)
            }
            message = commands.recv(), if commands_open && actor.outgoing.accepts_frames() => {
                match message {
                    Some(message) => actor.handle_command(message),
                    None => {
                        commands_open = false;
                        ControlFlow::Continue(())
                    }
                }
            }
        };
        if step.is_break() {
            break;
        }

        if actor.shutdown_write_requested && actor.outgoing.is_drained() {
            actor.shutdown_write_requested = false;
            if let Err(failure) = writer.shutdown().await.map_err(TransportFailure::io) {
                actor.fail(failure);
                break;
            }
        }

        if !commands_open && actor.pending.is_empty() && actor.shutdown_reply.is_none() {
            let _ = writer.shutdown().await;
            break;
        }
    }
}

struct ActorLoop {
    state: Arc<TransportState>,
    pending: VecDeque<PendingCall>,
    outgoing: OutgoingFrames,
    shutdown_reply: Option<oneshot::Sender<Result<()>>>,
    shutdown_write_requested: bool,
}

impl ActorLoop {
    fn new(state: Arc<TransportState>) -> Self {
        Self {
            state,
            pending: VecDeque::new(),
            outgoing: OutgoingFrames::default(),
            shutdown_reply: None,
            shutdown_write_requested: false,
        }
    }

    /// Queues a request frame with its FIFO response slot. Frames are written
    /// in queue order, so the slot order matches the wire order.
    fn queue(&mut self, call: PendingCall, frame: Vec<u8>) {
        self.pending.push_back(call);
        self.outgoing.push(frame);
    }

    fn handle_command(&mut self, message: ActorMessage) -> ControlFlow<()> {
        if self.shutdown_reply.is_some() {
            reject_command_after_shutdown(message);
            return ControlFlow::Continue(());
        }

        match message {
            ActorMessage::Request {
                request,
                operation,
                reply,
            } => {
                let command_name = request.command_name();
                match encode_request(&request) {
                    Ok(frame) => {
                        self.queue(PendingCall::reply(command_name, operation, reply), frame);
                    }
                    Err(failure) => {
                        let _ = reply.send(Err(failure.to_error(&operation)));
                    }
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
                match encode_request(&request) {
                    Ok(frame) => self.queue(
                        PendingCall::armed_reply(command_name, operation, reply, armed, wait_id),
                        frame,
                    ),
                    Err(failure) => {
                        let _ = reply.send(Err(failure.to_error(&operation)));
                        let _ = armed.send(Err(failure));
                    }
                }
            }
            ActorMessage::BestEffort { request } => {
                if let Ok(frame) = encode_request(&request) {
                    let call =
                        PendingCall::discard(request.command_name(), request_operation(&request));
                    self.queue(call, frame);
                }
            }
            ActorMessage::Shutdown { reply } => {
                // The write side closes only after every frame queued before
                // this shutdown has been written.
                self.shutdown_reply = Some(reply);
                self.shutdown_write_requested = true;
            }
        }
        ControlFlow::Continue(())
    }

    fn handle_response(
        &mut self,
        result: core::result::Result<Response, TransportFailure>,
    ) -> ControlFlow<()> {
        let response = match result {
            Ok(response) => response,
            Err(failure) => {
                if self.shutdown_reply.is_some() && self.pending.is_empty() && failure.is_eof() {
                    complete_shutdown(&mut self.shutdown_reply);
                } else {
                    self.fail(failure);
                }
                return ControlFlow::Break(());
            }
        };

        let Some(mut pending_call) = self.pending.pop_front() else {
            self.fail(TransportFailure::unsolicited_response(&response));
            return ControlFlow::Break(());
        };
        match pending_call.accept_response(&response) {
            Ok(PendingResponseAction::Complete) => {
                if let Some(cleanup) = pending_call.complete(response) {
                    let frame = match encode_request(&cleanup) {
                        Ok(frame) => frame,
                        Err(failure) => {
                            self.fail(failure);
                            return ControlFlow::Break(());
                        }
                    };
                    let call =
                        PendingCall::discard(cleanup.command_name(), request_operation(&cleanup));
                    self.queue(call, frame);
                }
            }
            Ok(PendingResponseAction::KeepPending) => {
                self.pending.push_front(pending_call);
            }
            Err(failure) => {
                pending_call.fail(&failure);
                self.fail(failure);
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    }

    fn handle_write_progress(
        &mut self,
        progress: core::result::Result<(), TransportFailure>,
    ) -> ControlFlow<()> {
        match progress {
            Ok(()) => ControlFlow::Continue(()),
            Err(failure) => {
                self.fail(failure);
                ControlFlow::Break(())
            }
        }
    }

    fn fail(&mut self, failure: TransportFailure) {
        fail_shutdown(&mut self.shutdown_reply, &failure);
        fail_transport(&mut self.pending, &self.state, failure);
    }
}

/// Encoded request frames waiting for the socket, written strictly in order.
#[derive(Default)]
struct OutgoingFrames {
    frames: VecDeque<Vec<u8>>,
    front_written: usize,
    needs_flush: bool,
}

impl OutgoingFrames {
    fn push(&mut self, frame: Vec<u8>) {
        self.frames.push_back(frame);
    }

    fn has_work(&self) -> bool {
        !self.frames.is_empty() || self.needs_flush
    }

    fn is_drained(&self) -> bool {
        !self.has_work()
    }

    fn accepts_frames(&self) -> bool {
        self.frames.len() < OUTGOING_FRAME_LIMIT
    }

    /// Makes one step of write progress: a partial write of the front frame,
    /// or a flush once every frame is written. State changes only after the
    /// I/O completes, so dropping this future in `select!` loses nothing.
    async fn advance<W>(&mut self, writer: &mut W) -> core::result::Result<(), TransportFailure>
    where
        W: AsyncWrite + Unpin,
    {
        let Some(frame) = self.frames.front() else {
            writer.flush().await.map_err(TransportFailure::io)?;
            self.needs_flush = false;
            return Ok(());
        };

        let written = writer
            .write(&frame[self.front_written..])
            .await
            .map_err(TransportFailure::io)?;
        if written == 0 {
            return Err(TransportFailure::io(io::ErrorKind::WriteZero.into()));
        }
        self.needs_flush = true;
        self.front_written += written;
        if self.front_written == frame.len() {
            self.frames.pop_front();
            self.front_written = 0;
        }
        Ok(())
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
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

async fn forward_responses<R>(
    mut reader: R,
    responses: mpsc::Sender<core::result::Result<Response, TransportFailure>>,
) where
    R: AsyncRead + Unpin,
{
    let mut decoder = FrameDecoder::new();
    loop {
        let result = read_response(&mut reader, &mut decoder).await;
        let stop = result.is_err();
        if responses.send(result).await.is_err() {
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

pub(super) fn request_operation(request: &Request) -> String {
    format!(
        "complete `{}` request/response exchange with rmux daemon",
        request.command_name()
    )
}

pub(super) fn sdk_wait_id_for_request(request: &Request) -> Option<SdkWaitId> {
    match request {
        Request::SdkWaitForOutput(request) => Some(request.wait_id),
        Request::SdkWaitForOutputRef(request) => Some(request.wait_id),
        _ => None,
    }
}
