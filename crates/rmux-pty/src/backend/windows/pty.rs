use std::io;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::sync::Arc;
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_BROKEN_PIPE, ERROR_INVALID_PARAMETER, E_HANDLE, HANDLE, S_OK,
};
use windows_sys::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};

use crate::{Result, TerminalGeometry, TerminalSize};

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
            let bytes_read = super::io::read(&output_read, buffer)?;
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

    pub(crate) fn uses_passthrough(&self) -> bool {
        self.state
            .read()
            .map(|state| state.flags.uses_passthrough())
            .unwrap_or(false)
    }

    pub(crate) fn recreate_without_passthrough(&self) -> Result<()> {
        let size = *self
            .size
            .lock()
            .map_err(|_| io::Error::other("ConPTY size mutex poisoned"))?;
        let state = create_pty_state(size, conpty_flags_without_passthrough())?;
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
    let state = create_pty_state_with_fallback(size, selected_conpty_flags())?;
    tracing::debug!(
        target: "rmux::conpty",
        cols = size.cols,
        rows = size.rows,
        flags = state.flags.bits(),
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

pub(crate) fn apply_size(pty: &WindowsPty, size: TerminalSize) -> Result<()> {
    tracing::trace!(
        target: "rmux::conpty",
        cols = size.cols,
        rows = size.rows,
        "resizing ConPTY"
    );
    let coord = coord_from_size(size)?;
    let state = pty
        .state
        .read()
        .map_err(|_| io::Error::other("ConPTY state lock poisoned"))?;
    // SAFETY: `state.hpc` is an owned live ConPTY handle while the read lock is
    // held, and `coord` was range-checked from `TerminalSize`.
    let hr = unsafe { ResizePseudoConsole(state.hpc.raw(), coord) };
    drop(state);
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
    selected: ConptyFlags,
) -> Result<WindowsPtyState> {
    match create_pty_state(size, selected) {
        Ok(state) => Ok(state),
        Err(error) if selected.uses_passthrough() => {
            tracing::warn!(
                target: "rmux::conpty",
                flags = selected.bits(),
                "CreatePseudoConsole with passthrough failed; retrying without passthrough: {error}"
            );
            create_pty_state_with_fallback(size, conpty_flags_without_passthrough())
        }
        Err(error) if selected.bits() != 0 => {
            tracing::warn!(
                target: "rmux::conpty",
                flags = selected.bits(),
                "CreatePseudoConsole with extended flags failed; retrying standard flags: {error}"
            );
            create_pty_state(size, standard_conpty_flags())
        }
        Err(error) => Err(error),
    }
}

fn create_pty_state(size: TerminalSize, flags: ConptyFlags) -> Result<WindowsPtyState> {
    let (input, output) = {
        let _span = perf::span("conpty_create_pipes");
        (
            create_conpty_input_pipe(64 * 1024)?,
            create_pipe(64 * 1024)?,
        )
    };
    let hpc = {
        let _span = perf::span("conpty_create_pseudoconsole");
        create_pseudo_console(size, &input, &output, flags)?
    };
    drop(input.read);
    drop(output.write);
    let input_writer = Arc::new(OverlappedPipeWriter::new(input.write)?);
    Ok(WindowsPtyState {
        hpc,
        input_writer,
        output_read: output.read,
        flags,
    })
}

fn create_pseudo_console(
    size: TerminalSize,
    input: &PipePair,
    output: &PipePair,
    flags: ConptyFlags,
) -> Result<OwnedHpcon> {
    let mut hpc = 0_isize;
    // SAFETY: `input` and `output` are valid pipe handles owned by the caller,
    // `hpc` is a valid out-pointer, and `coord_from_size` range-checks the
    // dimensions before the API call.
    let hr = unsafe {
        CreatePseudoConsole(
            coord_from_size(size)?,
            input.read.as_raw_handle() as HANDLE,
            output.write.as_raw_handle() as HANDLE,
            flags.bits(),
            &mut hpc,
        )
    };
    if hr != S_OK {
        tracing::warn!(
            target: "rmux::conpty",
            hresult = hr,
            flags = flags.bits(),
            "CreatePseudoConsole failed"
        );
        return Err(hresult_error(hr).into());
    }
    Ok(OwnedHpcon(hpc))
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
    flags: ConptyFlags,
}

#[derive(Debug)]
struct OwnedHpcon(HPCON);

impl OwnedHpcon {
    fn raw(&self) -> HPCON {
        self.0
    }

    fn close(&mut self) {
        if self.0 != 0 {
            tracing::trace!(target: "rmux::conpty", "closing ConPTY");
            // SAFETY: `OwnedHpcon` owns a non-null ConPTY handle and closes it
            // exactly once here or from `Drop`.
            unsafe { ClosePseudoConsole(self.0) };
            self.0 = 0;
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
        drop(pty);

        let _ = read_done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("old output read should close after recreate");
        reader.join().expect("reader thread");
    }
}
