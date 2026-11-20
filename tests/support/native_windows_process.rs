//! Kernel identity and waits always use the Child's original owned handle.
use std::ffi::c_void;
use std::fs::File;
use std::io::{self, Read};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) type Result<T> = std::result::Result<T, NativeError>;

#[derive(Debug)]
pub(super) enum NativeError {
    Io(io::Error),
    Json(serde_json::Error),
    Contract(&'static str),
    Kernel(&'static str, io::Error),
}

impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Json(error) => write!(f, "{error}"),
            Self::Contract(message) => f.write_str(message),
            Self::Kernel(api, error) => write!(f, "{api}: {error}"),
        }
    }
}
impl std::error::Error for NativeError {}
impl From<io::Error> for NativeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<serde_json::Error> for NativeError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

type Handle = *mut c_void;
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FileTime {
    low: u32,
    high: u32,
}
impl FileTime {
    fn ticks(self) -> u64 {
        (u64::from(self.high) << 32) | u64::from(self.low)
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess() -> Handle;
    fn GetProcessId(handle: Handle) -> u32;
    fn GetProcessTimes(
        handle: Handle,
        birth: *mut FileTime,
        end: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
    fn QueryFullProcessImageNameW(
        handle: Handle,
        flags: u32,
        image: *mut u16,
        length: *mut u32,
    ) -> i32;
    fn WaitForSingleObject(handle: Handle, timeout: u32) -> u32;
    fn GetExitCodeProcess(handle: Handle, exit: *mut u32) -> i32;
    fn GetDiskFreeSpaceExW(
        path: *const u16,
        available: *mut u64,
        total: *mut u64,
        free: *mut u64,
    ) -> i32;
    fn GetFileInformationByHandleEx(
        handle: Handle,
        class: u32,
        information: *mut c_void,
        length: u32,
    ) -> i32;
}

#[derive(Clone, Debug)]
pub(super) struct Identity {
    pub pid: u32,
    pub birth: u64,
    pub end: u64,
    pub image: PathBuf,
}
impl Identity {
    pub fn record(&self) -> serde_json::Value {
        serde_json::json!({"pid":self.pid,"birth_100ns":self.birth,
            "birth_ns":epoch_ns(self.birth),"end_ns":if self.end == 0 {None} else {Some(epoch_ns(self.end))},
            "image":self.image})
    }
    pub fn same_life(&self, other: &Self) -> bool {
        self.pid == other.pid && self.birth == other.birth && same_path(&self.image, &other.image)
    }
}

fn epoch_ns(ticks: u64) -> u64 {
    ticks.saturating_sub(116_444_736_000_000_000) * 100
}
pub(super) fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Windows epoch")
        .as_nanos() as u64
}
pub(super) fn same_path(a: &Path, b: &Path) -> bool {
    fn normalized(path: &Path) -> String {
        path.to_string_lossy()
            .trim_start_matches(r"\\?\")
            .replace('/', "\\")
            .to_lowercase()
    }
    normalized(a) == normalized(b)
}
fn api_ok(ok: i32, name: &'static str) -> Result<()> {
    if ok != 0 {
        Ok(())
    } else {
        Err(NativeError::Kernel(name, io::Error::last_os_error()))
    }
}
fn identity(handle: Handle, known_image: Option<&Path>) -> Result<Identity> {
    let mut times = [FileTime::default(); 4];
    unsafe {
        api_ok(
            GetProcessTimes(
                handle,
                &mut times[0],
                &mut times[1],
                &mut times[2],
                &mut times[3],
            ),
            "GetProcessTimes",
        )?;
        let pid = GetProcessId(handle);
        if pid == 0 || times[0].ticks() == 0 {
            return Err(NativeError::Contract("process has no kernel birth"));
        }
        let image = if let Some(image) = known_image {
            image.to_owned()
        } else {
            let mut buffer = vec![0u16; 32768];
            let mut length = buffer.len() as u32;
            api_ok(
                QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length),
                "QueryFullProcessImageNameW",
            )?;
            use std::os::windows::ffi::OsStringExt;
            PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length as usize]))
        };
        Ok(Identity {
            pid,
            birth: times[0].ticks(),
            end: times[1].ticks(),
            image,
        })
    }
}
pub(super) fn current() -> Result<Identity> {
    unsafe { identity(GetCurrentProcess(), None) }
}
pub(super) fn child_identity(child: &Child) -> Result<Identity> {
    identity(child.as_raw_handle(), None)
}

pub(super) fn capacity(path: &Path, reserve: u64, additional: u64) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0;
    unsafe {
        api_ok(
            GetDiskFreeSpaceExW(
                path.as_ptr(),
                &mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ),
            "GetDiskFreeSpaceExW",
        )?;
    }
    if available < 50 * (1u64 << 30) + reserve + additional {
        return Err(NativeError::Contract(
            "Windows native retention floor and reserve unavailable",
        ));
    }
    Ok(())
}

#[repr(C)]
#[derive(Default)]
struct FileId {
    volume: u64,
    id: [u8; 16],
}
pub(super) fn verify_stamp(path: &Path, expected: &serde_json::Value) -> Result<()> {
    use std::os::windows::fs::MetadataExt;
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    let mut id = FileId::default();
    unsafe {
        api_ok(
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                18,
                (&mut id as *mut FileId).cast(),
                std::mem::size_of::<FileId>() as u32,
            ),
            "GetFileInformationByHandleEx",
        )?;
    }
    let inode = u128::from_le_bytes(id.id);
    let stamp =
        expected
            .as_array()
            .filter(|stamp| stamp.len() == 6)
            .ok_or(NativeError::Contract(
                "invalid Windows retained input stamp",
            ))?;
    let number = |index: usize| {
        stamp[index]
            .as_u64()
            .ok_or(NativeError::Contract("invalid Windows stamp number"))
    };
    if number(0)? != id.volume
        || u128::from(number(1)?) != inode
        || (number(2)? & 0o222 == 0) != metadata.permissions().readonly()
        || number(3)? != metadata.file_size()
        || number(4)? != epoch_ns(metadata.last_write_time())
        || number(5)? != epoch_ns(metadata.creation_time())
    {
        return Err(NativeError::Contract(
            "Windows retained input identity or stamp changed",
        ));
    }
    Ok(())
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtResumeProcess(handle: Handle) -> i32;
}
pub(super) fn resume(child: &Child) -> Result<()> {
    // std owns the process handle but closes CreateProcess's primary thread
    // handle. Resume that same process after observing its suspended birth.
    if unsafe { NtResumeProcess(child.as_raw_handle()) } < 0 {
        return Err(NativeError::Contract("could not resume the observed child"));
    }
    Ok(())
}

pub(super) struct Wait {
    pub child: Identity,
    pub started: u64,
    pub finished: u64,
    pub exit: u32,
    pub rust_exit: u32,
    pub rust_finished: u64,
    pub status: std::process::ExitStatus,
    pub returned: u32,
    pub handle: usize,
}

pub(super) fn wait(child: &mut Child, before: &Identity) -> Result<Wait> {
    let handle = child.as_raw_handle();
    let actual = identity(handle, Some(&before.image))?;
    if !actual.same_life(before) {
        return Err(NativeError::Contract("child handle lifetime changed"));
    }
    let started = now_ns();
    let result = unsafe { WaitForSingleObject(handle, u32::MAX) };
    let finished = now_ns();
    if result != 0 {
        return Err(NativeError::Contract(
            "parental wait did not return WAIT_OBJECT_0",
        ));
    }
    let mut exit = 0;
    unsafe {
        api_ok(GetExitCodeProcess(handle, &mut exit), "GetExitCodeProcess")?;
    }
    let status = child.wait()?;
    let rust_finished = now_ns();
    let rust_exit = status
        .code()
        .ok_or(NativeError::Contract("Rust wait has no exit code"))? as u32;
    let actual = identity(handle, Some(&before.image))?;
    if !actual.same_life(before) || actual.end == 0 || exit != rust_exit {
        return Err(NativeError::Contract(
            "native end or Rust wait disagrees with original child",
        ));
    }
    Ok(Wait {
        child: actual,
        started,
        finished,
        exit,
        rust_exit,
        rust_finished,
        status,
        returned: result,
        handle: handle as usize,
    })
}

#[link(name = "bcrypt")]
unsafe extern "system" {
    fn BCryptOpenAlgorithmProvider(
        algorithm: *mut Handle,
        name: *const u16,
        implementation: *const u16,
        flags: u32,
    ) -> i32;
    fn BCryptCloseAlgorithmProvider(algorithm: Handle, flags: u32) -> i32;
    fn BCryptCreateHash(
        algorithm: Handle,
        hash: *mut Handle,
        object: *mut u8,
        object_size: u32,
        secret: *const u8,
        secret_size: u32,
        flags: u32,
    ) -> i32;
    fn BCryptHashData(hash: Handle, bytes: *const u8, length: u32, flags: u32) -> i32;
    fn BCryptFinishHash(hash: Handle, output: *mut u8, length: u32, flags: u32) -> i32;
    fn BCryptDestroyHash(hash: Handle) -> i32;
}
struct Hash {
    algorithm: Handle,
    hash: Handle,
}
impl Drop for Hash {
    fn drop(&mut self) {
        unsafe {
            if !self.hash.is_null() {
                BCryptDestroyHash(self.hash);
            }
            if !self.algorithm.is_null() {
                BCryptCloseAlgorithmProvider(self.algorithm, 0);
            }
        }
    }
}
fn cng(status: i32) -> Result<()> {
    if status >= 0 {
        Ok(())
    } else {
        Err(NativeError::Contract("Windows SHA256 failed"))
    }
}
pub(super) fn sha(path: &Path) -> Result<String> {
    digest(File::open(path)?)
}
pub(super) fn sha_bytes(bytes: &[u8]) -> Result<String> {
    digest(bytes)
}
fn digest(mut input: impl Read) -> Result<String> {
    let mut context = Hash {
        algorithm: std::ptr::null_mut(),
        hash: std::ptr::null_mut(),
    };
    let name: Vec<u16> = "SHA256\0".encode_utf16().collect();
    unsafe {
        cng(BCryptOpenAlgorithmProvider(
            &mut context.algorithm,
            name.as_ptr(),
            std::ptr::null(),
            0,
        ))?;
        cng(BCryptCreateHash(
            context.algorithm,
            &mut context.hash,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            0,
            0,
        ))?;
    }
    let mut buffer = [0u8; 65536];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        unsafe {
            cng(BCryptHashData(
                context.hash,
                buffer.as_ptr(),
                count as u32,
                0,
            ))?;
        }
    }
    let mut output = [0u8; 32];
    unsafe {
        cng(BCryptFinishHash(
            context.hash,
            output.as_mut_ptr(),
            output.len() as u32,
            0,
        ))?;
    }
    Ok(output.iter().map(|byte| format!("{byte:02x}")).collect())
}
