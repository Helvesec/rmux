use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rmux_proto::TerminalSize;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::timeout;

#[cfg(not(test))]
// Leave room for a large paste on a loaded ConPTY while still bounding a
// permanently blocked synchronous WriteFile.
const POPUP_IO_RECEIPT_TIMEOUT: Duration = Duration::from_secs(8);
#[cfg(test)]
const POPUP_IO_RECEIPT_TIMEOUT: Duration = Duration::from_millis(100);

// Input and resize handlers enqueue while the active-attach mutex is held, so
// they cannot await backpressure. A small FIFO absorbs genuinely concurrent
// ingress without allowing a non-reading PTY to retain an unbounded number of
// input frames (each wire frame can itself be close to 1 MiB).
pub(super) const POPUP_IO_QUEUE_CAPACITY: usize = 8;

#[derive(Debug)]
pub(super) enum PopupIoOperation {
    Write(Vec<u8>),
    Resize(TerminalSize),
}

#[derive(Debug)]
struct PopupIoRequest {
    operation: PopupIoOperation,
    result_tx: oneshot::Sender<io::Result<()>>,
}

#[derive(Debug)]
pub(super) struct PopupIoReceipt {
    result_rx: Option<oneshot::Receiver<io::Result<()>>>,
    cancellation: Arc<PopupIoCancellation>,
    completed: bool,
}

impl PopupIoReceipt {
    pub(super) async fn wait(mut self) -> io::Result<()> {
        let result_rx = self
            .result_rx
            .take()
            .expect("popup I/O receipt can only be awaited once");
        let result = match timeout(POPUP_IO_RECEIPT_TIMEOUT, result_rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "popup I/O worker stopped before replying",
            )),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "popup I/O worker did not reply before the deadline",
            )),
        };
        if result.is_err() {
            self.cancellation.cancel();
        }
        self.completed = true;
        result
    }
}

impl Drop for PopupIoReceipt {
    fn drop(&mut self) {
        if !self.completed {
            self.cancellation.cancel();
        }
    }
}

struct PopupIoCancellation {
    worker_cancelled: AtomicBool,
    process_cancelled: AtomicBool,
    cancelled_tx: watch::Sender<bool>,
    on_cancel: Box<dyn Fn() + Send + Sync>,
}

impl std::fmt::Debug for PopupIoCancellation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PopupIoCancellation")
            .field(
                "worker_cancelled",
                &self.worker_cancelled.load(Ordering::Acquire),
            )
            .field(
                "process_cancelled",
                &self.process_cancelled.load(Ordering::Acquire),
            )
            .finish_non_exhaustive()
    }
}

impl PopupIoCancellation {
    fn stop_worker(&self) {
        if self.worker_cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        self.cancelled_tx.send_replace(true);
    }

    fn cancel(&self) {
        self.stop_worker();
        if !self.process_cancelled.swap(true, Ordering::AcqRel) {
            (self.on_cancel)();
        }
    }

    fn is_cancelled(&self) -> bool {
        self.worker_cancelled.load(Ordering::Acquire)
    }
}

#[derive(Debug, Clone)]
pub(super) struct PopupIoQueue {
    request_tx: mpsc::Sender<PopupIoRequest>,
    lifetime: Arc<PopupIoQueueLifetime>,
}

#[derive(Debug)]
struct PopupIoQueueLifetime {
    cancellation: Arc<PopupIoCancellation>,
}

impl Drop for PopupIoQueueLifetime {
    fn drop(&mut self) {
        // Queue and process ownership are separate: test adapters replace the
        // queue while retaining the process. Explicit PopupJob teardown and
        // every outstanding receipt still escalate to process cancellation.
        self.cancellation.stop_worker();
    }
}

impl PopupIoQueue {
    #[cfg(test)]
    pub(super) fn spawn<F>(execute: F) -> Self
    where
        F: Fn(PopupIoOperation) -> io::Result<()> + Send + Sync + 'static,
    {
        Self::spawn_with_cancel(execute, || {})
    }

    pub(super) fn spawn_with_cancel<F, C>(execute: F, on_cancel: C) -> Self
    where
        F: Fn(PopupIoOperation) -> io::Result<()> + Send + Sync + 'static,
        C: Fn() + Send + Sync + 'static,
    {
        // Enqueue runs while active-attach state is linearized, so it must not
        // await capacity or block that mutex. Saturation therefore fails the
        // popup closed instead of retaining input without a finite bound.
        let (request_tx, mut request_rx) = mpsc::channel::<PopupIoRequest>(POPUP_IO_QUEUE_CAPACITY);
        let (cancelled_tx, mut cancelled_rx) = watch::channel(false);
        let cancellation = Arc::new(PopupIoCancellation {
            worker_cancelled: AtomicBool::new(false),
            process_cancelled: AtomicBool::new(false),
            cancelled_tx,
            on_cancel: Box::new(on_cancel),
        });
        let worker_cancellation = Arc::clone(&cancellation);
        let lifetime = Arc::new(PopupIoQueueLifetime { cancellation });
        let execute = Arc::new(execute);
        tokio::spawn(async move {
            loop {
                let request = tokio::select! {
                    biased;
                    _ = wait_for_popup_io_cancellation(&mut cancelled_rx) => {
                        drain_popup_io_requests(&mut request_rx);
                        return;
                    }
                    request = request_rx.recv() => {
                        let Some(request) = request else {
                            return;
                        };
                        request
                    }
                };
                let execute = Arc::clone(&execute);
                let mut task = tokio::task::spawn_blocking(move || (execute)(request.operation));
                let result = tokio::select! {
                    biased;
                    _ = wait_for_popup_io_cancellation(&mut cancelled_rx) => {
                        let _ = request.result_tx.send(Err(popup_io_cancelled_error()));
                        // Abort prevents a not-yet-started blocking task from
                        // running. Once it has started, the cancellation hook
                        // closes the process/PTY; returning drops the handle so
                        // this worker can release every queued receipt now.
                        task.abort();
                        drain_popup_io_requests(&mut request_rx);
                        return;
                    }
                    result = &mut task => result.unwrap_or_else(|error| {
                        Err(io::Error::other(format!(
                            "popup I/O worker failed: {error}"
                        )))
                    }),
                };
                let failed = result.is_err();
                let _ = request.result_tx.send(result);
                if failed {
                    worker_cancellation.cancel();
                    drain_popup_io_requests(&mut request_rx);
                    return;
                }
            }
        });
        Self {
            request_tx,
            lifetime,
        }
    }

    pub(super) fn enqueue(&self, operation: PopupIoOperation) -> io::Result<PopupIoReceipt> {
        if self.lifetime.cancellation.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "popup I/O worker is shutting down",
            ));
        }
        let (result_tx, result_rx) = oneshot::channel();
        let request = PopupIoRequest {
            operation,
            result_tx,
        };
        match self.request_tx.try_send(request) {
            Ok(()) => Ok(PopupIoReceipt {
                result_rx: Some(result_rx),
                cancellation: Arc::clone(&self.lifetime.cancellation),
                completed: false,
            }),
            Err(TrySendError::Full(request)) => {
                let _ = request.result_tx.send(Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "popup I/O queue is saturated",
                )));
                // Do not run process teardown while the caller holds the
                // active-attach mutex. Waiting or dropping this ready receipt
                // escalates cancellation immediately after that lock is left.
                self.stop_worker();
                Ok(PopupIoReceipt {
                    result_rx: Some(result_rx),
                    cancellation: Arc::clone(&self.lifetime.cancellation),
                    completed: false,
                })
            }
            Err(TrySendError::Closed(request)) => {
                let _ = request.result_tx.send(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "popup I/O worker stopped",
                )));
                self.stop_worker();
                Ok(PopupIoReceipt {
                    result_rx: Some(result_rx),
                    cancellation: Arc::clone(&self.lifetime.cancellation),
                    completed: false,
                })
            }
        }
    }

    pub(super) fn cancel(&self) {
        self.lifetime.cancellation.cancel();
    }

    fn stop_worker(&self) {
        self.lifetime.cancellation.stop_worker();
    }
}

async fn wait_for_popup_io_cancellation(cancelled_rx: &mut watch::Receiver<bool>) {
    loop {
        if *cancelled_rx.borrow_and_update() {
            return;
        }
        if cancelled_rx.changed().await.is_err() {
            return;
        }
    }
}

fn drain_popup_io_requests(request_rx: &mut mpsc::Receiver<PopupIoRequest>) {
    request_rx.close();
    while let Ok(request) = request_rx.try_recv() {
        let _ = request.result_tx.send(Err(popup_io_cancelled_error()));
    }
}

fn popup_io_cancelled_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::Interrupted,
        "popup I/O cancelled during teardown",
    )
}
