//! Daemon-backed byte waits and snapshot-polled text wait helpers.

#[path = "wait/visible.rs"]
mod visible;

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use rmux_proto::{
    CancelSdkWaitRequest, PaneOutputSubscriptionStart, Request, Response, RmuxError as ProtoError,
    SdkWaitForOutputRefRequest, SdkWaitForOutputRequest, SdkWaitId, SdkWaitOutcome,
    CAPABILITY_SDK_PANE_BY_ID,
};

use crate::handles::{connect_transport_to_endpoint, Pane};
use crate::transport::{DropGuard, PendingResponse};
use crate::{Result, RmuxError};

pub use visible::{VisibleTextExpectation, VisibleTextWait, WaitTimeoutError};

const WAIT_FOR_BYTES_OPERATION: &str = "wait for pane output bytes";
const WAIT_FOR_TEXT_OPERATION: &str = "wait for pane snapshot text";
const WAIT_FOR_NEXT_BYTES_OPERATION: &str = "wait for next pane output bytes";
const WAIT_FOR_TEXT_NEXT_OPERATION: &str = "wait for next pane output text";
const WAIT_FOR_EXIT_OPERATION: &str = "wait for pane process exit";
pub(crate) const TEXT_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A daemon-armed wait for future pane output.
///
/// Values are returned by [`Pane::wait_for_next`](crate::Pane::wait_for_next)
/// and [`Pane::wait_for_text_next`](crate::Pane::wait_for_text_next) after the
/// SDK has written the daemon wait request. Awaiting the value completes when
/// that daemon wait reports a match. Dropping it before a match sends a
/// best-effort SDK wait cancellation request; cancellation never closes panes,
/// sessions, child processes, or the daemon.
#[must_use = "armed waits do nothing useful unless awaited or explicitly dropped"]
pub struct ArmedWait {
    response: PendingResponse,
    wait_id: SdkWaitId,
    cancel_guard: DropGuard,
    timeout: Option<Pin<Box<tokio::time::Sleep>>>,
    timeout_duration: Option<Duration>,
    operation: &'static str,
}

impl ArmedWait {
    fn new(
        response: PendingResponse,
        wait_id: SdkWaitId,
        cancel_guard: DropGuard,
        operation: &'static str,
        timeout: Option<Duration>,
    ) -> Self {
        Self {
            response,
            wait_id,
            cancel_guard,
            timeout: timeout.map(|duration| Box::pin(tokio::time::sleep(duration))),
            timeout_duration: timeout,
            operation,
        }
    }
}

impl Future for ArmedWait {
    type Output = Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.response).poll(cx) {
            Poll::Ready(Ok(response)) => {
                if sdk_wait_response_disarms_cancel(&response, self.wait_id) {
                    self.cancel_guard.disarm();
                }
                let result = sdk_wait_response_to_result(response, self.wait_id);
                return Poll::Ready(result);
            }
            Poll::Ready(Err(error)) => {
                if sdk_wait_error_disarms_cancel(&error) {
                    self.cancel_guard.disarm();
                }
                return Poll::Ready(Err(error));
            }
            Poll::Pending => {}
        }

        if let Some(duration) = self.timeout_duration
            && let Some(timeout) = self.timeout.as_mut()
                && timeout.as_mut().poll(cx).is_ready() {
                    self.cancel_guard.trigger();
                    return Poll::Ready(Err(wait_timeout_error(self.operation, duration)));
                }

        Poll::Pending
    }
}

impl std::fmt::Debug for ArmedWait {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ArmedWait")
            .field("wait_id", &self.wait_id)
            .field("operation", &self.operation)
            .finish_non_exhaustive()
    }
}

pub(crate) async fn wait_for_bytes(pane: &Pane, bytes: Vec<u8>) -> Result<()> {
    if bytes.is_empty() {
        return Err(RmuxError::protocol(ProtoError::Server(
            "SDK wait bytes must not be empty".to_owned(),
        )));
    }

    let timeout = resolved_wait_timeout(pane.configured_default_timeout());
    with_wait_timeout(
        WAIT_FOR_BYTES_OPERATION,
        timeout,
        wait_for_bytes_without_timeout(pane, bytes, timeout),
    )
    .await
}

pub(crate) async fn wait_for_next_bytes(pane: &Pane, bytes: Vec<u8>) -> Result<ArmedWait> {
    if bytes.is_empty() {
        return Err(RmuxError::protocol(ProtoError::Server(
            "SDK wait bytes must not be empty".to_owned(),
        )));
    }

    let timeout = resolved_wait_timeout(pane.configured_default_timeout());
    arm_sdk_wait(pane, bytes, WAIT_FOR_NEXT_BYTES_OPERATION, timeout).await
}

pub(crate) async fn wait_for_text(pane: &Pane, text: String) -> Result<()> {
    if text.is_empty() {
        return Err(RmuxError::protocol(ProtoError::Server(
            "SDK wait text must not be empty".to_owned(),
        )));
    }

    let timeout = resolved_wait_timeout(pane.configured_default_timeout());
    with_wait_timeout(
        WAIT_FOR_TEXT_OPERATION,
        timeout,
        wait_for_text_without_timeout(pane, text),
    )
    .await
}

pub(crate) async fn wait_for_text_next(pane: &Pane, text: String) -> Result<ArmedWait> {
    if text.is_empty() {
        return Err(RmuxError::protocol(ProtoError::Server(
            "SDK wait text must not be empty".to_owned(),
        )));
    }

    let timeout = resolved_wait_timeout(pane.configured_default_timeout());
    arm_sdk_wait(
        pane,
        text.into_bytes(),
        WAIT_FOR_TEXT_NEXT_OPERATION,
        timeout,
    )
    .await
}

pub(crate) async fn wait_exit(pane: &Pane) -> Result<Option<crate::PaneExitState>> {
    let timeout = resolved_wait_timeout(pane.configured_default_timeout());
    with_wait_timeout(
        WAIT_FOR_EXIT_OPERATION,
        timeout,
        wait_exit_without_timeout(pane),
    )
    .await
}

async fn wait_for_bytes_without_timeout(
    pane: &Pane,
    bytes: Vec<u8>,
    timeout: Option<Duration>,
) -> Result<()> {
    let owner_id = pane.transport().sdk_wait_owner_id();
    let wait_id = pane.transport().allocate_sdk_wait_id();
    let cancel_request = Request::CancelSdkWait(CancelSdkWaitRequest { owner_id, wait_id });
    let cancel_client = connect_transport_to_endpoint(pane.endpoint(), timeout).await?;
    let mut cancel_guard = DropGuard::best_effort(cancel_client, cancel_request);

    let response = if pane.is_stable_id() {
        crate::capabilities::require(pane.transport(), &[CAPABILITY_SDK_PANE_BY_ID]).await?;
        pane.transport()
            .request(Request::SdkWaitForOutputRef(SdkWaitForOutputRefRequest {
                owner_id,
                wait_id,
                target: pane.proto_target_ref(),
                bytes,
                start: PaneOutputSubscriptionStart::Now,
            }))
            .await
    } else {
        pane.transport()
            .request(Request::SdkWaitForOutput(SdkWaitForOutputRequest {
                owner_id,
                wait_id,
                target: pane.target().into(),
                bytes,
                start: PaneOutputSubscriptionStart::Now,
            }))
            .await
    };

    let response = match response {
        Ok(response) => response,
        Err(error) => {
            if sdk_wait_error_disarms_cancel(&error) {
                cancel_guard.disarm();
            }
            return Err(error);
        }
    };

    if sdk_wait_response_disarms_cancel(&response, wait_id) {
        cancel_guard.disarm();
    }
    sdk_wait_response_to_result(response, wait_id)
}

async fn arm_sdk_wait(
    pane: &Pane,
    bytes: Vec<u8>,
    operation: &'static str,
    timeout: Option<Duration>,
) -> Result<ArmedWait> {
    let wait_client = connect_transport_to_endpoint(pane.endpoint(), timeout).await?;
    let cancel_client = connect_transport_to_endpoint(pane.endpoint(), timeout).await?;
    let owner_id = wait_client.sdk_wait_owner_id();
    let wait_id = wait_client.allocate_sdk_wait_id();
    let cancel_request = Request::CancelSdkWait(CancelSdkWaitRequest { owner_id, wait_id });
    let cancel_guard = DropGuard::best_effort(cancel_client, cancel_request);

    let response = with_wait_timeout(
        operation,
        timeout,
        wait_client.armed_request(sdk_wait_request_for_pane(pane, owner_id, wait_id, bytes).await?),
    )
    .await?;

    Ok(ArmedWait::new(
        response,
        wait_id,
        cancel_guard,
        operation,
        timeout,
    ))
}

async fn sdk_wait_request_for_pane(
    pane: &Pane,
    owner_id: rmux_proto::SdkWaitOwnerId,
    wait_id: SdkWaitId,
    bytes: Vec<u8>,
) -> Result<Request> {
    if pane.is_stable_id() {
        crate::capabilities::require(pane.transport(), &[CAPABILITY_SDK_PANE_BY_ID]).await?;
        return Ok(Request::SdkWaitForOutputRef(SdkWaitForOutputRefRequest {
            owner_id,
            wait_id,
            target: pane.proto_target_ref(),
            bytes,
            start: PaneOutputSubscriptionStart::Now,
        }));
    }

    Ok(Request::SdkWaitForOutput(SdkWaitForOutputRequest {
        owner_id,
        wait_id,
        target: pane.target().into(),
        bytes,
        start: PaneOutputSubscriptionStart::Now,
    }))
}

async fn wait_for_text_without_timeout(pane: &Pane, text: String) -> Result<()> {
    loop {
        let snapshot = pane.snapshot().await?;
        if snapshot.visible_text().contains(&text) {
            return Ok(());
        }
        tokio::time::sleep(TEXT_POLL_INTERVAL).await;
    }
}

async fn wait_exit_without_timeout(pane: &Pane) -> Result<Option<crate::PaneExitState>> {
    loop {
        match pane_exit_observation(pane).await? {
            PaneExitObservation::Running => {}
            PaneExitObservation::Exited(exit_state) => return Ok(exit_state),
        }
        tokio::time::sleep(TEXT_POLL_INTERVAL).await;
    }
}

pub(crate) async fn pane_exit_observation(pane: &Pane) -> Result<PaneExitObservation> {
    let info = pane.info().await?;
    let Some(pane) = info.panes.first() else {
        return Ok(PaneExitObservation::Exited(None));
    };

    if matches!(pane.process, crate::PaneProcessState::Exited) || pane.exit_state.is_some() {
        return Ok(PaneExitObservation::Exited(pane.exit_state.clone()));
    }

    Ok(PaneExitObservation::Running)
}

pub(crate) enum PaneExitObservation {
    Running,
    Exited(Option<crate::PaneExitState>),
}

pub(crate) async fn with_wait_timeout<F, T>(
    operation: &'static str,
    timeout: Option<Duration>,
    future: F,
) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    match timeout {
        Some(timeout) => tokio::time::timeout(timeout, future)
            .await
            .map_err(|_| wait_timeout_error(operation, timeout))?,
        None => future.await,
    }
}

pub(crate) fn resolved_wait_timeout(default_timeout: Option<Duration>) -> Option<Duration> {
    crate::bootstrap::discovery::resolve_timeout(None, default_timeout)
}

pub(crate) fn wait_timeout_error(operation: &'static str, timeout: Duration) -> RmuxError {
    RmuxError::transport(
        operation,
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "timed out after {}s while {operation}",
                timeout.as_secs_f32()
            ),
        ),
    )
}

fn sdk_wait_response_disarms_cancel(response: &Response, expected_wait_id: SdkWaitId) -> bool {
    matches!(
        response,
        Response::SdkWaitForOutput(response) if response.wait_id == expected_wait_id
    )
}

fn sdk_wait_error_disarms_cancel(error: &RmuxError) -> bool {
    matches!(
        error,
        RmuxError::Protocol { .. } | RmuxError::Unsupported { .. }
    )
}

fn sdk_wait_response_to_result(response: Response, expected_wait_id: SdkWaitId) -> Result<()> {
    match response {
        Response::SdkWaitForOutput(response)
            if response.wait_id == expected_wait_id
                && response.outcome == SdkWaitOutcome::Matched =>
        {
            Ok(())
        }
        Response::SdkWaitForOutput(response)
            if response.wait_id == expected_wait_id
                && response.outcome == SdkWaitOutcome::Cancelled =>
        {
            Err(RmuxError::protocol(ProtoError::Server(format!(
                "SDK wait {} was cancelled",
                response.wait_id.as_u64()
            ))))
        }
        Response::SdkWaitForOutput(response) => {
            if response.wait_id != expected_wait_id {
                return Err(RmuxError::protocol(ProtoError::Server(format!(
                    "SDK wait response id {} did not match request id {}",
                    response.wait_id.as_u64(),
                    expected_wait_id.as_u64()
                ))));
            }

            Err(RmuxError::protocol(ProtoError::Server(format!(
                "SDK wait {} completed with unexpected outcome {:?}",
                response.wait_id.as_u64(),
                response.outcome
            ))))
        }
        response => Err(crate::handles::session::unexpected_response(
            "sdk-wait-output",
            response,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::TransportClient;
    use rmux_proto::{encode_frame, CancelSdkWaitResponse, FrameDecoder, SdkWaitForOutputResponse};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn read_request(stream: &mut tokio::io::DuplexStream) -> Request {
        let mut decoder = FrameDecoder::new();
        let mut buffer = [0_u8; 512];

        loop {
            if let Some(request) = decoder
                .next_frame::<Request>()
                .expect("request frame decodes")
            {
                return request;
            }

            let read = stream.read(&mut buffer).await.expect("read request");
            assert_ne!(read, 0, "stream closed before request");
            decoder.push_bytes(&buffer[..read]);
        }
    }

    async fn write_response(stream: &mut tokio::io::DuplexStream, response: Response) {
        let frame = encode_frame(&response).expect("response encodes");
        stream.write_all(&frame).await.expect("write response");
        stream.flush().await.expect("flush response");
    }

    #[tokio::test]
    async fn drop_guard_sends_cancel_request_once_when_wait_future_is_dropped() {
        let (client_stream, mut server_stream) = tokio::io::duplex(4096);
        let client = TransportClient::spawn(client_stream);
        let owner_id = client.sdk_wait_owner_id();
        let wait_id = client.allocate_sdk_wait_id();
        let guard = DropGuard::best_effort(
            client,
            Request::CancelSdkWait(CancelSdkWaitRequest { owner_id, wait_id }),
        );

        drop(guard);

        assert_eq!(
            read_request(&mut server_stream).await,
            Request::CancelSdkWait(CancelSdkWaitRequest { owner_id, wait_id })
        );
        write_response(
            &mut server_stream,
            Response::CancelSdkWait(CancelSdkWaitResponse {
                wait_id,
                removed: true,
            }),
        )
        .await;
    }

    #[tokio::test]
    async fn disarmed_drop_guard_does_not_send_stale_cancel() {
        let (client_stream, mut server_stream) = tokio::io::duplex(4096);
        let client = TransportClient::spawn(client_stream);
        let owner_id = client.sdk_wait_owner_id();
        let mut guard = DropGuard::best_effort(
            client,
            Request::CancelSdkWait(CancelSdkWaitRequest {
                owner_id,
                wait_id: SdkWaitId::new(9),
            }),
        );
        guard.disarm();
        drop(guard);

        let mut buffer = [0_u8; 1];
        let read = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            server_stream.read(&mut buffer),
        )
        .await;
        match read {
            Err(_) => {}
            Ok(Ok(0)) => {}
            Ok(other) => panic!("disarmed guard must not write cancel, got {other:?}"),
        }
    }

    #[test]
    fn sdk_wait_response_rejects_mismatched_wait_id() {
        let result = sdk_wait_response_to_result(
            Response::SdkWaitForOutput(SdkWaitForOutputResponse {
                wait_id: SdkWaitId::new(10),
                outcome: SdkWaitOutcome::Matched,
            }),
            SdkWaitId::new(9),
        );

        match result.expect_err("mismatched wait id must fail") {
            RmuxError::Protocol {
                source: ProtoError::Server(message),
                ..
            } => assert!(message.contains("did not match request id 9")),
            error => panic!("expected protocol mismatch, got {error:?}"),
        }
    }

    #[test]
    fn duration_max_resolves_to_no_timeout_for_wait_operations() {
        assert_eq!(resolved_wait_timeout(Some(Duration::MAX)), None);
    }

    #[tokio::test]
    async fn finite_wait_timeout_surfaces_typed_timeout_error() {
        let error = with_wait_timeout(
            "test wait operation",
            Some(Duration::from_millis(1)),
            std::future::pending::<Result<()>>(),
        )
        .await
        .expect_err("pending wait must time out");

        match error {
            RmuxError::Transport { operation, source } => {
                assert_eq!(operation, "test wait operation");
                assert_eq!(source.kind(), io::ErrorKind::TimedOut);
            }
            other => panic!("expected typed transport timeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn no_timeout_branch_awaits_future_directly() {
        let value = with_wait_timeout("test no timeout", None, async { Ok(7_u8) })
            .await
            .expect("untimed ready future completes");

        assert_eq!(value, 7);
    }
}
