//! Windows terminal-handle classification shared by public binary variants.

/// Returns whether a Windows standard-stream handle belongs to an MSYS/Cygwin
/// pseudo-terminal rather than an ordinary anonymous pipe.
pub(crate) fn handle_is_msys_pty(handle: std::os::windows::io::RawHandle) -> bool {
    use windows_sys::Win32::Storage::FileSystem::{FileNameInfo, GetFileInformationByHandleEx};

    // FILE_NAME_INFO: a u32 byte length followed by the UTF-16 name.
    let mut buffer = [0_u8; 1024];
    // SAFETY: `handle` is borrowed from a standard stream for this synchronous
    // query. `buffer` is initialized and writable for the exact byte count passed;
    // a failed or unsupported query returns zero and is handled before parsing.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileNameInfo,
            buffer.as_mut_ptr().cast(),
            u32::try_from(buffer.len()).expect("buffer length fits in u32"),
        )
    };
    if ok == 0 {
        return false;
    }
    let name_bytes = u32::from_ne_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
    let name_units = (name_bytes / 2).min((buffer.len() - 4) / 2);
    let name = String::from_utf16_lossy(
        &buffer[4..4 + name_units * 2]
            .chunks_exact(2)
            .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>(),
    )
    .to_ascii_lowercase();
    is_msys_pty_pipe_name(&name)
}

/// Classifies the normalized kernel pipe name used by MSYS2 and Cygwin ptys.
pub(crate) fn is_msys_pty_pipe_name(name: &str) -> bool {
    (name.contains("msys-") || name.contains("cygwin-")) && name.contains("-pty")
}

#[cfg(test)]
mod tests {
    #[test]
    fn recognizes_only_msys_and_cygwin_pty_pipe_names() {
        for name in [
            r"\msys-dd50a72ab4668b33-pty0-from-master",
            r"\msys-dd50a72ab4668b33-pty1-to-master",
            r"\cygwin-e022582115c10879-pty4-from-master",
        ] {
            assert!(super::is_msys_pty_pipe_name(&name.to_ascii_lowercase()));
        }
        for name in [
            r"\pipe\rmux-daemon",
            r"\msys-dd50a72ab4668b33-shared",
            r"\some-pty-alike",
            "",
        ] {
            assert!(!super::is_msys_pty_pipe_name(&name.to_ascii_lowercase()));
        }
    }
}
