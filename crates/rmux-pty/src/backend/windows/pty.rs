use std::collections::VecDeque;
use std::io;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_BROKEN_PIPE, ERROR_INVALID_PARAMETER, E_HANDLE, HANDLE, S_OK,
};
use windows_sys::Win32::System::Console::{COORD, HPCON};

use crate::{ProcessId, Result, TerminalGeometry, TerminalSize};

use super::api::{self, ConptyApi};
use super::bounded_write::OverlappedPipeWriter;
use super::flags::{
    conpty_flags_without_passthrough, selected_conpty_flags, standard_conpty_flags, ConptyFlags,
};
use super::io::{create_conpty_input_pipe, create_pipe, PipePair};
use super::perf;
use super::DsrBootstrap;

#[derive(Debug)]
pub(crate) struct WindowsPty {
    state: RwLock<WindowsPtyState>,
    size: Mutex<TerminalSize>,
    dsr_bootstrap: Mutex<Option<DsrBootstrap>>,
}

impl WindowsPty {
    pub(crate) fn hpc(&self) -> HPCON {
        self.state
            .read()
            .expect("ConPTY state lock poisoned")
            .hpc
            .raw()
    }

    pub(crate) fn close_pseudoconsole(&self) {
        let Ok(mut state) = self.state.write() else {
            tracing::warn!(target: "rmux::conpty", "failed to close ConPTY after child exit: state lock poisoned");
            return;
        };
        state.hpc.close();
    }

    pub(crate) fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            if let Some(len) = self.drain_deferred_dsr_bytes(buffer)? {
                return Ok(len);
            }

            let output_read = self.output_read_handle()?;
            let bytes_read = {
                let mut state = self
                    .state
                    .write()
                    .map_err(|_| io::Error::other("ConPTY state lock poisoned"))?;
                let len = buffer.len().min(state.startup_output.len());
                for byte in &mut buffer[..len] {
                    *byte = state
                        .startup_output
                        .pop_front()
                        .expect("counted startup output");
                }
                len
            };
            let bytes_read = if bytes_read == 0 {
                super::io::read(&output_read, buffer)?
            } else {
                bytes_read
            };
            let filtered = {
                let mut dsr_bootstrap = self
                    .dsr_bootstrap
                    .lock()
                    .map_err(|_| io::Error::other("ConPTY DSR mutex poisoned"))?;
                let Some(dsr) = dsr_bootstrap.as_mut() else {
                    return Ok(bytes_read);
                };

                let filtered = dsr.filter(buffer, bytes_read);
                if dsr.is_finished() {
                    *dsr_bootstrap = None;
                }
                filtered
            };
            if let Some(response) = filtered.response {
                self.write_all_with_timeout(response, Duration::from_secs(2))?;
            }
            if filtered.len > 0 || bytes_read == 0 {
                return Ok(filtered.len);
            }
        }
    }

    pub(crate) fn write_all_with_timeout(&self, bytes: &[u8], timeout: Duration) -> io::Result<()> {
        let input_writer = self.input_writer()?;
        input_writer.write_all_with_timeout(bytes, timeout)
    }

    pub(crate) fn write_all_with_stall_recovery(
        &self,
        bytes: &[u8],
        timeout: Duration,
        recovery_grace: Duration,
    ) -> io::Result<()> {
        let input_writer = self.input_writer()?;
        input_writer.write_all_with_stall_recovery(bytes, timeout, recovery_grace)
    }

    pub(crate) fn write_console_utf8_ordered(
        &self,
        process_id: ProcessId,
        bytes: &[u8],
        timeout: Duration,
    ) -> io::Result<()> {
        let input_writer = self.input_writer()?;
        input_writer.run_after_pipe_drain(timeout, || {
            crate::windows_console_input::write_windows_console_utf8(process_id, bytes)
        })
    }

    fn output_read_handle(&self) -> io::Result<OwnedHandle> {
        let state = self
            .state
            .read()
            .map_err(|_| io::Error::other("ConPTY state lock poisoned"))?;
        state.output_read.try_clone()
    }

    fn input_writer(&self) -> io::Result<Arc<OverlappedPipeWriter>> {
        let state = self
            .state
            .read()
            .map_err(|_| io::Error::other("ConPTY state lock poisoned"))?;
        Ok(Arc::clone(&state.input_writer))
    }

    pub(crate) fn enable_dsr_bootstrap(&self) -> io::Result<()> {
        let mut dsr = self
            .dsr_bootstrap
            .lock()
            .map_err(|_| io::Error::other("ConPTY DSR mutex poisoned"))?;
        *dsr = Some(DsrBootstrap::from_env());
        Ok(())
    }

    fn drain_deferred_dsr_bytes(&self, buffer: &mut [u8]) -> io::Result<Option<usize>> {
        let mut dsr_bootstrap = self
            .dsr_bootstrap
            .lock()
            .map_err(|_| io::Error::other("ConPTY DSR mutex poisoned"))?;
        let Some(dsr) = dsr_bootstrap.as_mut() else {
            return Ok(None);
        };
        let drained = dsr.drain_deferred(buffer);
        if dsr.is_finished() {
            *dsr_bootstrap = None;
        }
        Ok(drained)
    }

    // The newly spawned child's master has not escaped to a reader yet.
    pub(crate) fn finish_companion_startup(&self) -> io::Result<()> {
        let mut state = self
            .state
            .write()
            .map_err(|_| io::Error::other("ConPTY state lock poisoned"))?;
        if state.hpc.api.companion {
            state.startup_output =
                super::startup::negotiate(&state.output_read, &state.input_writer)?;
        }
        Ok(())
    }

    /// A coherent snapshot of the console this PTY finally holds.
    ///
    /// Taken under the state guard, so the handle, the API and the flags all
    /// describe the SAME console: a fallback that recreated it will already
    /// have replaced the state, and this reads what replaced it. `create_flags`
    /// is the value actually handed to `CreatePseudoConsole` - zero for the
    /// companion API - and not the selected flags, which are not what the
    /// companion was created with.
    ///
    /// Diagnostic only: compiled with `test-seams`, so no shipped runtime
    /// carries it and no product behaviour changes.
    #[cfg(feature = "test-seams")]
    pub(crate) fn backend_snapshot(&self) -> Option<(u64, bool, u32, bool)> {
        let state = self.state.read().ok()?;
        let companion = state.hpc.api.companion;
        Some((
            state.hpc.raw() as u64,
            companion,
            if companion { 0 } else { state.flags.bits() },
            state.flags.uses_passthrough(),
        ))
    }

    pub(crate) fn uses_passthrough(&self) -> bool {
        self.state
            .read()
            .map(|state| state.hpc.api.companion || state.flags.uses_passthrough())
            .unwrap_or(false)
    }

    pub(crate) fn recreate_without_passthrough(&self) -> Result<()> {
        let size = *self
            .size
            .lock()
            .map_err(|_| io::Error::other("ConPTY size mutex poisoned"))?;
        let state =
            create_pty_state_with_fallback(size, &api::INBOX, conpty_flags_without_passthrough())?;
        let mut current = self
            .state
            .write()
            .map_err(|_| io::Error::other("ConPTY state lock poisoned"))?;
        *current = state;
        tracing::warn!(
            target: "rmux::conpty",
            cols = size.cols,
            rows = size.rows,
            "recreated ConPTY without passthrough"
        );
        Ok(())
    }
}

pub(crate) fn open_pty_pair(size: TerminalSize) -> Result<WindowsPty> {
    let _span = perf::span("conpty_open_pty_pair");
    tracing::debug!(
        target: "rmux::conpty",
        cols = size.cols,
        rows = size.rows,
        "creating ConPTY"
    );
    let state = create_pty_state_with_fallback(size, api::selected(), selected_conpty_flags())?;
    tracing::debug!(
        target: "rmux::conpty",
        cols = size.cols,
        rows = size.rows,
        companion = state.hpc.api.companion,
        "ConPTY created"
    );

    Ok(WindowsPty {
        state: RwLock::new(state),
        size: Mutex::new(size),
        dsr_bootstrap: Mutex::new(None),
    })
}

pub(crate) fn query_size(pty: &WindowsPty) -> Result<TerminalSize> {
    let size = pty
        .size
        .lock()
        .map_err(|_| io::Error::other("ConPTY size mutex poisoned"))?;
    Ok(*size)
}

/// How many resize calls the CQUIT1-R7 trace reports before it goes quiet.
const RESIZE_TRACE_LIMIT: u64 = 32;
static RESIZE_TRACE_CALLS: AtomicU64 = AtomicU64::new(0);

/// CQUIT1-R7 diagnostic: whether this process was asked to report its resizes.
///
/// Read once. Without `RMUX_CQUIT1_R7_TRACE=1` nothing below runs at all, so a
/// server that was not asked to explain itself behaves exactly as before.
fn resize_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("RMUX_CQUIT1_R7_TRACE").is_ok_and(|value| value == "1"))
}

/// Writes one CQUIT1-R7 record. It cannot panic and it returns nothing: a
/// diagnostic must not decide anything the resize does.
fn resize_trace(line: &str) {
    use std::io::Write as _;

    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "CQUIT1-R7 {line}");
    let _ = out.flush();
}

pub(crate) fn apply_size(pty: &WindowsPty, size: TerminalSize) -> Result<()> {
    tracing::trace!(
        target: "rmux::conpty",
        cols = size.cols,
        rows = size.rows,
        "resizing ConPTY"
    );
    let coord = coord_from_size(size)?;
    // CQUIT1-R7: what the endpoint already holds, read before the state lock so
    // this diagnostic can never invert the order the resize itself uses.
    let call = if resize_trace_enabled() {
        RESIZE_TRACE_CALLS.fetch_add(1, Ordering::Relaxed) + 1
    } else {
        0
    };
    let traced = call > 0 && call <= RESIZE_TRACE_LIMIT;
    let stored = traced
        .then(|| {
            pty.size
                .lock()
                .ok()
                .map(|stored| (stored.cols, stored.rows))
        })
        .flatten();
    let state = pty
        .state
        .read()
        .map_err(|_| io::Error::other("ConPTY state lock poisoned"))?;
    if traced {
        resize_trace(&format!(
            "resize call={call} enter hpcon={:#x} api={} resize_fn={:#x} requested={}x{} \
             stored={stored:?} identical={}",
            state.hpc.raw(),
            if state.hpc.api.companion {
                "companion"
            } else {
                "inbox"
            },
            state.hpc.api.resize as usize,
            size.cols,
            size.rows,
            stored == Some((size.cols, size.rows)),
        ));
    }
    // SAFETY: `state.hpc` is an owned live ConPTY handle while the read lock is
    // held, and `coord` was range-checked from `TerminalSize`.
    let hr = unsafe { (state.hpc.api.resize)(state.hpc.raw(), coord) };
    drop(state);
    if traced {
        resize_trace(&format!("resize call={call} exit hresult={hr:#010x}"));
    }
    if hr != S_OK {
        if is_benign_resize_after_exit(hr) {
            tracing::debug!(
                target: "rmux::conpty",
                hresult = hr,
                "ignoring ConPTY resize after child exit"
            );
            return Ok(());
        }
        tracing::warn!(
            target: "rmux::conpty",
            hresult = hr,
            "ConPTY resize failed"
        );
        return Err(hresult_error(hr).into());
    }

    let mut stored = pty
        .size
        .lock()
        .map_err(|_| io::Error::other("ConPTY size mutex poisoned"))?;
    *stored = size;
    Ok(())
}

pub(crate) fn apply_geometry(pty: &WindowsPty, geometry: TerminalGeometry) -> Result<()> {
    apply_size(pty, geometry.size)
}

fn create_pty_state_with_fallback(
    size: TerminalSize,
    selected: &'static ConptyApi,
    flags: ConptyFlags,
) -> Result<WindowsPtyState> {
    match create_pty_state(size, selected, flags) {
        Err(error) if selected.companion => {
            tracing::warn!(target: "rmux::conpty", %error, "companion ConPTY creation failed; retrying system console");
            create_pty_state_with_fallback(size, &api::INBOX, flags)
        }
        Err(_) if flags.uses_passthrough() => {
            create_pty_state_with_fallback(size, selected, conpty_flags_without_passthrough())
        }
        Err(_) if flags.bits() != 0 => create_pty_state(size, selected, standard_conpty_flags()),
        other => other,
    }
}

fn create_pty_state(
    size: TerminalSize,
    api: &'static ConptyApi,
    flags: ConptyFlags,
) -> Result<WindowsPtyState> {
    let (input, output) = {
        let _span = perf::span("conpty_create_pipes");
        (
            create_conpty_input_pipe(64 * 1024)?,
            create_pipe(64 * 1024)?,
        )
    };
    let hpc = {
        let _span = perf::span("conpty_create_pseudoconsole");
        create_pseudo_console(size, &input, &output, api, flags)?
    };
    drop(input.read);
    drop(output.write);
    let input_writer = Arc::new(OverlappedPipeWriter::new(input.write)?);
    Ok(WindowsPtyState {
        hpc,
        input_writer,
        output_read: output.read,
        startup_output: VecDeque::new(),
        flags,
    })
}

fn create_pseudo_console(
    size: TerminalSize,
    input: &PipePair,
    output: &PipePair,
    api: &'static ConptyApi,
    flags: ConptyFlags,
) -> Result<OwnedHpcon> {
    let mut hpc = 0_isize;
    let coord = coord_from_size(size)?;
    // SAFETY: `input` and `output` are valid pipe handles owned by the caller,
    // `hpc` is a valid out-pointer, and `coord_from_size` range-checks the
    // dimensions before the API call.
    let hr = unsafe {
        (api.create)(
            coord,
            input.read.as_raw_handle() as HANDLE,
            output.write.as_raw_handle() as HANDLE,
            if api.companion { 0 } else { flags.bits() },
            &mut hpc,
        )
    };
    if hr != S_OK {
        tracing::warn!(
            target: "rmux::conpty",
            hresult = hr,
            companion = api.companion,
            "CreatePseudoConsole failed"
        );
        return Err(hresult_error(hr).into());
    }
    Ok(OwnedHpcon { handle: hpc, api })
}

fn coord_from_size(size: TerminalSize) -> Result<COORD> {
    if size.cols == 0 || size.rows == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "terminal size must be at least 1x1",
        )
        .into());
    }
    let cols = i16::try_from(size.cols).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "terminal column count exceeds Windows COORD range",
        )
    })?;
    let rows = i16::try_from(size.rows).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "terminal row count exceeds Windows COORD range",
        )
    })?;
    Ok(COORD { X: cols, Y: rows })
}

#[derive(Debug)]
struct WindowsPtyState {
    hpc: OwnedHpcon,
    input_writer: Arc<OverlappedPipeWriter>,
    output_read: OwnedHandle,
    startup_output: VecDeque<u8>,
    flags: ConptyFlags,
}

#[derive(Debug)]
struct OwnedHpcon {
    handle: HPCON,
    api: &'static ConptyApi,
}

impl OwnedHpcon {
    fn raw(&self) -> HPCON {
        self.handle
    }

    fn close(&mut self) {
        if self.handle != 0 {
            tracing::trace!(target: "rmux::conpty", "closing ConPTY");
            // SAFETY: `OwnedHpcon` owns a non-null ConPTY handle and closes it
            // exactly once here or from `Drop`.
            unsafe { (self.api.close)(self.handle) };
            self.handle = 0;
        }
    }
}

impl Drop for OwnedHpcon {
    fn drop(&mut self) {
        self.close();
    }
}

fn hresult_error(hr: i32) -> io::Error {
    io::Error::from_raw_os_error(hr)
}

fn is_benign_resize_after_exit(hr: i32) -> bool {
    hr == E_HANDLE
        || hr == hresult_from_win32(ERROR_BROKEN_PIPE)
        || hr == hresult_from_win32(ERROR_INVALID_PARAMETER)
}

fn hresult_from_win32(error: u32) -> i32 {
    if error == 0 {
        error as i32
    } else {
        ((error & 0x0000_FFFF) | 0x8007_0000) as i32
    }
}

#[allow(dead_code)]
fn last_os_error() -> io::Error {
    // SAFETY: `GetLastError` reads the calling thread's last-error slot and has
    // no preconditions.
    let code = unsafe { GetLastError() };
    io::Error::from_raw_os_error(code as i32)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::Duration;

    unsafe extern "system" fn reject_companion(
        _size: COORD,
        _input: HANDLE,
        _output: HANDLE,
        _flags: u32,
        _hpc: *mut HPCON,
    ) -> i32 {
        windows_sys::Win32::Foundation::E_INVALIDARG
    }

    #[test]
    fn failed_companion_creation_falls_back_to_inbox_api() {
        static REJECTED: ConptyApi = ConptyApi {
            create: reject_companion,
            resize: api::INBOX.resize,
            close: api::INBOX.close,
            companion: true,
        };
        let state = create_pty_state_with_fallback(
            TerminalSize::new(80, 24),
            &REJECTED,
            selected_conpty_flags(),
        )
        .expect("system fallback");
        assert!(!state.hpc.api.companion);
        // Exercise the public backend resize path using the API retained by
        // fallback, without depending on a recorder to observe its outcome.
        let pty = WindowsPty {
            state: RwLock::new(state),
            size: Mutex::new(TerminalSize::new(80, 24)),
            dsr_bootstrap: Mutex::new(None),
        };
        apply_size(&pty, TerminalSize::new(90, 25)).unwrap();
        assert_eq!(query_size(&pty).unwrap(), TerminalSize::new(90, 25));
    }

    #[test]
    fn rejected_creation_retries_flags_and_preserves_failure() {
        // Thread-local observations belong only to this synchronous test; they
        // cannot become a product control or race another native test.
        thread_local! {
            static FLAGS: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        unsafe extern "system" fn reject_and_record(
            _size: COORD,
            _input: HANDLE,
            _output: HANDLE,
            flags: u32,
            _hpc: *mut HPCON,
        ) -> i32 {
            FLAGS.with(|calls| calls.borrow_mut().push(flags));
            windows_sys::Win32::Foundation::E_INVALIDARG
        }
        static REJECTED: ConptyApi = ConptyApi {
            create: reject_and_record,
            resize: api::INBOX.resize,
            close: api::INBOX.close,
            companion: false,
        };
        let flags = selected_conpty_flags();
        assert!(
            create_pty_state_with_fallback(TerminalSize::new(80, 24), &REJECTED, flags).is_err()
        );
        FLAGS.with(|calls| {
            let calls = calls.borrow();
            assert_eq!(calls.len(), if flags.uses_passthrough() { 3 } else { 2 });
            assert_eq!(calls[0], flags.bits());
            assert_eq!(calls.last(), Some(&0));
        });
    }

    #[test]
    fn resize_after_exit_invalid_parameter_is_benign() {
        assert!(is_benign_resize_after_exit(hresult_from_win32(
            ERROR_INVALID_PARAMETER
        )));
    }

    #[test]
    fn coord_from_size_rejects_zero_dimensions() {
        let error = coord_from_size_error(TerminalSize { cols: 0, rows: 24 });
        assert!(error
            .to_string()
            .contains("terminal size must be at least 1x1"));

        let error = coord_from_size_error(TerminalSize { cols: 80, rows: 0 });
        assert!(error
            .to_string()
            .contains("terminal size must be at least 1x1"));
    }

    fn coord_from_size_error(size: TerminalSize) -> crate::PtyError {
        match coord_from_size(size) {
            Ok(_) => panic!("zero terminal dimensions should be rejected"),
            Err(error) => error,
        }
    }

    #[test]
    fn idle_blocking_read_does_not_block_conpty_recreate() {
        let pty = Arc::new(open_pty_pair(TerminalSize { cols: 80, rows: 24 }).expect("pty"));
        let (read_started_tx, read_started_rx) = mpsc::channel();
        let (read_done_tx, read_done_rx) = mpsc::channel();
        let reader_pty = Arc::clone(&pty);
        let reader = thread::spawn(move || {
            let mut buffer = [0_u8; 64];
            let _ = read_started_tx.send(());
            let result = reader_pty.read(&mut buffer).map(|_| ());
            let _ = read_done_tx.send(result);
        });

        read_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("reader thread started");
        thread::sleep(Duration::from_millis(50));
        assert!(
            matches!(read_done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "reader must still be blocked before recreation"
        );

        let (recreate_tx, recreate_rx) = mpsc::channel();
        let recreate_pty = Arc::clone(&pty);
        let recreate = thread::spawn(move || {
            let result = recreate_pty
                .recreate_without_passthrough()
                .map_err(|error| error.to_string());
            let _ = recreate_tx.send(result);
        });

        recreate_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("ConPTY recreate should not wait for idle output")
            .expect("ConPTY recreate");
        recreate.join().expect("recreate thread");
        assert!(!pty.state.read().expect("state").hpc.api.companion);
        assert!(!pty.uses_passthrough());
        drop(pty);

        let _ = read_done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("old output read should close after recreate");
        reader.join().expect("reader thread");
    }
}
