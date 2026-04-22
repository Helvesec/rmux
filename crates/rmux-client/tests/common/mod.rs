#![allow(dead_code)]

use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rmux_proto::SessionName;
use rmux_server::{DaemonConfig, ServerDaemon};
use tokio::runtime::Builder;
use tokio::sync::oneshot;

static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);
const SERVER_READY_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn start_server(harness: &TestHarness) -> Result<TestServer, Box<dyn Error>> {
    TestServer::start(harness.socket_path().to_path_buf())
}

pub(crate) fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

pub(crate) struct TestServer {
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<Result<(), String>>>,
}

impl TestServer {
    fn start(socket_path: PathBuf) -> Result<Self, Box<dyn Error>> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let thread = thread::spawn(move || -> Result<(), String> {
            let runtime = Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|error| error.to_string())?;

            runtime.block_on(async move {
                let handle = match ServerDaemon::new(DaemonConfig::new(socket_path))
                    .bind()
                    .await
                {
                    Ok(handle) => handle,
                    Err(error) => {
                        let message = error.to_string();
                        let _ = ready_tx.send(Err(message.clone()));
                        return Err(message);
                    }
                };

                if ready_tx.send(Ok(())).is_err() {
                    return handle.shutdown().await.map_err(|error| error.to_string());
                }

                let _ = shutdown_rx.await;
                handle.shutdown().await.map_err(|error| error.to_string())
            })
        });

        match ready_rx.recv_timeout(SERVER_READY_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                shutdown: Some(shutdown_tx),
                thread: Some(thread),
            }),
            Ok(Err(message)) => {
                join_server_thread(thread)?;
                Err(io::Error::other(message).into())
            }
            Err(error) => {
                let _ = shutdown_tx.send(());
                let _ = join_server_thread(thread);
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("server failed to become ready: {error}"),
                )
                .into())
            }
        }
    }

    pub(crate) fn shutdown(&mut self) -> Result<(), Box<dyn Error>> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }

        if let Some(thread) = self.thread.take() {
            join_server_thread(thread)?;
        }

        Ok(())
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

pub(crate) struct TestHarness {
    root: PathBuf,
    socket_path: PathBuf,
}

impl TestHarness {
    pub(crate) fn new(label: &str) -> Self {
        let unique_id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "rmux-client-test-{label}-{}-{unique_id}",
            std::process::id()
        ));
        let socket_path = root.join("default.sock");

        Self { root, socket_path }
    }

    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn join_server_thread(
    thread: thread::JoinHandle<Result<(), String>>,
) -> Result<(), Box<dyn Error>> {
    thread
        .join()
        .map_err(|_| io::Error::other("server thread panicked"))?
        .map_err(io::Error::other)?;
    Ok(())
}
