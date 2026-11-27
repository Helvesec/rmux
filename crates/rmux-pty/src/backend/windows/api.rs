use std::env;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::null_mut;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{FreeLibrary, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO,
};
use windows_sys::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

type Create = unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> i32;
type Resize = unsafe extern "system" fn(HPCON, COORD) -> i32;
type Close = unsafe extern "system" fn(HPCON);

/// Each HPCON retains the API which created it, including across fallback.
#[derive(Debug)]
pub(super) struct ConptyApi {
    pub(super) create: Create,
    pub(super) resize: Resize,
    pub(super) close: Close,
    pub(super) companion: bool,
}

pub(super) static INBOX: ConptyApi = ConptyApi {
    create: CreatePseudoConsole,
    resize: ResizePseudoConsole,
    close: ClosePseudoConsole,
    companion: false,
};

pub(super) fn selected() -> &'static ConptyApi {
    static API: OnceLock<ConptyApi> = OnceLock::new();
    API.get_or_init(|| {
        if !env::var("RMUX_CONPTY_NO_PASSTHROUGH")
            .is_ok_and(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        {
            match load_companion() {
                Ok(Some(api)) => return api,
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(target: "rmux::conpty", %error, "companion ConPTY unavailable; using system console");
                }
            }
        }
        ConptyApi { ..INBOX }
    })
}

fn load_companion() -> io::Result<Option<ConptyApi>> {
    // Never search PATH, the working directory, or a Windows Terminal install.
    let executable = env::current_exe()?;
    let directory = executable
        .parent()
        .ok_or_else(|| io::Error::other("executable directory"))?;
    let dll = directory.join("conpty.dll");
    let host = directory.join("OpenConsole.exe");
    if !dll.try_exists()? && !host.try_exists()? {
        return Ok(None);
    }
    let dll_version = file_version(&dll)?;
    let host_version = file_version(&host)?;
    // The baseline is the Microsoft.Windows.Console.ConPTY 1.24.260710001
    // runtime measured for OSC requests/replies, keyboard, resize and teardown.
    // OS build numbers and CreatePseudoConsole's glyph-width bits do not
    // establish either input or output passthrough capability.
    if dll_version != host_version || dll_version < [1, 24, 2607, 10001] {
        return Err(io::Error::other(
            "ConPTY requires a matching runtime pair, version 1.24.2607.10001 or later",
        ));
    }
    let wide: Vec<u16> = dll.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: an absolute NUL-terminated application-local path; dependencies
    // are restricted to that directory and System32.
    let module = unsafe {
        LoadLibraryExW(
            wide.as_ptr(),
            null_mut(),
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    if module.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: module is live, names are NUL-terminated official ConPTY exports.
    let exports = unsafe {
        (
            GetProcAddress(module, c"ConptyCreatePseudoConsole".as_ptr().cast()),
            GetProcAddress(module, c"ConptyResizePseudoConsole".as_ptr().cast()),
            GetProcAddress(module, c"ConptyClosePseudoConsole".as_ptr().cast()),
        )
    };
    let (Some(create), Some(resize), Some(close)) = exports else {
        // SAFETY: no function pointer has escaped; release our load reference.
        unsafe { FreeLibrary(module) };
        return Err(io::Error::other("incomplete ConPTY runtime exports"));
    };
    // Intentionally retain the load reference for the process lifetime: API is
    // stored in OnceLock and every live console may call it during teardown.
    tracing::debug!(target: "rmux::conpty", version = ?dll_version, "using companion ConPTY");
    // SAFETY: the three exports use the published ConPTY C ABI in inc/conpty.h.
    Ok(Some(unsafe {
        ConptyApi {
            create: std::mem::transmute::<unsafe extern "system" fn() -> isize, Create>(create),
            resize: std::mem::transmute::<unsafe extern "system" fn() -> isize, Resize>(resize),
            close: std::mem::transmute::<unsafe extern "system" fn() -> isize, Close>(close),
            companion: true,
        }
    }))
}

fn file_version(path: &Path) -> io::Result<[u16; 4]> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: path is NUL-terminated; the unused handle output is optional.
    let size = unsafe { GetFileVersionInfoSizeW(wide.as_ptr(), null_mut()) };
    if size == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut data = vec![0_u8; size as usize];
    let mut value = null_mut();
    let mut len = 0;
    // SAFETY: data has the reported capacity; query returns storage within data.
    let ok = unsafe {
        GetFileVersionInfoW(wide.as_ptr(), 0, size, data.as_mut_ptr().cast()) != 0
            && VerQueryValueW(
                data.as_ptr().cast(),
                [b'\\' as u16, 0].as_ptr(),
                &mut value,
                &mut len,
            ) != 0
    };
    if !ok || value.is_null() || (len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>() {
        return Err(io::Error::other("invalid ConPTY file version resource"));
    }
    // SAFETY: query returned at least one fixed-info structure; unaligned read
    // does not require the Vec<u8> allocation to have structure alignment.
    let info = unsafe { value.cast::<VS_FIXEDFILEINFO>().read_unaligned() };
    if info.dwSignature != 0xfeef04bd {
        return Err(io::Error::other("invalid ConPTY file version signature"));
    }
    Ok([
        (info.dwFileVersionMS >> 16) as u16,
        info.dwFileVersionMS as u16,
        (info.dwFileVersionLS >> 16) as u16,
        info.dwFileVersionLS as u16,
    ])
}
