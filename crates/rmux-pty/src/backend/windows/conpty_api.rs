use std::env;
use std::ffi::c_void;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{HANDLE, HMODULE};
use windows_sys::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

// The ConPTY ABI is identical between kernel32.dll and a terminal-project
// conpty.dll (OpenConsole backend): same three exports, same signatures.
type CreateFn = unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> i32;
type ResizeFn = unsafe extern "system" fn(HPCON, COORD) -> i32;
type CloseFn = unsafe extern "system" fn(HPCON);

pub(super) const CONPTY_DLL_ENV: &str = "RMUX_CONPTY_DLL";
const CONPTY_HELPER_BINARY: &str = "OpenConsole.exe";
const CREATE_EXPORT: &str = "CreatePseudoConsole";
const RESIZE_EXPORT: &str = "ResizePseudoConsole";
const CLOSE_EXPORT: &str = "ClosePseudoConsole";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ConptyApiSource {
    Kernel32,
    ConptyDll,
}

impl ConptyApiSource {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Kernel32 => "kernel32",
            Self::ConptyDll => "conpty.dll",
        }
    }

    pub(super) const fn is_dll(self) -> bool {
        matches!(self, Self::ConptyDll)
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ConptyApi {
    create: CreateFn,
    resize: ResizeFn,
    close: CloseFn,
    pub(super) source: ConptyApiSource,
}

impl ConptyApi {
    // SAFETY: the caller owns the pipe handles and the out pointer for the
    // lifetime of the call, mirroring the `CreatePseudoConsole` contract.
    pub(super) unsafe fn create_pseudo_console(
        &self,
        size: COORD,
        input: HANDLE,
        output: HANDLE,
        flags: u32,
        pseudoconsole: *mut HPCON,
    ) -> i32 {
        unsafe { (self.create)(size, input, output, flags, pseudoconsole) }
    }

    // SAFETY: `pseudoconsole` is a live handle owned by the caller.
    pub(super) unsafe fn resize_pseudo_console(&self, pseudoconsole: HPCON, size: COORD) -> i32 {
        unsafe { (self.resize)(pseudoconsole, size) }
    }

    // SAFETY: `pseudoconsole` is owned and closed exactly once by the caller.
    pub(super) unsafe fn close_pseudo_console(&self, pseudoconsole: HPCON) {
        unsafe { (self.close)(pseudoconsole) }
    }
}

pub(super) fn kernel32_conpty_api() -> ConptyApi {
    ConptyApi {
        create: CreatePseudoConsole,
        resize: ResizePseudoConsole,
        close: ClosePseudoConsole,
        source: ConptyApiSource::Kernel32,
    }
}

pub(super) fn selected_conpty_api() -> ConptyApi {
    static SELECTED: OnceLock<ConptyApi> = OnceLock::new();
    *SELECTED.get_or_init(resolve_selected_conpty_api)
}

fn resolve_selected_conpty_api() -> ConptyApi {
    let requested = env::var(CONPTY_DLL_ENV).ok();
    match resolve_requested_conpty_api(requested.as_deref()) {
        Ok(api) if api.source.is_dll() => {
            tracing::info!(
                target: "rmux::conpty",
                env = CONPTY_DLL_ENV,
                "ConPTY backend selected: conpty.dll (OpenConsole)"
            );
            api
        }
        Ok(api) => api,
        Err(error) => {
            tracing::warn!(
                target: "rmux::conpty",
                env = CONPTY_DLL_ENV,
                "conpty.dll backend unavailable, using kernel32 ConPTY: {error}"
            );
            kernel32_conpty_api()
        }
    }
}

fn resolve_requested_conpty_api(requested: Option<&str>) -> io::Result<ConptyApi> {
    let Some(path) = requested.filter(|value| !value.trim().is_empty()) else {
        return Ok(kernel32_conpty_api());
    };
    load_conpty_api(Path::new(path))
}

pub(super) fn load_conpty_api(dll_path: &Path) -> io::Result<ConptyApi> {
    validate_conpty_dll_layout(dll_path)?;
    let module = load_conpty_module(dll_path)?;
    let create = resolve_export(module, CREATE_EXPORT)?;
    let resize = resolve_export(module, RESIZE_EXPORT)?;
    let close = resolve_export(module, CLOSE_EXPORT)?;
    // SAFETY: the pointers come from `GetProcAddress` on a module that is
    // deliberately never unloaded (`FreeLibrary` is not called), so they stay
    // valid for the rest of the process, like the kernel32 statics they shadow.
    let create: CreateFn = unsafe { std::mem::transmute(create) };
    let resize: ResizeFn = unsafe { std::mem::transmute(resize) };
    let close: CloseFn = unsafe { std::mem::transmute(close) };
    let api = ConptyApi {
        create,
        resize,
        close,
        source: ConptyApiSource::ConptyDll,
    };
    Ok(api)
}

fn validate_conpty_dll_layout(dll_path: &Path) -> io::Result<()> {
    if !dll_path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("conpty.dll not found at {}", dll_path.display()),
        ));
    }
    let Some(directory) = dll_path.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "conpty.dll path has no parent directory: {}",
                dll_path.display()
            ),
        ));
    };
    let helper = directory.join(CONPTY_HELPER_BINARY);
    if !helper.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "{} must sit next to conpty.dll, missing at {}",
                CONPTY_HELPER_BINARY,
                helper.display()
            ),
        ));
    }
    Ok(())
}

fn load_conpty_module(dll_path: &Path) -> io::Result<HMODULE> {
    let mut wide_path: Vec<u16> = dll_path.as_os_str().encode_wide().collect();
    wide_path.push(0);
    // SAFETY: `wide_path` is a NUL-terminated UTF-16 string valid for the call.
    let module = unsafe { LoadLibraryW(wide_path.as_ptr()) };
    if module.is_null() {
        let error = io::Error::last_os_error();
        return Err(io::Error::new(
            error.kind(),
            format!("failed to load {}: {error}", dll_path.display()),
        ));
    }
    Ok(module)
}

fn resolve_export(module: HMODULE, name: &str) -> io::Result<*const c_void> {
    let Ok(export_name) = std::ffi::CString::new(name) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid export name {name}"),
        ));
    };
    // SAFETY: `module` is a live module handle and `export_name` is a
    // NUL-terminated string valid for the call.
    let export =
        unsafe { GetProcAddress(module, export_name.as_ptr() as windows_sys::core::PCSTR) };
    let Some(export) = export else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("conpty.dll is missing the required {name} export"),
        ));
    };
    Ok(export as *const c_void)
}

#[cfg(test)]
mod tests {
    use super::super::flags::conpty_flags_without_passthrough;
    use super::*;
    use std::path::PathBuf;
    use std::ptr::null_mut;
    use std::sync::atomic::{AtomicI32, AtomicIsize, AtomicU32, Ordering};

    const PASSTHROUGH_FLAG_BIT: u32 = 0x8;

    static RECORDED_FLAGS: AtomicU32 = AtomicU32::new(0);
    static RECORDED_SIZE_X: AtomicI32 = AtomicI32::new(0);
    static RECORDED_RESIZE_Y: AtomicI32 = AtomicI32::new(0);
    static RECORDED_HANDLE: AtomicIsize = AtomicIsize::new(0);

    unsafe extern "system" fn recording_create(
        size: COORD,
        _input: HANDLE,
        _output: HANDLE,
        flags: u32,
        pseudoconsole: *mut HPCON,
    ) -> i32 {
        RECORDED_FLAGS.store(flags, Ordering::SeqCst);
        RECORDED_SIZE_X.store(size.X as i32, Ordering::SeqCst);
        if !pseudoconsole.is_null() {
            unsafe { *pseudoconsole = 42 };
        }
        0
    }

    unsafe extern "system" fn recording_resize(pseudoconsole: HPCON, size: COORD) -> i32 {
        RECORDED_HANDLE.store(pseudoconsole, Ordering::SeqCst);
        RECORDED_RESIZE_Y.store(size.Y as i32, Ordering::SeqCst);
        0
    }

    unsafe extern "system" fn recording_close(pseudoconsole: HPCON) {
        RECORDED_HANDLE.store(pseudoconsole, Ordering::SeqCst);
    }

    fn recording_api(source: ConptyApiSource) -> ConptyApi {
        ConptyApi {
            create: recording_create,
            resize: recording_resize,
            close: recording_close,
            source,
        }
    }

    #[test]
    fn conpty_api_source_labels_distinguish_backends() {
        assert_eq!(kernel32_conpty_api().source, ConptyApiSource::Kernel32);
        assert!(!kernel32_conpty_api().source.is_dll());
        assert!(ConptyApiSource::ConptyDll.is_dll());
        assert_eq!(ConptyApiSource::Kernel32.label(), "kernel32");
        assert_eq!(ConptyApiSource::ConptyDll.label(), "conpty.dll");
    }

    #[test]
    fn conpty_api_relays_flags_and_handles_verbatim() {
        let api = recording_api(ConptyApiSource::ConptyDll);
        let mut pseudoconsole: HPCON = 0;
        let flags = conpty_flags_without_passthrough().bits() | PASSTHROUGH_FLAG_BIT;

        let hr = unsafe {
            api.create_pseudo_console(
                COORD { X: 80, Y: 24 },
                null_mut(),
                null_mut(),
                flags,
                &mut pseudoconsole,
            )
        };

        assert_eq!(hr, 0);
        assert_eq!(pseudoconsole, 42);
        assert_eq!(RECORDED_FLAGS.load(Ordering::SeqCst), flags);
        assert_eq!(
            RECORDED_FLAGS.load(Ordering::SeqCst) & PASSTHROUGH_FLAG_BIT,
            PASSTHROUGH_FLAG_BIT
        );
        assert_eq!(RECORDED_SIZE_X.load(Ordering::SeqCst), 80);
    }

    #[test]
    fn conpty_api_routes_resize_and_close_through_the_same_source() {
        let api = recording_api(ConptyApiSource::ConptyDll);

        let hr = unsafe { api.resize_pseudo_console(7, COORD { X: 100, Y: 30 }) };
        unsafe { api.close_pseudo_console(7) };

        assert_eq!(hr, 0);
        assert_eq!(RECORDED_HANDLE.load(Ordering::SeqCst), 7);
        assert_eq!(RECORDED_RESIZE_Y.load(Ordering::SeqCst), 30);
    }

    #[test]
    fn unset_conpty_dll_env_resolves_kernel32() {
        let api = resolve_requested_conpty_api(None).expect("unset env resolves");
        assert_eq!(api.source, ConptyApiSource::Kernel32);
    }

    #[test]
    fn empty_conpty_dll_env_resolves_kernel32() {
        for value in ["", "   "] {
            let api = resolve_requested_conpty_api(Some(value)).expect("blank env resolves");
            assert_eq!(api.source, ConptyApiSource::Kernel32);
        }
    }

    #[test]
    fn missing_conpty_dll_path_fails_naming_the_path() {
        let missing = "Z:\\rmux-missing\\conpty.dll";
        let error = resolve_requested_conpty_api(Some(missing)).expect_err("missing file fails");
        assert!(error.to_string().contains(missing));
    }

    #[test]
    fn conpty_dll_without_openconsole_sibling_fails_naming_the_helper() {
        let directory = unique_temp_dir("no-helper");
        let dll = directory.join("conpty.dll");
        std::fs::write(&dll, b"stub").expect("stub dll");

        let error = resolve_requested_conpty_api(Some(&path_string(&dll)))
            .expect_err("missing helper fails");

        assert!(error.to_string().contains(CONPTY_HELPER_BINARY));
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn conpty_dll_with_openconsole_sibling_fails_gracefully_on_a_stub_dll() {
        let directory = unique_temp_dir("stub-dll");
        std::fs::write(directory.join("conpty.dll"), b"stub").expect("stub dll");
        std::fs::write(directory.join(CONPTY_HELPER_BINARY), b"stub").expect("stub helper");

        let error = resolve_requested_conpty_api(Some(&path_string(&directory.join("conpty.dll"))))
            .expect_err("a stub dll must fail to load, never panic");

        assert!(error.to_string().contains("failed to load"));
        let _ = std::fs::remove_dir_all(&directory);
    }

    fn path_string(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "rmux-conpty-api-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("temp directory creation");
        directory
    }
}
