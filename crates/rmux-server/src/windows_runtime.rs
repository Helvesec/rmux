//! Minimal Windows server runtime while pane/session support is being ported.

use std::io;

use rmux_ipc::{LocalListener, LocalStream};
use rmux_proto::{
    encode_frame, CommandOutput, ErrorResponse, FrameDecoder, ListClientsResponse,
    ListPanesResponse, ListSessionsResponse, ListWindowsResponse, Request, Response, RmuxError,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio::task::{JoinError, JoinSet};
use tracing::warn;

use crate::daemon::ShutdownHandle;

const WINDOWS_RUNTIME_UNSUPPORTED: &str =
    "rmux-server accepts Windows IPC, but session runtime support is not enabled yet";

pub(crate) async fn serve(
    listener: LocalListener,
    shutdown_handle: ShutdownHandle,
    mut shutdown: oneshot::Receiver<()>,
) -> io::Result<()> {
    let mut connection_tasks = JoinSet::new();

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, _peer) = result?;
                connection_tasks.spawn(serve_connection(stream, shutdown_handle.clone()));
            }
            _ = &mut shutdown => {
                break;
            }
        }
    }

    connection_tasks.abort_all();
    while let Some(result) = connection_tasks.join_next().await {
        log_connection_task_result(result);
    }

    Ok(())
}

async fn serve_connection(stream: LocalStream, shutdown_handle: ShutdownHandle) -> io::Result<()> {
    let mut conn = Connection::new(stream);

    while let Some(request) = conn.read_request().await? {
        let should_shutdown = matches!(request, Request::KillServer(_));
        let response = dispatch_minimal_windows_request(request);
        conn.write_response(&response).await?;

        if should_shutdown {
            shutdown_handle.request_shutdown();
            break;
        }
    }

    Ok(())
}

fn dispatch_minimal_windows_request(request: Request) -> Response {
    match request {
        Request::KillServer(_) => Response::KillServer(rmux_proto::KillServerResponse),
        Request::ListSessions(_) => Response::ListSessions(ListSessionsResponse {
            output: empty_output(),
        }),
        Request::ListWindows(_) => Response::ListWindows(ListWindowsResponse {
            windows: Vec::new(),
            output: empty_output(),
        }),
        Request::ListPanes(_) => Response::ListPanes(ListPanesResponse {
            output: empty_output(),
        }),
        Request::ListClients(_) => Response::ListClients(ListClientsResponse {
            output: empty_output(),
            match_count: 0,
        }),
        _ => unsupported_response(),
    }
}

fn empty_output() -> CommandOutput {
    CommandOutput::from_stdout(Vec::new())
}

fn unsupported_response() -> Response {
    Response::Error(ErrorResponse {
        error: RmuxError::Server(WINDOWS_RUNTIME_UNSUPPORTED.to_owned()),
    })
}

fn log_connection_task_result(result: Result<io::Result<()>, JoinError>) {
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => warn!("windows connection error: {error}"),
        Err(error) => warn!("windows connection task failed: {error}"),
    }
}

struct Connection {
    stream: LocalStream,
    decoder: FrameDecoder,
    read_buffer: [u8; 8192],
}

impl Connection {
    fn new(stream: LocalStream) -> Self {
        Self {
            stream,
            decoder: FrameDecoder::new(),
            read_buffer: [0; 8192],
        }
    }

    async fn read_request(&mut self) -> io::Result<Option<Request>> {
        loop {
            match self.decoder.next_frame::<Request>() {
                Ok(Some(request)) => return Ok(Some(request)),
                Ok(None) => {}
                Err(error) => {
                    let response = Response::Error(ErrorResponse { error });
                    self.write_response(&response).await?;
                    return Ok(None);
                }
            }

            let bytes_read = self.stream.read(&mut self.read_buffer).await?;
            if bytes_read == 0 {
                return Ok(None);
            }

            self.decoder.push_bytes(&self.read_buffer[..bytes_read]);
        }
    }

    async fn write_response(&mut self, response: &Response) -> io::Result<()> {
        let frame = encode_frame(response).map_err(io::Error::other)?;
        self.stream.write_all(&frame).await
    }
}
