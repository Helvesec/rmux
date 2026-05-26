#![cfg(unix)]
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::error::Error;
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::net::UnixListener as StdUnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rmux_proto::{
    decode_frame, encode_frame, AttachSessionRequest, AttachSessionResponse, FrameDecoder, Request,
    Response, RmuxError, SessionName, TerminalSize, DEFAULT_MAX_FRAME_LENGTH, RMUX_FRAME_MAGIC,
    RMUX_WIRE_VERSION,
};
use rmux_server::{DaemonConfig, ServerDaemon, ServerHandle};
use rustix::event::{poll, PollFd, PollFlags, Timespec};
use rustix::termios::{
    tcgetattr, tcgetwinsize, tcsetattr, OptionalActions, SpecialCodeIndex, Termios,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PTY_TEST_LOCK: Mutex<()> = Mutex::const_new(());

/// Resolve a usable `bash` for integration tests at runtime.
///
/// Mirrors `rmux_server::test_shell::test_bash_path` (private to the lib).
/// Integration tests live in a separate compilation unit and can't reach
/// `pub(crate)` items, so the logic is duplicated rather than re-exposed.
/// Resolution: `$RMUX_TEST_BASH` → `/bin/bash` if present → first
/// `bash` on `$PATH` → bare `"bash"`.
pub(crate) fn test_bash_path() -> String {
    use std::sync::OnceLock;
    static CACHED: OnceLock<String> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            if let Ok(value) = std::env::var("RMUX_TEST_BASH") {
                if !value.is_empty() {
                    return value;
                }
            }
            if Path::new("/bin/bash").is_file() {
                return "/bin/bash".to_owned();
            }
            if let Some(path_var) = std::env::var_os("PATH") {
                for dir in std::env::split_paths(&path_var) {
                    let candidate = dir.join("bash");
                    if candidate.is_file() {
                        return candidate.to_string_lossy().into_owned();
                    }
                }
            }
            "bash".to_owned()
        })
        .clone()
}

pub(crate) async fn start_server(harness: &TestHarness) -> Result<ServerHandle, Box<dyn Error>> {
    let socket_path = harness.socket_path().to_path_buf();
    ServerDaemon::new(DaemonConfig::new(socket_path))
        .bind()
        .await
        .map_err(Into::into)
}

pub(crate) async fn send_request(
    socket_path: &Path,
    request: &Request,
) -> Result<Response, Box<dyn Error>> {
    let mut client = ClientConnection::connect(socket_path).await?;
    client.send_request(request).await
}

pub(crate) fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

pub(crate) fn create_stale_socket(socket_path: &Path) -> Result<StdUnixListener, Box<dyn Error>> {
    let parent = socket_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket path must include a parent directory",
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let listener = StdUnixListener::bind(socket_path)?;
    Ok(listener)
}

pub(crate) async fn wait_for_socket_removal(socket_path: &Path) -> Result<(), Box<dyn Error>> {
    for _ in 0..200 {
        if !socket_path.exists() {
            return Ok(());
        }

        tokio::task::yield_now().await;
    }

    Err(io::Error::other(format!(
        "socket '{}' was not removed after drop",
        socket_path.display()
    ))
    .into())
}

pub(crate) fn pane_tty_paths() -> Result<BTreeSet<PathBuf>, Box<dyn Error>> {
    let mut paths = BTreeSet::new();

    for pid in pane_child_pids()? {
        let target = match std::fs::read_link(format!("/proc/{pid}/fd/0")) {
            Ok(target) => target,
            Err(_) => continue,
        };

        if is_pts_device(&target) {
            paths.insert(target);
        }
    }

    Ok(paths)
}

pub(crate) fn pane_child_pids() -> Result<BTreeSet<u32>, Box<dyn Error>> {
    let task_directory = format!("/proc/{}/task", std::process::id());
    let tasks = match std::fs::read_dir(task_directory) {
        Ok(tasks) => tasks,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(error.into()),
    };

    let mut pids = BTreeSet::new();

    for task in tasks {
        let task = task?;
        let children = match std::fs::read_to_string(task.path().join("children")) {
            Ok(children) => children,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };

        for pid in children.split_whitespace() {
            pids.insert(pid.parse()?);
        }
    }

    Ok(pids)
}

pub(crate) fn tty_size(path: &Path) -> Result<TerminalSize, Box<dyn Error>> {
    let file = std::fs::File::open(path)?;
    let winsize = tcgetwinsize(&file)?;

    Ok(TerminalSize {
        cols: winsize.ws_col,
        rows: winsize.ws_row,
    })
}

pub(crate) struct RawTty {
    file: std::fs::File,
    original_termios: Termios,
}

impl RawTty {
    pub(crate) fn open(path: &Path) -> Result<Self, Box<dyn Error>> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let original_termios = tcgetattr(&file)?;
        let mut raw_termios = original_termios.clone();
        raw_termios.make_raw();
        raw_termios.special_codes[SpecialCodeIndex::VMIN] = 1;
        raw_termios.special_codes[SpecialCodeIndex::VTIME] = 0;
        tcsetattr(&file, OptionalActions::Now, &raw_termios)?;

        Ok(Self {
            file,
            original_termios,
        })
    }

    pub(crate) fn read_exact(&mut self, len: usize) -> Result<Vec<u8>, Box<dyn Error>> {
        let mut buffer = vec![0; len];
        self.file.read_exact(&mut buffer)?;
        Ok(buffer)
    }

    pub(crate) fn read_exact_with_timeout(
        &mut self,
        len: usize,
        timeout: Duration,
    ) -> Result<Vec<u8>, Box<dyn Error>> {
        let mut fds = [PollFd::new(
            &self.file,
            PollFlags::IN | PollFlags::ERR | PollFlags::HUP,
        )];
        let timeout = Timespec {
            tv_sec: timeout.as_secs() as i64,
            tv_nsec: timeout.subsec_nanos() as i64,
        };

        let ready = poll(&mut fds, Some(&timeout))?;
        if ready == 0 || fds[0].revents().is_empty() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "tty read timed out").into());
        }

        self.read_exact(len)
    }

    pub(crate) fn write_all(&mut self, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
        use std::io::Write;

        self.file.write_all(bytes)?;
        self.file.flush()?;
        Ok(())
    }
}

impl Drop for RawTty {
    fn drop(&mut self) {
        let _ = tcsetattr(&self.file, OptionalActions::Now, &self.original_termios);
    }
}

fn is_pts_device(path: &Path) -> bool {
    path.parent() == Some(Path::new("/dev/pts"))
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.chars().all(|character| character.is_ascii_digit()))
            .unwrap_or(false)
}

pub(crate) struct ClientConnection {
    stream: UnixStream,
    decoder: FrameDecoder,
    read_buffer: [u8; 4096],
}

impl ClientConnection {
    pub(crate) async fn connect(socket_path: &Path) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            stream: UnixStream::connect(socket_path).await?,
            decoder: FrameDecoder::new(),
            read_buffer: [0; 4096],
        })
    }

    pub(crate) async fn send_request(
        &mut self,
        request: &Request,
    ) -> Result<Response, Box<dyn Error>> {
        let frame = encode_frame(request)?;
        self.stream.write_all(&frame).await?;
        self.read_response().await
    }

    async fn read_response(&mut self) -> Result<Response, Box<dyn Error>> {
        loop {
            match self.decoder.next_frame::<Response>() {
                Ok(Some(response)) => return Ok(response),
                Ok(None) => {}
                Err(error) => return Err(Box::new(error)),
            }

            let bytes_read = self.stream.read(&mut self.read_buffer).await?;
            if bytes_read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed before a response frame arrived",
                )
                .into());
            }

            self.decoder.push_bytes(&self.read_buffer[..bytes_read]);
        }
    }

    pub(crate) async fn begin_attach(
        mut self,
        request: AttachSessionRequest,
    ) -> Result<(AttachSessionResponse, UnixStream), Box<dyn Error>> {
        let frame = encode_frame(&Request::AttachSession(request))?;
        self.stream.write_all(&frame).await?;

        match read_response_exact(&mut self.stream).await? {
            Response::AttachSession(response) => Ok((response, self.stream)),
            other => Err(io::Error::other(format!("unexpected attach response: {other:?}")).into()),
        }
    }
}

pub(crate) async fn read_response_exact(
    stream: &mut UnixStream,
) -> Result<Response, Box<dyn Error>> {
    let frame = read_detached_frame_exact(stream).await?;
    decode_frame(&frame).map_err(Into::into)
}

async fn read_detached_frame_exact(stream: &mut UnixStream) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut frame = Vec::new();
    let mut magic = [0_u8; 1];
    stream.read_exact(&mut magic).await?;
    if magic[0] != RMUX_FRAME_MAGIC {
        return Err(RmuxError::BadFrameMagic(magic[0]).into());
    }
    frame.push(magic[0]);

    let version = read_varint_u32_exact(stream, &mut frame).await?;
    if version != RMUX_WIRE_VERSION {
        return Err(RmuxError::UnsupportedWireVersion {
            got: version,
            minimum: RMUX_WIRE_VERSION,
            maximum: RMUX_WIRE_VERSION,
        }
        .into());
    }

    let mut length_bytes = [0_u8; 4];
    stream.read_exact(&mut length_bytes).await?;
    frame.extend_from_slice(&length_bytes);
    let length = u32::from_le_bytes(length_bytes) as usize;
    if length == 0 {
        return Err(RmuxError::EmptyFrame.into());
    }
    if length > DEFAULT_MAX_FRAME_LENGTH {
        return Err(RmuxError::FrameTooLarge {
            length,
            maximum: DEFAULT_MAX_FRAME_LENGTH,
        }
        .into());
    }

    let mut payload = vec![0_u8; length];
    stream.read_exact(&mut payload).await?;
    frame.extend_from_slice(&payload);
    Ok(frame)
}

async fn read_varint_u32_exact(
    stream: &mut UnixStream,
    frame: &mut Vec<u8>,
) -> Result<u32, Box<dyn Error>> {
    let mut value = 0_u32;
    for index in 0..5 {
        let mut byte = [0_u8; 1];
        stream.read_exact(&mut byte).await?;
        let byte = byte[0];
        frame.push(byte);
        value |= u32::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }

    Err(RmuxError::Decode("wire-version varint exceeds u32 length".to_owned()).into())
}

pub(crate) struct TestHarness {
    root: PathBuf,
    socket_path: PathBuf,
}

impl TestHarness {
    pub(crate) fn new(label: &str) -> Self {
        let unique_id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from("/tmp").join(format!(
            "rxs-{}-{}-{unique_id}",
            compact_label(label),
            std::process::id()
        ));
        let socket_path = root.join("s.sock");

        Self { root, socket_path }
    }

    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

fn compact_label(label: &str) -> String {
    let compact = label
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(16)
        .collect::<String>();
    if compact.is_empty() {
        "x".to_owned()
    } else {
        compact
    }
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
