use std::collections::VecDeque;

use crate::handles::session::unexpected_response;
use crate::transport::{DropGuard, TransportClient};
use crate::{Pane, PaneId, Result};
use rmux_proto::{
    PaneOptionEntry as ProtoPaneOptionEntry, PaneStateClosedReason, PaneStateCursorRequest,
    PaneStateEventDto, PaneStateSnapshot, PaneStateSubscriptionId, Request, Response,
    SubscribePaneStateRequest, UnsubscribePaneStateRequest, CAPABILITY_SDK_PANE_FOREGROUND,
    CAPABILITY_SDK_PANE_STATE_EVENTS,
};

use super::foreground::ForegroundState;

const PANE_STATE_BATCH_SIZE: u16 = 256;

/// Options used when opening a pane-state event stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneStateEventsOptions {
    /// Include title snapshots and title-change events.
    pub include_title: bool,
    /// Include pane-local option snapshots and option mutation events.
    pub include_options: bool,
    /// Include best-effort foreground snapshots and foreground-change events.
    pub include_foreground: bool,
}

impl Default for PaneStateEventsOptions {
    fn default() -> Self {
        Self {
            include_title: true,
            include_options: true,
            include_foreground: false,
        }
    }
}

/// One explicit pane option in a pane-state snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneStateOption {
    /// Canonical option name.
    pub name: String,
    /// Exact explicit option value.
    pub value: String,
}

/// One item from a pane-state event stream.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PaneStateEvent {
    /// Initial or rebased state.
    Snapshot {
        /// Global pane-state journal revision.
        revision: u64,
        /// Stable pane id.
        pane_id: PaneId,
        /// Current title when requested.
        title: Option<String>,
        /// Current explicit pane options when requested.
        options: Vec<PaneStateOption>,
        /// Best-effort foreground state when requested.
        foreground: Option<ForegroundState>,
    },
    /// The pane title changed.
    TitleChanged {
        /// Global pane-state journal revision.
        revision: u64,
        /// Stable pane id.
        pane_id: PaneId,
        /// Previous title.
        old_title: String,
        /// New title.
        new_title: String,
    },
    /// A pane-local option was set or replaced.
    OptionSet {
        /// Global pane-state journal revision.
        revision: u64,
        /// Stable pane id.
        pane_id: PaneId,
        /// Canonical option name.
        name: String,
        /// Previous explicit value.
        old_value: Option<String>,
        /// New explicit value.
        new_value: String,
    },
    /// A pane-local option was unset.
    OptionUnset {
        /// Global pane-state journal revision.
        revision: u64,
        /// Stable pane id.
        pane_id: PaneId,
        /// Canonical option name.
        name: String,
        /// Previous explicit value.
        old_value: Option<String>,
    },
    /// Best-effort foreground process state changed.
    ForegroundChanged {
        /// Global pane-state journal revision.
        revision: u64,
        /// Stable pane id.
        pane_id: PaneId,
        /// Previous foreground state.
        old_state: ForegroundState,
        /// New foreground state.
        new_state: ForegroundState,
    },
    /// The local cursor fell behind the daemon's bounded journal.
    Lagged {
        /// Cursor revision that was too old.
        missed_from_revision: u64,
        /// Oldest retained revision after the gap.
        resume_revision: u64,
    },
    /// The pane reached a terminal state for this stream.
    Closed {
        /// Global pane-state journal revision.
        revision: u64,
        /// Stable pane id.
        pane_id: PaneId,
        /// Terminal close reason.
        reason: PaneStateClosedReason,
    },
}

/// Opaque long-poll stream for pane title/option/foreground/close events.
pub struct PaneStateEventStream {
    transport: TransportClient,
    subscription_id: PaneStateSubscriptionId,
    pane_id: PaneId,
    next_revision: u64,
    pending: VecDeque<PaneStateEvent>,
    closed: bool,
    _drop_guard: DropGuard,
}

impl PaneStateEventStream {
    pub(super) async fn open(pane: &Pane, options: PaneStateEventsOptions) -> Result<Self> {
        let mut capabilities = vec![CAPABILITY_SDK_PANE_STATE_EVENTS];
        if options.include_foreground {
            capabilities.push(CAPABILITY_SDK_PANE_FOREGROUND);
        }
        crate::capabilities::require(pane.transport(), &capabilities).await?;

        let response = pane
            .transport()
            .request(Request::SubscribePaneState(SubscribePaneStateRequest {
                target: pane.proto_target_ref(),
                include_title: options.include_title,
                include_options: options.include_options,
                include_foreground: options.include_foreground,
            }))
            .await?;

        let response = match response {
            Response::SubscribePaneState(response) => *response,
            response => return Err(unexpected_response("subscribe-pane-state", response)),
        };
        let unsubscribe = Request::UnsubscribePaneState(UnsubscribePaneStateRequest {
            subscription_id: response.subscription_id,
        });
        let drop_guard = DropGuard::best_effort(pane.transport().clone(), unsubscribe);
        let mut pending = VecDeque::new();
        pending.push_back(snapshot_event(response.pane_id, response.snapshot));

        Ok(Self {
            transport: pane.transport().clone(),
            subscription_id: response.subscription_id,
            pane_id: response.pane_id,
            next_revision: 0,
            pending,
            closed: false,
            _drop_guard: drop_guard,
        })
    }

    /// Returns the next pane-state event, blocking on the daemon when needed.
    pub async fn next(&mut self) -> Result<Option<PaneStateEvent>> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                self.observe_event_cursor(&event);
                return Ok(Some(event));
            }
            if self.closed {
                return Ok(None);
            }

            let response = self
                .transport
                .request(Request::PaneStateCursor(PaneStateCursorRequest {
                    subscription_id: self.subscription_id,
                    after_revision: self.next_revision,
                    wait: true,
                    max_events: Some(PANE_STATE_BATCH_SIZE),
                }))
                .await?;

            match response {
                Response::PaneStateCursor(response) => {
                    self.next_revision = response.next_revision;
                    self.pending
                        .extend(response.events.into_iter().map(PaneStateEvent::from));
                }
                Response::PaneStateLag(response) => {
                    self.pending.push_back(PaneStateEvent::Lagged {
                        missed_from_revision: response.missed_from_revision,
                        resume_revision: response.resume_revision,
                    });
                    self.next_revision = response.snapshot.revision;
                    self.pending
                        .push_back(snapshot_event(self.pane_id, response.snapshot));
                }
                response => return Err(unexpected_response("pane-state-cursor", response)),
            }
        }
    }

    fn observe_event_cursor(&mut self, event: &PaneStateEvent) {
        match event {
            PaneStateEvent::Snapshot { revision, .. } => {
                self.next_revision = self.next_revision.max(*revision);
            }
            PaneStateEvent::TitleChanged { revision, .. }
            | PaneStateEvent::OptionSet { revision, .. }
            | PaneStateEvent::OptionUnset { revision, .. }
            | PaneStateEvent::ForegroundChanged { revision, .. }
            | PaneStateEvent::Closed { revision, .. } => {
                self.next_revision = self.next_revision.max(*revision);
            }
            PaneStateEvent::Lagged { .. } => {}
        }
        if matches!(event, PaneStateEvent::Closed { .. }) {
            self.closed = true;
        }
    }
}

impl From<PaneStateEventDto> for PaneStateEvent {
    fn from(value: PaneStateEventDto) -> Self {
        match value {
            PaneStateEventDto::TitleChanged {
                revision,
                pane_id,
                old_title,
                new_title,
            } => Self::TitleChanged {
                revision,
                pane_id,
                old_title,
                new_title,
            },
            PaneStateEventDto::OptionSet {
                revision,
                pane_id,
                name,
                old_value,
                new_value,
            } => Self::OptionSet {
                revision,
                pane_id,
                name,
                old_value,
                new_value,
            },
            PaneStateEventDto::OptionUnset {
                revision,
                pane_id,
                name,
                old_value,
            } => Self::OptionUnset {
                revision,
                pane_id,
                name,
                old_value,
            },
            PaneStateEventDto::ForegroundChanged {
                revision,
                pane_id,
                old_state,
                new_state,
            } => Self::ForegroundChanged {
                revision,
                pane_id,
                old_state: ForegroundState::from(old_state),
                new_state: ForegroundState::from(new_state),
            },
            PaneStateEventDto::Closed {
                revision,
                pane_id,
                reason,
            } => Self::Closed {
                revision,
                pane_id,
                reason,
            },
        }
    }
}

fn snapshot_event(pane_id: PaneId, snapshot: PaneStateSnapshot) -> PaneStateEvent {
    PaneStateEvent::Snapshot {
        revision: snapshot.revision,
        pane_id,
        title: snapshot.title,
        options: snapshot
            .options
            .into_iter()
            .map(PaneStateOption::from)
            .collect(),
        foreground: snapshot.foreground.map(ForegroundState::from),
    }
}

impl From<ProtoPaneOptionEntry> for PaneStateOption {
    fn from(value: ProtoPaneOptionEntry) -> Self {
        Self {
            name: value.name,
            value: value.value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PaneRef, RmuxEndpoint, SessionName};
    use rmux_proto::{
        encode_frame, FrameDecoder, HandshakeResponse, PaneId, PaneStateCursorResponse,
        PaneStateEventDto, PaneStateSnapshot, PaneStateSubscriptionId, SubscribePaneStateResponse,
        CAPABILITY_HANDSHAKE, RMUX_WIRE_VERSION,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::time::{timeout, Duration};

    fn alpha() -> SessionName {
        SessionName::new("alpha").expect("valid session name")
    }

    async fn read_request(stream: &mut tokio::io::DuplexStream) -> Request {
        let mut decoder = FrameDecoder::new();
        let mut buffer = [0; 512];
        loop {
            if let Some(request) = decoder
                .next_frame::<Request>()
                .expect("request frame decodes")
            {
                return request;
            }
            let read = stream.read(&mut buffer).await.expect("read request bytes");
            assert_ne!(read, 0, "client closed before request arrived");
            decoder.push_bytes(&buffer[..read]);
        }
    }

    async fn write_response(stream: &mut tokio::io::DuplexStream, response: Response) {
        let frame = encode_frame(&response).expect("response encodes");
        stream.write_all(&frame).await.expect("write response");
        stream.flush().await.expect("flush response");
    }

    #[tokio::test]
    async fn next_keeps_waiting_after_empty_long_poll_response() {
        let (client_stream, mut server_stream) = tokio::io::duplex(4096);
        let transport = TransportClient::spawn(client_stream);
        let pane = Pane::new(
            PaneRef::in_first_window(alpha(), 0),
            RmuxEndpoint::Default,
            None,
            transport,
        );
        let pane_id = PaneId::new(42);
        let subscription_id = PaneStateSubscriptionId::new(7);

        let server = tokio::spawn(async move {
            assert!(matches!(
                read_request(&mut server_stream).await,
                Request::Handshake(_)
            ));
            write_response(
                &mut server_stream,
                Response::Handshake(HandshakeResponse {
                    wire_version: RMUX_WIRE_VERSION,
                    capabilities: vec![
                        CAPABILITY_HANDSHAKE.to_owned(),
                        CAPABILITY_SDK_PANE_STATE_EVENTS.to_owned(),
                    ],
                }),
            )
            .await;

            assert!(matches!(
                read_request(&mut server_stream).await,
                Request::SubscribePaneState(_)
            ));
            write_response(
                &mut server_stream,
                Response::SubscribePaneState(Box::new(SubscribePaneStateResponse {
                    subscription_id,
                    pane_id,
                    snapshot: PaneStateSnapshot {
                        revision: 0,
                        title: Some("initial".to_owned()),
                        options: Vec::new(),
                        foreground: None,
                    },
                })),
            )
            .await;

            match read_request(&mut server_stream).await {
                Request::PaneStateCursor(request) => {
                    assert_eq!(request.subscription_id, subscription_id);
                    assert_eq!(request.after_revision, 0);
                    assert!(request.wait);
                }
                request => panic!("expected pane-state-cursor, got {request:?}"),
            }
            write_response(
                &mut server_stream,
                Response::PaneStateCursor(PaneStateCursorResponse {
                    subscription_id,
                    events: Vec::new(),
                    next_revision: 0,
                }),
            )
            .await;

            match read_request(&mut server_stream).await {
                Request::PaneStateCursor(request) => {
                    assert_eq!(request.subscription_id, subscription_id);
                    assert_eq!(request.after_revision, 0);
                    assert!(request.wait);
                }
                request => panic!("expected second pane-state-cursor, got {request:?}"),
            }
            write_response(
                &mut server_stream,
                Response::PaneStateCursor(PaneStateCursorResponse {
                    subscription_id,
                    events: vec![PaneStateEventDto::TitleChanged {
                        revision: 1,
                        pane_id,
                        old_title: "initial".to_owned(),
                        new_title: "next".to_owned(),
                    }],
                    next_revision: 1,
                }),
            )
            .await;
        });

        let mut stream = PaneStateEventStream::open(&pane, PaneStateEventsOptions::default())
            .await
            .expect("stream opens");
        assert!(matches!(
            stream.next().await.expect("snapshot succeeds"),
            Some(PaneStateEvent::Snapshot { revision: 0, .. })
        ));

        let event = timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("empty cursor response must not end the stream")
            .expect("next succeeds");
        assert!(matches!(
            event,
            Some(PaneStateEvent::TitleChanged {
                revision: 1,
                old_title,
                new_title,
                ..
            }) if old_title == "initial" && new_title == "next"
        ));
        server.await.expect("server task succeeds");
    }
}
