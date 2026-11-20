//! macOS libproc and sysctl process inspection.

use std::collections::HashMap;
use std::ffi::{CStr, OsString};
use std::io;
use std::path::PathBuf;

use super::{
    environment_from_nul_entries, raw_environment_from_nul_entries, window_name_from_process_name,
};

pub(super) fn current_path_impl(pid: u32) -> io::Result<Option<String>> {
    let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>();
    let Some(pid) = pid.try_into().ok() else {
        return Ok(None);
    };
    let Some(size_i32) = size.try_into().ok() else {
        return Ok(None);
    };
    let read = unsafe {
        // SAFETY: `info` points to writable memory sized for the requested flavor.
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            size_i32,
        )
    };
    if usize::try_from(read)
        .map(|read| read < size)
        .unwrap_or(true)
    {
        return Ok(None);
    }

    let info = unsafe {
        // SAFETY: `proc_pidinfo` reported that it initialized the full structure.
        info.assume_init()
    };
    Ok(string_from_c_chars(info.pvi_cdir.vip_path.as_ptr().cast()))
}

pub(super) fn command_name_impl(pid: u32) -> io::Result<Option<String>> {
    Ok(command_name_from_macos_pidpath(pid).or_else(|| command_name_from_macos_proc_name(pid)))
}

pub(super) fn executable_path_impl(pid: u32) -> io::Result<Option<String>> {
    Ok(macos_pidpath(pid))
}

/// tmux takes the executable's own name (`pbsi_comm`), never argv[0], then
/// parses it as a window name. The full path's file name is that name without
/// the 15-byte `MAXCOMLEN` truncation.
fn command_name_from_macos_pidpath(pid: u32) -> Option<String> {
    let path = macos_pidpath(pid)?;
    let file_name = std::path::Path::new(&path).file_name()?.to_str()?;
    window_name_from_process_name(file_name)
}

fn macos_pidpath(pid: u32) -> Option<String> {
    let mut buffer = [0 as libc::c_char; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let written = unsafe {
        // SAFETY: `buffer` is writable for the size passed to `proc_pidpath`.
        libc::proc_pidpath(
            pid.try_into().ok()?,
            buffer.as_mut_ptr().cast(),
            buffer.len().try_into().ok()?,
        )
    };
    if written <= 0 {
        return None;
    }
    string_from_c_chars(buffer.as_ptr())
}

fn command_name_from_macos_proc_name(pid: u32) -> Option<String> {
    let mut buffer = [0 as libc::c_char; 1024];
    let written = unsafe {
        // SAFETY: `buffer` is writable for the size passed to `proc_name`.
        libc::proc_name(
            pid.try_into().ok()?,
            buffer.as_mut_ptr().cast(),
            buffer.len().try_into().ok()?,
        )
    };
    if written <= 0 {
        return None;
    }
    string_from_c_chars(buffer.as_ptr()).and_then(|name| window_name_from_process_name(&name))
}

pub(super) fn fd_path_impl(pid: u32, fd: i32) -> io::Result<Option<PathBuf>> {
    let mut info = std::mem::MaybeUninit::<MacosVnodeFdInfoWithPath>::zeroed();
    let size = std::mem::size_of::<MacosVnodeFdInfoWithPath>();
    let Some(pid) = pid.try_into().ok() else {
        return Ok(None);
    };
    let Some(size_i32) = size.try_into().ok() else {
        return Ok(None);
    };
    let read = unsafe {
        // SAFETY: `info` points to writable memory sized for the requested flavor.
        libc::proc_pidfdinfo(
            pid,
            fd,
            MACOS_PROC_PIDFDVNODEPATHINFO,
            info.as_mut_ptr().cast(),
            size_i32,
        )
    };
    if usize::try_from(read)
        .map(|read| read < size)
        .unwrap_or(true)
    {
        return Ok(None);
    }

    let info = unsafe {
        // SAFETY: `proc_pidfdinfo` reported that it initialized the full structure.
        info.assume_init()
    };
    Ok(string_from_c_chars(info.pvip.vip_path.as_ptr().cast()).map(PathBuf::from))
}

pub(super) fn is_live_impl(pid: u32) -> io::Result<Option<bool>> {
    let Some(pid) = libc::c_int::try_from(pid).ok() else {
        return Ok(None);
    };
    let Some(size) = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok() else {
        return Ok(None);
    };
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let read = unsafe {
        // SAFETY: `info` points to writable memory sized for the requested flavor.
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read < size {
        return Ok(Some(false));
    }

    let info = unsafe {
        // SAFETY: `proc_pidinfo` reported that it initialized the full structure.
        info.assume_init()
    };
    Ok(Some(info.pbi_status != libc::SZOMB))
}

pub(super) fn environment_impl(pid: u32) -> io::Result<Option<HashMap<String, String>>> {
    Ok(macos_procargs(pid).and_then(|buffer| environment_from_macos_procargs(&buffer)))
}

pub(super) fn raw_environment_impl(pid: u32) -> io::Result<Option<Vec<(OsString, OsString)>>> {
    Ok(macos_procargs(pid).and_then(|buffer| raw_environment_from_macos_procargs(&buffer)))
}

const MACOS_PROC_PIDFDVNODEPATHINFO: libc::c_int = 2;

#[repr(C)]
struct MacosProcFileInfo {
    fi_openflags: u32,
    fi_status: u32,
    fi_offset: libc::off_t,
    fi_type: i32,
    fi_guardflags: u32,
}

#[repr(C)]
struct MacosVnodeFdInfoWithPath {
    pfi: MacosProcFileInfo,
    pvip: libc::vnode_info_path,
}

fn string_from_c_chars(chars: *const libc::c_char) -> Option<String> {
    let value = unsafe {
        // SAFETY: macOS libproc path/name buffers are nul-terminated on success.
        CStr::from_ptr(chars)
    }
    .to_string_lossy()
    .into_owned();
    (!value.is_empty()).then_some(value)
}

fn macos_procargs(pid: u32) -> Option<Vec<u8>> {
    let pid = libc::c_int::try_from(pid).ok()?;
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mib_len = u32::try_from(mib.len()).ok()?;
    let mut size = 0;
    let result = unsafe {
        // SAFETY: The first sysctl call asks only for the required buffer size.
        libc::sysctl(
            mib.as_mut_ptr(),
            mib_len,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || size == 0 {
        return None;
    }

    let mut buffer = vec![0; size];
    let result = unsafe {
        // SAFETY: `buffer` is writable for `size` bytes reported by sysctl.
        libc::sysctl(
            mib.as_mut_ptr(),
            mib_len,
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || size == 0 {
        return None;
    }
    buffer.truncate(size);
    Some(buffer)
}

pub(super) fn environment_from_macos_procargs(buffer: &[u8]) -> Option<HashMap<String, String>> {
    let offset = macos_environment_offset(buffer)?;
    environment_from_nul_entries(&buffer[offset..])
}

fn raw_environment_from_macos_procargs(buffer: &[u8]) -> Option<Vec<(OsString, OsString)>> {
    let offset = macos_environment_offset(buffer)?;
    Some(raw_environment_from_nul_entries(&buffer[offset..]))
}

fn macos_environment_offset(buffer: &[u8]) -> Option<usize> {
    let argc_size = std::mem::size_of::<libc::c_int>();
    if buffer.len() < argc_size {
        return None;
    }
    let mut argc_bytes = [0; std::mem::size_of::<libc::c_int>()];
    argc_bytes.copy_from_slice(&buffer[..argc_size]);
    let argc = libc::c_int::from_ne_bytes(argc_bytes);
    if argc < 0 {
        return None;
    }

    let mut offset = skip_nul_terminated(buffer, argc_size)?;
    offset = skip_nul_padding(buffer, offset);
    for _ in 0..argc {
        offset = skip_nul_terminated(buffer, offset)?;
    }
    Some(skip_nul_padding(buffer, offset))
}

fn skip_nul_terminated(buffer: &[u8], offset: usize) -> Option<usize> {
    let relative_end = buffer.get(offset..)?.iter().position(|byte| *byte == 0)?;
    Some(offset + relative_end + 1)
}

fn skip_nul_padding(buffer: &[u8], mut offset: usize) -> usize {
    while buffer.get(offset).is_some_and(|byte| *byte == 0) {
        offset += 1;
    }
    offset
}
