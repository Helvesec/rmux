use std::io;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{GetLastError, E_HANDLE, ERROR_BROKEN_PIPE, HANDLE, S_OK};
use windows_sys::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};

use crate::{Result, TerminalSize};

use super::io::create_pipe;

#[derive(Debug)]
pub(crate) struct WindowsPty {
    hpc: OwnedHpcon,
    input_write: OwnedHandle,
    output_read: OwnedHandle,
    size: Mutex<TerminalSize>,
}

impl WindowsPty {
    pub(crate) fn hpc(&self) -> HPCON {
        self.hpc.raw()
    }

    pub(crate) fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        super::io::read(&self.output_read, buffer)
    }

    pub(crate) fn write_all(&self, bytes: &[u8]) -> io::Result<()> {
        super::io::write_all(&self.input_write, bytes)
    }
}

pub(crate) fn open_pty_pair(size: TerminalSize) -> Result<WindowsPty> {
    let input = create_pipe(64 * 1024)?;
    let output = create_pipe(64 * 1024)?;
    let hpc = create_pseudo_console(
        size,
        input.read.as_raw_handle() as HANDLE,
        output.write.as_raw_handle() as HANDLE,
    )?;
    drop(input.read);
    drop(output.write);

    Ok(WindowsPty {
        hpc,
        input_write: input.write,
        output_read: output.read,
        size: Mutex::new(size),
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
    let coord = coord_from_size(size)?;
    let hr = unsafe { ResizePseudoConsole(pty.hpc.raw(), coord) };
    if hr != S_OK {
        if is_benign_resize_after_exit(hr) {
            return Ok(());
        }
        return Err(hresult_error(hr).into());
    }

    let mut stored = pty
        .size
        .lock()
        .map_err(|_| io::Error::other("ConPTY size mutex poisoned"))?;
    *stored = size;
    Ok(())
}

fn create_pseudo_console(size: TerminalSize, input: HANDLE, output: HANDLE) -> Result<OwnedHpcon> {
    let mut hpc = 0_isize;
    let hr = unsafe { CreatePseudoConsole(coord_from_size(size)?, input, output, 0, &mut hpc) };
    if hr != S_OK {
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
struct OwnedHpcon(HPCON);

impl OwnedHpcon {
    fn raw(&self) -> HPCON {
        self.0
    }
}

impl Drop for OwnedHpcon {
    fn drop(&mut self) {
        if self.0 != 0 {
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
    let code = unsafe { GetLastError() };
    io::Error::from_raw_os_error(code as i32)
}
