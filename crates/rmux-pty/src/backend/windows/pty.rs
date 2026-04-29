use std::io;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::sync::{Mutex, RwLock};

use windows_sys::Win32::Foundation::{GetLastError, ERROR_BROKEN_PIPE, E_HANDLE, HANDLE, S_OK};
use windows_sys::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};

use crate::{Result, TerminalSize};

use super::flags::{
    conpty_flags_without_passthrough, selected_conpty_flags, standard_conpty_flags, ConptyFlags,
};
use super::io::{create_pipe, PipePair};
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

    pub(crate) fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            if let Some(len) = self.drain_deferred_dsr_bytes(buffer)? {
                return Ok(len);
            }

            let state = self
                .state
                .read()
                .map_err(|_| io::Error::other("ConPTY state lock poisoned"))?;
            let bytes_read = super::io::read(&state.output_read, buffer)?;
            drop(state);
            let filtered = {
                let mut dsr_bootstrap = self
                    .dsr_bootstrap
                    .lock()
                    .map_err(|_| io::Error::other("ConPTY DSR mutex poisoned"))?;
                let Some(dsr) = dsr_bootstrap.as_mut() else {
                    return Ok(bytes_read);
                };

                let filtered = dsr.filter(buffer, bytes_read);
                if filtered.response.is_some() {
                    *dsr_bootstrap = None;
                }
                filtered
            };
            if let Some(response) = filtered.response {
                self.write_all(response)?;
            }
            if filtered.len > 0 || bytes_read == 0 {
                return Ok(filtered.len);
            }
        }
    }

    pub(crate) fn write_all(&self, bytes: &[u8]) -> io::Result<()> {
        let state = self
            .state
            .read()
            .map_err(|_| io::Error::other("ConPTY state lock poisoned"))?;
        super::io::write_all(&state.input_write, bytes)
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
        Ok(dsr_bootstrap
            .as_mut()
            .and_then(|dsr| dsr.drain_deferred(buffer)))
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
    let input = create_pipe(64 * 1024)?;
    let output = create_pipe(64 * 1024)?;
    let hpc = create_pseudo_console(size, &input, &output, flags)?;
    drop(input.read);
    drop(output.write);
    Ok(WindowsPtyState {
        hpc,
        input_write: input.write,
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
    input_write: OwnedHandle,
    output_read: OwnedHandle,
    flags: ConptyFlags,
}

#[derive(Debug)]
struct OwnedHpcon(HPCON);

impl OwnedHpcon {
    fn raw(&self) -> HPCON {
        self.0
    }
}

impl Drop for OwnedHpcon {
    fn drop(&mut self) {
        if self.0 != 0 {
            tracing::trace!(target: "rmux::conpty", "closing ConPTY");
            // SAFETY: `OwnedHpcon` owns a non-null ConPTY handle and closes it
            // exactly once from `Drop`.
            unsafe { ClosePseudoConsole(self.0) };
        }
    }
}

fn hresult_error(hr: i32) -> io::Error {
    io::Error::from_raw_os_error(hr)
}

fn is_benign_resize_after_exit(hr: i32) -> bool {
    hr == E_HANDLE || hr == hresult_from_win32(ERROR_BROKEN_PIPE)
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
