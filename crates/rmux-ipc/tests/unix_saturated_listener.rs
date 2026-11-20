#![cfg(unix)]

//! A live listener whose accept queue is full is busy, not absent.
//!
//! Darwin refuses such a connect with ECONNREFUSED, exactly as for a socket
//! file left behind by a dead daemon, and Linux rejects a non-blocking one
//! with EAGAIN. `connect_blocking` must wait for the queue within its timeout
//! on both, and keep the immediate refusal for a socket that nobody serves.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rmux_ipc::{connect_blocking, LocalEndpoint};

static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

#[test]
fn a_saturated_live_listener_times_out_instead_of_being_reported_absent() -> io::Result<()> {
    let root = unique_root("timeout");
    let socket_path = root.join("s");
    let saturated = SaturatedListener::bind(&socket_path)?;
    let endpoint = LocalEndpoint::from_path(socket_path.clone());
    let timeout = Duration::from_millis(300);

    let started = Instant::now();
    let error = connect_blocking(&endpoint, timeout)
        .expect_err("nobody accepts, so the connect cannot complete");
    let waited = started.elapsed();

    assert_eq!(
        error.kind(),
        io::ErrorKind::TimedOut,
        "a full accept queue is a busy server, not an absent one: {error}"
    );
    assert!(
        waited >= timeout,
        "returned after {waited:?}, before its timeout"
    );
    assert!(socket_path.exists(), "the live socket must stay in place");
    drop(saturated);
    fs::remove_dir_all(root)
}

#[test]
fn a_saturated_live_listener_is_joined_once_it_accepts() -> io::Result<()> {
    let root = unique_root("joined");
    let socket_path = root.join("s");
    let saturated = SaturatedListener::bind(&socket_path)?;
    let endpoint = LocalEndpoint::from_path(socket_path);
    let acceptor = saturated.accept_after(Duration::from_millis(150));

    let mut stream = connect_blocking(&endpoint, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut greeting = [0_u8; 1];
    stream.read_exact(&mut greeting)?;

    assert_eq!(&greeting, b"!", "the stream must reach the live listener");
    acceptor.stop();
    drop(saturated);
    fs::remove_dir_all(root)
}

#[test]
fn a_socket_file_without_a_listener_is_refused_at_once() -> io::Result<()> {
    let root = unique_root("stale");
    let socket_path = root.join("s");
    drop(UnixListener::bind(&socket_path)?);
    let endpoint = LocalEndpoint::from_path(socket_path);

    let started = Instant::now();
    let error = connect_blocking(&endpoint, Duration::from_secs(5))
        .expect_err("a socket file without a listener cannot be joined");

    assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "an absent server must not cost the connect timeout"
    );
    fs::remove_dir_all(root)
}

/// A bound listener with a backlog of one whose queue is already full.
struct SaturatedListener {
    listener: Arc<UnixListener>,
    _queued: Vec<queue::QueuedConnect>,
}

impl SaturatedListener {
    fn bind(socket_path: &Path) -> io::Result<Self> {
        let listener = UnixListener::bind(socket_path)?;
        rustix::net::listen(&listener, 1)?;
        let mut queued = Vec::new();
        loop {
            match queue::QueuedConnect::start(socket_path)? {
                Some(connect) => queued.push(connect),
                None => break,
            }
            assert!(queued.len() < 64, "the accept queue never filled");
        }
        Ok(Self {
            listener: Arc::new(listener),
            _queued: queued,
        })
    }

    /// Accepts every queued and future connection after `delay`, greeting each.
    fn accept_after(&self, delay: Duration) -> Acceptor {
        let listener = Arc::clone(&self.listener);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            thread::sleep(delay);
            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            let mut accepted = Vec::new();
            while !stop_for_thread.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.write_all(b"!");
                        accepted.push(stream);
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            }
        });
        Acceptor {
            stop,
            thread: Some(thread),
        }
    }
}

struct Acceptor {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Acceptor {
    fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("acceptor thread");
        }
    }
}

mod queue {
    use std::io;
    use std::os::fd::OwnedFd;
    use std::path::Path;

    use rustix::net::{connect, socket, AddressFamily, SocketAddrUnix, SocketType};

    /// One non-blocking connect left waiting in the listener's accept queue.
    pub(super) struct QueuedConnect {
        _socket: OwnedFd,
    }

    impl QueuedConnect {
        /// Returns `None` once the queue is full: Darwin refuses the connect
        /// (ECONNREFUSED) and Linux rejects it (EAGAIN).
        pub(super) fn start(socket_path: &Path) -> io::Result<Option<Self>> {
            let socket = socket(AddressFamily::UNIX, SocketType::STREAM, None)?;
            rustix::io::ioctl_fionbio(&socket, true)?;
            match connect(&socket, &SocketAddrUnix::new(socket_path)?) {
                Ok(()) => Ok(Some(Self { _socket: socket })),
                Err(rustix::io::Errno::CONNREFUSED | rustix::io::Errno::AGAIN) => Ok(None),
                Err(error) => Err(error.into()),
            }
        }
    }
}

fn unique_root(label: &str) -> PathBuf {
    let unique = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
    let root = PathBuf::from("/tmp").join(format!("ris-{label}-{}-{unique}", std::process::id()));
    fs::create_dir_all(&root).expect("create test root");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("secure test root");
    root
}
