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
    SdkWaitForOutputRefRequest, SdkWaitId, SdkWaitOutcome, CAPABILITY_SDK_PANE_BY_ID,
    CAPABILITY_SDK_WAITS_ARMED,
};
use tokio::time::Instant;

use crate::handles::{connect_transport_to_endpoint, Pane};
use crate::transport::{DropGuard, PendingResponse, TransportClient};
use crate::{PaneSnapshot, Result, RmuxError};

pub use visible::{VisibleTextExpectation, VisibleTextWait, WaitTimeoutError};

const WAIT_FOR_BYTES_OPERATION: &str = "wait for pane output bytes";
pub(crate) const WAIT_FOR_TEXT_OPERATION: &str = "wait for pane snapshot text";
const WAIT_FOR_NEXT_BYTES_OPERATION: &str = "wait for next pane output bytes";
const WAIT_FOR_TEXT_NEXT_OPERATION: &str = "wait for next pane output text";
const WAIT_FOR_EXIT_OPERATION: &str = "wait for pane process exit";
pub(crate) const TEXT_POLL_INTERVAL: Duration = Duration::from_millis(25);
#[cfg(windows)]
const SDK_WAIT_ARM_DISPATCH_SETTLE: Duration = Duration::from_millis(250);

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
    _wait_client: TransportClient,
    wait_id: SdkWaitId,
    cancel_guard: DropGuard,
    timeout: Option<Pin<Box<tokio::time::Sleep>>>,
    timeout_duration: Option<Duration>,
    operation: &'static str,
}

impl ArmedWait {
    fn new(
        response: PendingResponse,
        wait_client: TransportClient,
        wait_id: SdkWaitId,
        cancel_guard: DropGuard,
        operation: &'static str,
        timeout: Option<Duration>,
    ) -> Self {
        Self {
            response,
            _wait_client: wait_client,
            wait_id,
            cancel_guard,
            // Arm lazily on the first poll so platform dispatch-settle work
            // performed before this value is returned cannot consume the
            // caller's wait budget.
            timeout: None,
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

        if let Some(duration) = self.timeout_duration {
            if self.timeout.is_none() {
                self.timeout = Some(Box::pin(tokio::time::sleep(duration)));
            }
            if let Some(timeout) = self.timeout.as_mut() {
                if timeout.as_mut().poll(cx).is_ready() {
                    self.cancel_guard.trigger();
                    return Poll::Ready(Err(wait_timeout_error(self.operation, duration)));
                }
            }
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
    let armed_wait = arm_sdk_wait(pane, bytes, WAIT_FOR_BYTES_OPERATION, timeout).await?;
    armed_wait.await
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

async fn arm_sdk_wait(
    pane: &Pane,
    bytes: Vec<u8>,
    operation: &'static str,
    timeout: Option<Duration>,
) -> Result<ArmedWait> {
    let armed_wait = with_wait_timeout(
        operation,
        timeout,
        arm_sdk_wait_inner(pane, bytes, operation, timeout),
    )
    .await?;

    #[cfg(windows)]
    tokio::time::sleep(SDK_WAIT_ARM_DISPATCH_SETTLE).await;

    Ok(armed_wait)
}

async fn arm_sdk_wait_inner(
    pane: &Pane,
    bytes: Vec<u8>,
    operation: &'static str,
    timeout: Option<Duration>,
) -> Result<ArmedWait> {
    let wait_client = connect_transport_to_endpoint(pane.endpoint(), timeout).await?;
    let cancel_client = connect_transport_to_endpoint(pane.endpoint(), timeout).await?;
    let owner_id = wait_client.sdk_wait_owner_id();
    crate::capabilities::require_with_handshake(
        &wait_client,
        &[CAPABILITY_SDK_WAITS_ARMED],
        &[CAPABILITY_SDK_WAITS_ARMED],
    )
    .await?;
    let wait_id = wait_client.allocate_sdk_wait_id();
    let cancel_request = Request::CancelSdkWait(CancelSdkWaitRequest { owner_id, wait_id });
    let cancel_guard = DropGuard::best_effort(cancel_client, cancel_request);
    let request = sdk_wait_request_for_pane(pane, owner_id, wait_id, bytes).await?;

    let response = wait_client.armed_request(request).await?;

    Ok(ArmedWait::new(
        response,
        wait_client,
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
    crate::capabilities::require(pane.transport(), &[CAPABILITY_SDK_PANE_BY_ID]).await?;
    Ok(Request::SdkWaitForOutputRef(SdkWaitForOutputRefRequest {
        owner_id,
        wait_id,
        target: pane.required_resolved_proto_target_ref().await?,
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

/// Applies an SDK wait timeout to one protocol phase.
///
/// Platform settle delays that run after the daemon has acknowledged success
/// should happen outside this wrapper so short user timeouts do not fail after
/// the requested phase already completed.
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

pub(crate) async fn with_wait_deadline<F, T>(
    operation: &'static str,
    timeout: Option<Duration>,
    deadline: Option<Instant>,
    future: F,
) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    let Some(deadline) = deadline else {
        return future.await;
    };
    let timeout = timeout.expect("deadline implies timeout");
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(wait_timeout_error(operation, timeout));
    }
    match tokio::time::timeout(remaining, future).await {
        Ok(Err(RmuxError::Transport { source, .. }))
            if source.kind() == io::ErrorKind::TimedOut =>
        {
            // The transport shares this wait's absolute deadline, so its
            // request timer may win the race against the outer wait timer.
            // Keep the original typed I/O cause while exposing the stable
            // public wait operation instead of an internal RPC label.
            Err(RmuxError::transport(operation, source))
        }
        Ok(result) => result,
        Err(_) => Err(wait_timeout_error(operation, timeout)),
    }
}

pub(crate) async fn snapshot_with_wait_deadline(
    pane: &Pane,
    operation: &'static str,
    timeout: Option<Duration>,
    deadline: Option<Instant>,
    last_snapshot: Option<&PaneSnapshot>,
    description: impl FnOnce() -> String,
) -> Result<PaneSnapshot> {
    match with_wait_deadline(operation, timeout, deadline, pane.snapshot()).await {
        Ok(snapshot) => Ok(snapshot),
        Err(error) if is_wait_deadline_error(&error) && last_snapshot.is_some() => {
            Err(RmuxError::wait_timeout(WaitTimeoutError::new(
                description(),
                timeout.expect("deadline implies timeout"),
                last_snapshot.expect("checked snapshot presence").clone(),
            )))
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn is_wait_deadline_error(error: &RmuxError) -> bool {
    matches!(
        error,
        RmuxError::Transport { source, .. } if source.kind() == io::ErrorKind::TimedOut
    )
}

pub(crate) fn resolved_wait_timeout(default_timeout: Option<Duration>) -> Option<Duration> {
    crate::bootstrap::discovery::resolve_timeout(None, default_timeout)
}

pub(crate) fn resolved_wait_timeout_override(
    timeout: Option<Duration>,
    default_timeout: Option<Duration>,
) -> Option<Duration> {
    crate::bootstrap::discovery::resolve_timeout(timeout, default_timeout)
}

pub(crate) fn wait_deadline(timeout: Option<Duration>) -> Option<Instant> {
    timeout.and_then(|timeout| Instant::now().checked_add(timeout))
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
mod tests;
