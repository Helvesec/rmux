use std::fs::{self, File, Metadata};
use std::future::Future;
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::thread;
use std::time::Duration;

use rustix::event::{poll, PollFd, PollFlags, Timespec};
use rustix::fs::{Mode, OFlags};
use tokio::task::JoinHandle;

#[cfg(target_os = "macos")]
#[path = "darwin_fifo_reader.rs"]
mod fifo_reader;
#[cfg(not(target_os = "macos"))]
#[path = "fifo_reader_fallback.rs"]
mod fifo_reader;

const PHASE_PENDING: u8 = 0;
const PHASE_OPENING: u8 = 1;
const PHASE_OPENED: u8 = 2;
const PHASE_FINISHED: u8 = 3;
const RETRY_INTERVAL: Duration = Duration::from_millis(5);
const OPEN_RETRY_LIMIT: usize = 16;
const RETRY_POLL_TIMEOUT: Timespec = Timespec {
    tv_sec: 0,
    tv_nsec: 5_000_000,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SpecialFileKind {
    Fifo,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

impl FileIdentity {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SpecialFile {
    kind: SpecialFileKind,
    identity: FileIdentity,
}

enum ReadTarget {
    Ordinary(File),
    Special(File, SpecialFile),
}

enum WriteTarget {
    Ordinary(File),
    Special(Option<File>, SpecialFile),
}

#[derive(Debug)]
struct CancellationState {
    path: PathBuf,
    special: SpecialFile,
    cancelled: AtomicBool,
    phase: AtomicU8,
}

impl CancellationState {
    fn new(path: PathBuf, special: SpecialFile) -> Self {
        Self {
            path,
            special,
            cancelled: AtomicBool::new(false),
            phase: AtomicU8::new(PHASE_PENDING),
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn interrupted_error() -> io::Error {
        io::Error::new(
            io::ErrorKind::Interrupted,
            "buffer FIFO operation cancelled",
        )
    }
}

#[derive(Debug)]
struct CancellationGuard {
    state: Arc<CancellationState>,
    armed: bool,
}

impl CancellationGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancellationGuard {
    fn drop(&mut self) {
        if self.armed {
            self.state.cancel();
        }
    }
}

struct WorkerPhaseGuard(Arc<CancellationState>);

impl Drop for WorkerPhaseGuard {
    fn drop(&mut self) {
        self.0.phase.store(PHASE_FINISHED, Ordering::Release);
    }
}

struct FifoOperation<T> {
    task: JoinHandle<io::Result<T>>,
    cancellation: CancellationGuard,
}

impl<T> Future for FifoOperation<T> {
    type Output = io::Result<T>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match Pin::new(&mut this.task).poll(context) {
            Poll::Ready(Ok(result)) => {
                this.cancellation.disarm();
                Poll::Ready(result)
            }
            Poll::Ready(Err(error)) => {
                this.cancellation.disarm();
                Poll::Ready(Err(io::Error::other(format!(
                    "buffer FIFO worker failed: {error}"
                ))))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

pub(super) fn special_path(path: &Path) -> io::Result<Option<SpecialFile>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(special_file(&metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(super) async fn read(path: PathBuf) -> io::Result<Vec<u8>> {
    // Preserve the macOS helper's blocking-open handshake for paths that are
    // already FIFOs. If this observation becomes stale, every later open is
    // still O_NONBLOCK and descriptor-classified before any read.
    if let Some(special) = special_path(&path)? {
        return start_read(path, special, None).await;
    }

    match open_read_target(&path)? {
        ReadTarget::Ordinary(mut file) => tokio::task::spawn_blocking(move || {
            let mut content = Vec::new();
            file.read_to_end(&mut content)?;
            Ok(content)
        })
        .await
        .map_err(|error| io::Error::other(format!("buffer file reader failed: {error}")))?,
        ReadTarget::Special(file, special) => {
            read_special_discovered_after_classification(path, file, special).await
        }
    }
}

#[cfg(not(target_os = "macos"))]
async fn read_special_discovered_after_classification(
    path: PathBuf,
    file: File,
    special: SpecialFile,
) -> io::Result<Vec<u8>> {
    start_read(path, special, Some(file)).await
}

#[cfg(target_os = "macos")]
async fn read_special_discovered_after_classification(
    _path: PathBuf,
    _file: File,
    _special: SpecialFile,
) -> io::Result<Vec<u8>> {
    // A nonblocking FIFO descriptor counts as a reader on macOS. Handing the
    // path to the blocking helper after this point can lose an instantaneous
    // writer's bytes between the two opens. Fail the raced operation instead;
    // a path observed as a FIFO initially still uses the helper above.
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "buffer path changed to a special file during open",
    ))
}

pub(super) async fn write(path: PathBuf, content: Vec<u8>, append: bool) -> io::Result<()> {
    match open_write_target(&path, append)? {
        WriteTarget::Ordinary(mut file) => tokio::task::spawn_blocking(move || {
            if !append {
                file.set_len(0)?;
            }
            file.write_all(&content)
        })
        .await
        .map_err(|error| io::Error::other(format!("buffer file writer failed: {error}")))?,
        WriteTarget::Special(file, special) => {
            start_write(path, content, append, special, file).await
        }
    }
}

fn start_read(path: PathBuf, special: SpecialFile, file: Option<File>) -> FifoOperation<Vec<u8>> {
    let state = Arc::new(CancellationState::new(path, special));
    let worker_state = Arc::clone(&state);
    let task = tokio::task::spawn_blocking(move || read_special(worker_state, file));
    FifoOperation {
        task,
        cancellation: CancellationGuard { state, armed: true },
    }
}

fn start_write(
    path: PathBuf,
    content: Vec<u8>,
    append: bool,
    special: SpecialFile,
    file: Option<File>,
) -> FifoOperation<()> {
    let state = Arc::new(CancellationState::new(path, special));
    let worker_state = Arc::clone(&state);
    let task =
        tokio::task::spawn_blocking(move || write_special(worker_state, &content, append, file));
    FifoOperation {
        task,
        cancellation: CancellationGuard { state, armed: true },
    }
}

fn read_special(state: Arc<CancellationState>, file: Option<File>) -> io::Result<Vec<u8>> {
    let _phase_guard = WorkerPhaseGuard(Arc::clone(&state));

    if state.special.kind == SpecialFileKind::Fifo {
        if let Some(result) = fifo_reader::try_read_fifo(&state) {
            return result;
        }
    }

    let mut file = match file {
        Some(file) => use_open_file(&state, file)?,
        None => match state.special.kind {
            SpecialFileKind::Fifo => open_fifo_reader(&state)?,
            SpecialFileKind::Other => open_nonblocking(&state, OFlags::RDONLY, Mode::empty())?,
        },
    };

    let mut content = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut observed_fifo_writer = false;
    loop {
        ensure_not_cancelled(&state)?;
        if state.special.kind == SpecialFileKind::Fifo {
            // Poll is the bounded wait. We still issue one nonblocking read
            // after a timeout: EAGAIN distinguishes a live empty writer from
            // the pre-writer EOF returned by FIFOs.
            observed_fifo_writer |= fifo_read_ready(&file)?;
        }
        match file.read(&mut buffer) {
            // A nonblocking FIFO opened before its first writer also returns
            // zero. Treat EOF as final only after a writer has been observed;
            // otherwise load-buffer would complete before a writer has had a
            // chance to connect. Probe after each bounded poll timeout since
            // macOS does not reliably surface POLLHUP after the last writer
            // closes.
            Ok(0) if state.special.kind == SpecialFileKind::Fifo && !observed_fifo_writer => {
                thread::sleep(RETRY_INTERVAL);
            }
            Ok(0) => return Ok(content),
            Ok(length) => {
                observed_fifo_writer = true;
                content.extend_from_slice(&buffer[..length]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if state.special.kind == SpecialFileKind::Fifo {
                    observed_fifo_writer = true;
                }
                thread::sleep(RETRY_INTERVAL);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

pub(super) fn run_internal_fifo_reader_helper<I>(arguments: I) -> Option<i32>
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    fifo_reader::run_helper_if_requested(arguments)
}

fn write_special(
    state: Arc<CancellationState>,
    content: &[u8],
    append: bool,
    file: Option<File>,
) -> io::Result<()> {
    let _phase_guard = WorkerPhaseGuard(Arc::clone(&state));
    let mut file = match file {
        Some(file) => use_open_file(&state, file)?,
        None => match state.special.kind {
            SpecialFileKind::Fifo => open_fifo_writer(&state)?,
            SpecialFileKind::Other => {
                // The path was already classified as an existing non-regular
                // special file. Do not use CREATE or TRUNC here: either flag
                // can mutate an attacker-replaced regular file before its
                // identity is validated. APPEND only affects stream position.
                let disposition = if append {
                    OFlags::APPEND
                } else {
                    OFlags::empty()
                };
                open_nonblocking(&state, OFlags::WRONLY | disposition, Mode::empty())?
            }
        },
    };

    let mut offset = 0;
    while offset < content.len() {
        ensure_not_cancelled(&state)?;
        match file.write(&content[offset..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write buffer FIFO",
                ));
            }
            Ok(length) => offset += length,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(RETRY_INTERVAL);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn open_read_target(path: &Path) -> io::Result<ReadTarget> {
    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(errno_to_io)?;
    let file = File::from(descriptor);
    match special_file(&file.metadata()?) {
        Some(special) => Ok(ReadTarget::Special(file, special)),
        None => Ok(ReadTarget::Ordinary(file)),
    }
}

fn open_write_target(path: &Path, append: bool) -> io::Result<WriteTarget> {
    let disposition = if append {
        OFlags::APPEND
    } else {
        OFlags::empty()
    };
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::NONBLOCK | OFlags::CLOEXEC | disposition;

    for _ in 0..OPEN_RETRY_LIMIT {
        match rustix::fs::open(path, flags, Mode::from_raw_mode(0o666)) {
            Ok(descriptor) => {
                let file = File::from(descriptor);
                return match special_file(&file.metadata()?) {
                    Some(special) => Ok(WriteTarget::Special(Some(file), special)),
                    None => Ok(WriteTarget::Ordinary(file)),
                };
            }
            Err(error) if error == rustix::io::Errno::NXIO => {
                if let Some(special) = special_path(path)? {
                    return Ok(WriteTarget::Special(None, special));
                }
            }
            Err(error) => return Err(errno_to_io(error)),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "buffer path changed repeatedly during nonblocking open",
    ))
}

fn use_open_file(state: &CancellationState, file: File) -> io::Result<File> {
    ensure_not_cancelled(state)?;
    ensure_file_identity(state, &file)?;
    state.phase.store(PHASE_OPENED, Ordering::Release);
    ensure_not_cancelled(state)?;
    Ok(file)
}

fn open_fifo_reader(state: &CancellationState) -> io::Result<File> {
    open_nonblocking(state, OFlags::RDONLY, Mode::empty())
}

fn open_fifo_writer(state: &CancellationState) -> io::Result<File> {
    ensure_not_cancelled(state)?;
    state.phase.store(PHASE_OPENING, Ordering::Release);
    loop {
        ensure_not_cancelled(state)?;
        ensure_path_identity(state)?;
        match rustix::fs::open(
            &state.path,
            OFlags::WRONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(descriptor) => {
                let file = File::from(descriptor);
                ensure_file_identity(state, &file)?;
                state.phase.store(PHASE_OPENED, Ordering::Release);
                ensure_not_cancelled(state)?;
                return Ok(file);
            }
            Err(error) if error == rustix::io::Errno::INTR => continue,
            Err(error) if error == rustix::io::Errno::NXIO => {
                thread::sleep(RETRY_INTERVAL);
            }
            Err(error) => return Err(errno_to_io(error)),
        }
    }
}

fn open_nonblocking(state: &CancellationState, flags: OFlags, mode: Mode) -> io::Result<File> {
    ensure_not_cancelled(state)?;
    state.phase.store(PHASE_OPENING, Ordering::Release);
    ensure_not_cancelled(state)?;
    ensure_path_identity(state)?;

    let descriptor = rustix::fs::open(
        &state.path,
        flags | OFlags::NONBLOCK | OFlags::CLOEXEC,
        mode,
    )
    .map_err(errno_to_io)?;
    let file = File::from(descriptor);
    ensure_file_identity(state, &file)?;
    state.phase.store(PHASE_OPENED, Ordering::Release);
    ensure_not_cancelled(state)?;
    Ok(file)
}

fn fifo_read_ready(file: &File) -> io::Result<bool> {
    let mut descriptors = [PollFd::new(
        file,
        PollFlags::IN | PollFlags::HUP | PollFlags::ERR,
    )];
    match poll(&mut descriptors, Some(&RETRY_POLL_TIMEOUT)) {
        Ok(0) => Ok(false),
        Ok(_) => Ok(!descriptors[0].revents().is_empty()),
        Err(error) if error == rustix::io::Errno::INTR => Ok(false),
        Err(error) => Err(errno_to_io(error)),
    }
}

fn ensure_path_identity(state: &CancellationState) -> io::Result<()> {
    let metadata = fs::metadata(&state.path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "buffer special-file path disappeared during open",
            )
        } else {
            error
        }
    })?;
    ensure_identity(state, &metadata)
}

fn ensure_file_identity(state: &CancellationState, file: &File) -> io::Result<()> {
    ensure_identity(state, &file.metadata()?)
}

fn ensure_identity(state: &CancellationState, metadata: &Metadata) -> io::Result<()> {
    let actual = FileIdentity::from_metadata(metadata);
    if actual == state.special.identity
        && special_file_kind(metadata).is_some_and(|kind| kind == state.special.kind)
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "buffer special-file path changed during open",
        ))
    }
}

fn special_file_kind(metadata: &Metadata) -> Option<SpecialFileKind> {
    let file_type = metadata.file_type();
    if file_type.is_fifo() {
        Some(SpecialFileKind::Fifo)
    } else if file_type.is_block_device() || file_type.is_char_device() || file_type.is_socket() {
        Some(SpecialFileKind::Other)
    } else {
        None
    }
}

fn special_file(metadata: &Metadata) -> Option<SpecialFile> {
    special_file_kind(metadata).map(|kind| SpecialFile {
        kind,
        identity: FileIdentity::from_metadata(metadata),
    })
}

fn ensure_not_cancelled(state: &CancellationState) -> io::Result<()> {
    if state.cancelled.load(Ordering::Acquire) {
        Err(CancellationState::interrupted_error())
    } else {
        Ok(())
    }
}

fn errno_to_io(error: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_ID: AtomicU64 = AtomicU64::new(0);

    #[tokio::test]
    async fn cancelling_fifo_read_unblocks_the_open_worker() {
        let path = test_fifo("cancel-read");
        let operation = start_read(path.clone(), classify_special(&path), None);
        let state = Arc::clone(&operation.cancellation.state);
        wait_for_phase(&state, PHASE_OPENED).await;

        drop(operation);
        wait_for_phase(&state, PHASE_FINISHED).await;

        std::fs::remove_file(path).expect("remove test FIFO");
    }

    #[tokio::test]
    async fn cancelling_fifo_write_unblocks_the_open_worker() {
        let path = test_fifo("cancel-write");
        let operation = start_write(
            path.clone(),
            b"blocked".to_vec(),
            false,
            classify_special(&path),
            None,
        );
        let state = Arc::clone(&operation.cancellation.state);
        wait_for_phase(&state, PHASE_OPENING).await;

        drop(operation);
        wait_for_phase(&state, PHASE_FINISHED).await;

        std::fs::remove_file(path).expect("remove test FIFO");
    }

    #[tokio::test]
    async fn nonblocking_fifo_read_waits_for_a_writer_and_drains_to_eof() {
        let path = test_fifo("successful-read");
        let operation = start_read(path.clone(), classify_special(&path), None);
        let writer_path = path.clone();
        let writer = tokio::task::spawn_blocking(move || std::fs::write(writer_path, b"payload"));

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), operation)
                .await
                .expect("FIFO read must finish after writer EOF")
                .expect("read FIFO payload"),
            b"payload"
        );
        writer
            .await
            .expect("join FIFO writer")
            .expect("write FIFO payload");
        std::fs::remove_file(path).expect("remove test FIFO");
    }

    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn nonblocking_fifo_read_observes_an_instantaneous_empty_writer_then_eof() {
        let path = test_fifo("empty-read");
        let operation = start_read(path.clone(), classify_special(&path), None);
        let writer_path = path.clone();
        let writer = tokio::task::spawn_blocking(move || {
            drop(
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(writer_path)
                    .expect("open and immediately close empty FIFO writer"),
            );
        });

        assert!(tokio::time::timeout(Duration::from_secs(2), operation)
            .await
            .expect("FIFO read must observe the instantaneous empty writer's EOF")
            .expect("read empty FIFO payload")
            .is_empty());
        writer.await.expect("join empty FIFO writer");
        std::fs::remove_file(path).expect("remove test FIFO");
    }

    #[tokio::test]
    async fn nonblocking_fifo_write_waits_for_a_reader_and_delivers_all_bytes() {
        let path = test_fifo("successful-write");
        let operation = start_write(
            path.clone(),
            b"payload".to_vec(),
            false,
            classify_special(&path),
            None,
        );
        let reader_path = path.clone();
        let reader = tokio::task::spawn_blocking(move || std::fs::read(reader_path));

        tokio::time::timeout(Duration::from_secs(2), operation)
            .await
            .expect("FIFO write must finish after reader connects")
            .expect("write FIFO payload");
        assert_eq!(
            reader
                .await
                .expect("join FIFO reader")
                .expect("read FIFO payload"),
            b"payload"
        );
        std::fs::remove_file(path).expect("remove test FIFO");
    }

    #[tokio::test]
    async fn replacing_an_opening_fifo_write_fails_without_stranding_the_worker() {
        let path = test_fifo("cancel-replaced-write");
        let replacement = path.with_extension("replacement");
        create_fifo(&replacement);
        let operation = start_write(
            path.clone(),
            b"blocked".to_vec(),
            false,
            classify_special(&path),
            None,
        );
        let state = Arc::clone(&operation.cancellation.state);
        wait_for_phase(&state, PHASE_OPENING).await;

        std::fs::rename(&replacement, &path).expect("atomically replace opening FIFO");
        let error = tokio::time::timeout(Duration::from_secs(2), operation)
            .await
            .expect("path replacement must not strand the FIFO worker")
            .expect_err("replacement must not receive the original write");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        wait_for_phase(&state, PHASE_FINISHED).await;

        std::fs::remove_file(path).expect("remove replacement FIFO");
    }

    #[tokio::test]
    async fn cancelling_an_open_fifo_read_uses_its_original_inode() {
        let path = test_fifo("cancel-replaced-read");
        let original = path.with_extension("original");
        let operation = start_read(path.clone(), classify_special(&path), None);
        let state = Arc::clone(&operation.cancellation.state);
        wait_for_phase(&state, PHASE_OPENED).await;

        std::fs::rename(&path, &original).expect("rename opened FIFO");
        create_fifo(&path);
        drop(operation);
        wait_for_phase(&state, PHASE_FINISHED).await;

        std::fs::remove_file(path).expect("remove replacement FIFO");
        std::fs::remove_file(original).expect("remove original FIFO");
    }

    #[tokio::test]
    async fn character_devices_use_nonblocking_special_file_workers() {
        let path = PathBuf::from("/dev/null");
        assert_eq!(
            special_path(&path)
                .expect("classify /dev/null")
                .map(|special| special.kind),
            Some(SpecialFileKind::Other),
        );
        assert!(read(path.clone()).await.expect("read /dev/null").is_empty());
        write(path, b"discarded".to_vec(), false)
            .await
            .expect("write /dev/null");
    }

    #[tokio::test]
    async fn replaced_special_path_is_rejected_without_truncating_the_replacement() {
        use std::os::unix::fs::symlink;

        let path = std::env::temp_dir().join(format!(
            "rmux-buffer-replaced-device-{}-{}",
            std::process::id(),
            TEST_ID.fetch_add(1, Ordering::Relaxed)
        ));
        symlink("/dev/null", &path).expect("create special-file symlink");
        let special = classify_special(&path);
        std::fs::remove_file(&path).expect("remove special-file symlink");
        std::fs::write(&path, b"preserve-me").expect("create replacement regular file");

        let error = start_write(path.clone(), b"overwrite".to_vec(), false, special, None)
            .await
            .expect_err("replacement must fail stable special-file validation");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            std::fs::read(&path).expect("read replacement regular file"),
            b"preserve-me"
        );

        std::fs::remove_file(path).expect("remove replacement regular file");
    }

    #[tokio::test]
    async fn regular_to_fifo_read_is_safe_after_stale_classification() {
        let path = test_path("regular-to-fifo-read");
        let replacement = path.with_extension("fifo");
        std::fs::write(&path, b"stale regular file").expect("create original regular file");
        assert!(special_path(&path)
            .expect("classify original file")
            .is_none());
        create_fifo(&replacement);
        std::fs::rename(&replacement, &path).expect("atomically install replacement FIFO");

        assert_read_target_is_safe(path).await;
    }

    #[tokio::test]
    async fn missing_to_fifo_read_is_safe_after_stale_classification() {
        let path = test_path("missing-to-fifo-read");
        assert!(special_path(&path)
            .expect("classify missing path")
            .is_none());
        create_fifo(&path);

        assert_read_target_is_safe(path).await;
    }

    #[tokio::test]
    async fn regular_to_fifo_write_uses_a_cancellable_nonblocking_open() {
        let path = test_path("regular-to-fifo-write");
        let replacement = path.with_extension("fifo");
        std::fs::write(&path, b"stale regular file").expect("create original regular file");
        assert!(special_path(&path)
            .expect("classify original file")
            .is_none());
        create_fifo(&replacement);
        std::fs::rename(&replacement, &path).expect("atomically install replacement FIFO");

        assert_write_target_is_cancellable(path).await;
    }

    #[tokio::test]
    async fn missing_to_fifo_write_uses_a_cancellable_nonblocking_open() {
        let path = test_path("missing-to-fifo-write");
        assert!(special_path(&path)
            .expect("classify missing path")
            .is_none());
        create_fifo(&path);

        assert_write_target_is_cancellable(path).await;
    }

    async fn assert_read_target_is_safe(path: PathBuf) {
        let (file, special) = match open_read_target(&path).expect("open replacement FIFO") {
            ReadTarget::Special(file, special) => (file, special),
            ReadTarget::Ordinary(_) => panic!("replacement FIFO must be descriptor-classified"),
        };

        #[cfg(target_os = "macos")]
        {
            let error = read_special_discovered_after_classification(path.clone(), file, special)
                .await
                .expect_err("a raced macOS FIFO read must fail without reopening the path");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            std::fs::remove_file(path).expect("remove replacement FIFO");
        }

        #[cfg(not(target_os = "macos"))]
        {
            let operation = start_read(path.clone(), special, Some(file));
            let state = Arc::clone(&operation.cancellation.state);
            wait_for_phase(&state, PHASE_OPENED).await;

            drop(operation);
            wait_for_phase(&state, PHASE_FINISHED).await;
            std::fs::remove_file(path).expect("remove replacement FIFO");
        }
    }

    async fn assert_write_target_is_cancellable(path: PathBuf) {
        let (file, special) = match open_write_target(&path, false).expect("open replacement FIFO")
        {
            WriteTarget::Special(file, special) => (file, special),
            WriteTarget::Ordinary(_) => panic!("replacement FIFO must be descriptor-classified"),
        };
        let operation = start_write(path.clone(), b"blocked".to_vec(), false, special, file);
        let state = Arc::clone(&operation.cancellation.state);
        wait_for_phase(&state, PHASE_OPENING).await;

        drop(operation);
        wait_for_phase(&state, PHASE_FINISHED).await;
        std::fs::remove_file(path).expect("remove replacement FIFO");
    }

    fn test_fifo(label: &str) -> PathBuf {
        let path = test_path(label).with_extension("fifo");
        create_fifo(&path);
        path
    }

    fn test_path(label: &str) -> PathBuf {
        let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("rmux-buffer-{label}-{}-{id}", std::process::id()))
    }

    fn create_fifo(path: &Path) {
        let output = std::process::Command::new("mkfifo")
            .arg(path)
            .output()
            .expect("run mkfifo");
        assert!(
            output.status.success(),
            "mkfifo failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn classify_special(path: &Path) -> SpecialFile {
        special_path(path)
            .expect("classify special path")
            .expect("path must be special")
    }

    async fn wait_for_phase(state: &CancellationState, expected: u8) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while state.phase.load(Ordering::Acquire) != expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("FIFO worker should reach expected phase");
    }
}
