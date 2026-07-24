//! Private, rotating discovery state for Windows label endpoints.

#![cfg(windows)]

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::zeroed;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, LockFileEx, MoveFileExW, UnlockFileEx, CREATE_NEW, FILE_ATTRIBUTE_HIDDEN,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_WRITE_THROUGH, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, LOCKFILE_EXCLUSIVE_LOCK, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    OPEN_ALWAYS, OPEN_EXISTING, READ_CONTROL,
};
use windows_sys::Win32::System::IO::OVERLAPPED;

use crate::endpoint::{current_integrity_label, pipe_component, LocalEndpoint, PIPE_PREFIX};
use crate::windows_endpoint_legacy;
use crate::windows_endpoint_naming::{
    endpoint_key, pipe_path, random_nonce, random_nonce_excluding,
};
use crate::windows_endpoint_record::{
    is_lower_hex, parse as parse_record, serialize as serialize_record, EndpointPhase,
    EndpointRecord, ProcessStamp, KEY_HEX_LEN, NONCE_HEX_LEN,
};
use crate::windows_endpoint_reservation::{
    decide as decide_reservation, ManagedEndpointStartReservation, ManagedStartClaim,
    ReservationDecision,
};
use crate::windows_endpoint_security::{
    ensure_private_directory, validate_private_handle, wide_path, SameUserSecurityAttributes,
};
use crate::windows_process_stamp::{current as current_process_stamp, is_live as process_is_live};

const MANAGED_PIPE_MARKER: &str = "-g-";
const MAX_STATE_BYTES: u64 = 512;
/// Registration held by a managed listener for its lifetime.
pub(crate) struct ManagedEndpointRegistration {
    key: String,
    nonce: String,
    process: ProcessStamp,
    integrity: &'static str,
}

impl std::fmt::Debug for ManagedEndpointRegistration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedEndpointRegistration")
            .field("key", &self.key)
            .field("process", &self.process)
            .field("integrity", &self.integrity)
            .finish_non_exhaustive()
    }
}

impl Drop for ManagedEndpointRegistration {
    fn drop(&mut self) {
        if let Err(error) = mark_stopped(self) {
            tracing::warn!("failed to retire Windows managed endpoint state: {error}");
        }
    }
}

pub(crate) fn endpoint_for_label(
    label: &OsStr,
    sid_component: &str,
    integrity: &'static str,
) -> io::Result<LocalEndpoint> {
    let key = endpoint_key(label, sid_component, integrity);
    let legacy_component =
        windows_endpoint_legacy::supported_component(sid_component, integrity, label);
    let state = StateStore::open(&key, integrity)?;
    let _lock = state.lock()?;
    let process = current_process_stamp()?;
    let mut record = match state.read_record()? {
        Some(record)
            if matches!(
                record.phase,
                EndpointPhase::Starting | EndpointPhase::Running
            ) && process_is_live(record.process) =>
        {
            return Ok(managed_endpoint_from_parts(
                sid_component,
                integrity,
                &record,
            ));
        }
        Some(mut record)
            if record.phase == EndpointPhase::Resolved && process_is_live(record.process) =>
        {
            if record.legacy_component != legacy_component {
                record.legacy_component = legacy_component;
                record.process = process;
                state.write_record(&record)?;
            }
            return Ok(managed_endpoint_from_parts(
                sid_component,
                integrity,
                &record,
            ));
        }
        Some(mut record) => {
            record.nonce = random_nonce_excluding(&[&record.nonce])?;
            record
        }
        None => EndpointRecord {
            phase: EndpointPhase::Resolved,
            key: key.clone(),
            nonce: random_nonce()?,
            legacy_component: None,
            process,
        },
    };
    record.phase = EndpointPhase::Resolved;
    record.legacy_component = legacy_component;
    record.process = process;
    state.write_record(&record)?;
    Ok(managed_endpoint_from_parts(
        sid_component,
        integrity,
        &record,
    ))
}

/// Resolves the pre-rotation endpoint recorded for a managed endpoint.
///
/// This is intentionally limited to a passive `Resolved` generation. A
/// `Starting` or `Running` generation may own the legacy namespace as a guard,
/// and must never be mistaken for an older daemon. The private state record
/// also has to name the exact managed nonce supplied by the caller.
pub fn legacy_shutdown_endpoint(pipe_name: &Path) -> io::Result<Option<LocalEndpoint>> {
    let Some(components) = managed_components(pipe_name.as_os_str())? else {
        return Ok(None);
    };
    let Some(state) = StateStore::open_existing(&components.key, components.integrity)? else {
        return Ok(None);
    };
    let _lock = state.lock()?;
    let Some(record) = state.read_record()? else {
        return Ok(None);
    };
    if record.phase != EndpointPhase::Resolved
        || record.key != components.key
        || record.nonce != components.nonce
    {
        return Ok(None);
    }
    let Some(endpoint) = legacy_endpoint(&components, record.legacy_component.as_deref()) else {
        return Ok(None);
    };
    windows_endpoint_legacy::namespace_exists(&endpoint).map(|exists| exists.then_some(endpoint))
}

/// Atomically reserves a private Windows label endpoint for daemon startup.
///
/// Explicit `-S`, `RMUX`, and `TMUX` pipe paths are unchanged: paths without
/// the managed private shape remain caller-owned. A dead or stopped managed
/// generation is always rotated before it can be launched again.
pub fn reserve_managed_endpoint_start(
    pipe_name: &Path,
) -> io::Result<ManagedEndpointStartReservation> {
    let Some(components) = managed_components(pipe_name.as_os_str())? else {
        return Ok(ManagedEndpointStartReservation::explicit(
            LocalEndpoint::from_path(pipe_name.to_path_buf()),
        ));
    };
    let state = StateStore::open(&components.key, components.integrity)?;
    let _lock = state.lock()?;
    let requester = current_process_stamp()?;
    let Some(mut record) = state.read_record()? else {
        let record = EndpointRecord {
            phase: EndpointPhase::Starting,
            key: components.key.clone(),
            nonce: components.nonce.clone(),
            legacy_component: None,
            process: requester,
        };
        ensure_legacy_namespace_available(&components, &record)?;
        state.write_record(&record)?;
        let legacy_endpoint = legacy_endpoint(&components, record.legacy_component.as_deref());
        return Ok(ManagedEndpointStartReservation::owner(
            managed_endpoint(&components, &record.nonce),
            components.key,
            record.nonce,
            requester,
            components.integrity,
            legacy_endpoint,
        ));
    };
    let decision = decide_reservation(&record, &components.nonce, process_is_live(record.process));

    if decision == ReservationDecision::JoinCurrent {
        let endpoint = managed_endpoint(&components, &record.nonce);
        let legacy_endpoint = legacy_endpoint(&components, record.legacy_component.as_deref());
        return Ok(ManagedEndpointStartReservation::join(
            endpoint,
            legacy_endpoint,
        ));
    }

    if decision == ReservationDecision::Rotate {
        record.nonce = random_nonce_excluding(&[&record.nonce, &components.nonce])?;
    }
    record.phase = EndpointPhase::Starting;
    record.process = requester;
    ensure_legacy_namespace_available(&components, &record)?;
    state.write_record(&record)?;
    let legacy_endpoint = legacy_endpoint(&components, record.legacy_component.as_deref());
    Ok(ManagedEndpointStartReservation::owner(
        managed_endpoint(&components, &record.nonce),
        components.key,
        record.nonce,
        requester,
        components.integrity,
        legacy_endpoint,
    ))
}

pub(crate) fn register_running(
    pipe_name: &OsStr,
) -> io::Result<Option<ManagedEndpointRegistration>> {
    let Some(components) = managed_components(pipe_name)? else {
        return Ok(None);
    };
    let state = StateStore::open_existing(&components.key, components.integrity)?
        .ok_or_else(|| invalid_state("managed endpoint discovery state is missing"))?;
    let _lock = state.lock()?;
    let mut record = state
        .read_record()?
        .ok_or_else(|| invalid_state("managed endpoint discovery record is missing"))?;
    if record.key != components.key
        || record.nonce != components.nonce
        || record.phase != EndpointPhase::Starting
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "managed endpoint generation is no longer reserved for startup",
        ));
    }

    let process = current_process_stamp()?;
    record.phase = EndpointPhase::Running;
    record.process = process;
    state.write_record(&record)?;
    Ok(Some(ManagedEndpointRegistration {
        key: components.key,
        nonce: components.nonce,
        process,
        integrity: components.integrity,
    }))
}

pub(crate) fn retire_starting_claim(claim: &ManagedStartClaim) -> io::Result<()> {
    retire_matching_record(
        &claim.key,
        &claim.nonce,
        claim.process,
        claim.integrity,
        EndpointPhase::Starting,
    )
}

fn mark_stopped(registration: &ManagedEndpointRegistration) -> io::Result<()> {
    retire_matching_record(
        &registration.key,
        &registration.nonce,
        registration.process,
        registration.integrity,
        EndpointPhase::Running,
    )
}

fn retire_matching_record(
    key: &str,
    nonce: &str,
    process: ProcessStamp,
    integrity: &'static str,
    expected_phase: EndpointPhase,
) -> io::Result<()> {
    let Some(state) = StateStore::open_existing(key, integrity)? else {
        return Ok(());
    };
    let _lock = state.lock()?;
    let Some(mut record) = state.read_record()? else {
        return Ok(());
    };
    if record.key != key
        || record.nonce != nonce
        || record.process != process
        || record.phase != expected_phase
    {
        return Ok(());
    }
    record.phase = EndpointPhase::Stopped;
    state.write_record(&record)
}

struct ManagedComponents {
    sid: String,
    key: String,
    nonce: String,
    integrity: &'static str,
}

fn managed_components(pipe_name: &OsStr) -> io::Result<Option<ManagedComponents>> {
    let identity = rmux_os::identity::IdentityResolver::current()?;
    let rmux_os::identity::UserIdentity::Sid(sid) = identity else {
        return Err(io::Error::other(
            "Windows identity resolver returned a Unix uid",
        ));
    };
    let sid = pipe_component(OsStr::new(sid.as_ref()));
    let integrity = current_integrity_label()?;
    let prefix = format!("{PIPE_PREFIX}rmux-{sid}-il-{integrity}{MANAGED_PIPE_MARKER}");
    let display = pipe_name.to_string_lossy();
    let Some(rest) = strip_ascii_prefix(&display, &prefix) else {
        return Ok(None);
    };
    if rest.len() != KEY_HEX_LEN + 1 + NONCE_HEX_LEN {
        return Ok(None);
    }
    let (key, nonce) = rest.split_at(KEY_HEX_LEN);
    let Some(nonce) = nonce.strip_prefix('-') else {
        return Ok(None);
    };
    if !is_lower_hex(key) || !is_lower_hex(nonce) {
        return Ok(None);
    }
    Ok(Some(ManagedComponents {
        sid,
        key: key.to_owned(),
        nonce: nonce.to_owned(),
        integrity,
    }))
}

fn managed_endpoint(components: &ManagedComponents, nonce: &str) -> LocalEndpoint {
    LocalEndpoint::from_path(pipe_path(
        &components.sid,
        components.integrity,
        &components.key,
        nonce,
    ))
}

fn managed_endpoint_from_parts(
    sid: &str,
    integrity: &str,
    record: &EndpointRecord,
) -> LocalEndpoint {
    LocalEndpoint::from_path(pipe_path(sid, integrity, &record.key, &record.nonce))
}

fn ensure_legacy_namespace_available(
    components: &ManagedComponents,
    record: &EndpointRecord,
) -> io::Result<()> {
    let Some(component) = record.legacy_component.as_deref() else {
        return Ok(());
    };
    let endpoint =
        windows_endpoint_legacy::endpoint(&components.sid, components.integrity, component);
    if windows_endpoint_legacy::namespace_exists(&endpoint)? {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "an older RMUX daemon still owns this label; stop it before starting the current daemon",
        ));
    }
    Ok(())
}

fn legacy_endpoint(
    components: &ManagedComponents,
    component: Option<&str>,
) -> Option<LocalEndpoint> {
    component.map(|component| {
        windows_endpoint_legacy::endpoint(&components.sid, components.integrity, component)
    })
}

struct StateStore {
    root: PathBuf,
    state_path: PathBuf,
    lock_path: PathBuf,
    key: String,
    integrity: &'static str,
}

impl StateStore {
    fn open(key: &str, integrity: &'static str) -> io::Result<Self> {
        let store = Self::paths(key, integrity)?;
        ensure_private_directory(&store.root, integrity)?;
        Ok(store)
    }

    fn open_existing(key: &str, integrity: &'static str) -> io::Result<Option<Self>> {
        let store = Self::paths(key, integrity)?;
        match std::fs::symlink_metadata(&store.root) {
            Ok(_) => ensure_private_directory(&store.root, integrity)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
        if !store.lock_path.exists() || !store.state_path.exists() {
            return Ok(None);
        }
        Ok(Some(store))
    }

    fn paths(key: &str, integrity: &'static str) -> io::Result<Self> {
        let local_app_data = crate::windows_endpoint_directory::state_base(integrity)?;
        let root = local_app_data.join(format!("RMUX-runtime-{integrity}"));
        Ok(Self {
            state_path: root.join(format!("{key}.state")),
            lock_path: root.join(format!("{key}.lock")),
            root,
            key: key.to_owned(),
            integrity,
        })
    }

    fn lock(&self) -> io::Result<StateLock> {
        StateLock::open(&self.lock_path, self.integrity)
    }

    fn read_record(&self) -> io::Result<Option<EndpointRecord>> {
        let Some(mut file) =
            open_private_file(&self.state_path, self.integrity, FileDisposition::Existing)?
        else {
            return Ok(None);
        };
        let len = file.metadata()?.len();
        if len > MAX_STATE_BYTES {
            return Err(invalid_state("managed endpoint state is oversized"));
        }
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        parse_record(&text, &self.key).map(Some)
    }

    fn write_record(&self, record: &EndpointRecord) -> io::Result<()> {
        let suffix = random_nonce()?;
        let temporary = self.root.join(format!("{}.{}.tmp", self.key, suffix));
        let mut file = open_private_file(&temporary, self.integrity, FileDisposition::CreateNew)?
            .ok_or_else(|| {
            io::Error::other("failed to create endpoint state temporary file")
        })?;
        file.write_all(serialize_record(record).as_bytes())?;
        file.sync_all()?;
        drop(file);

        let source = wide_path(&temporary)?;
        let destination = wide_path(&self.state_path)?;
        let moved = unsafe {
            // SAFETY: both paths are NUL-terminated. The stable lock file is
            // held by the caller across this atomic replacement.
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if moved == 0 {
            let error = io::Error::last_os_error();
            let _ = std::fs::remove_file(&temporary);
            return Err(error);
        }
        Ok(())
    }
}

struct StateLock {
    file: File,
}

impl StateLock {
    fn open(path: &Path, integrity: &str) -> io::Result<Self> {
        let file = open_private_file(path, integrity, FileDisposition::OpenOrCreate)?
            .ok_or_else(|| io::Error::other("failed to create endpoint state lock"))?;
        let mut overlapped: OVERLAPPED = unsafe { zeroed() };
        let ok = unsafe {
            // SAFETY: file is live and overlapped describes the first byte.
            LockFileEx(
                file.as_raw_handle() as HANDLE,
                LOCKFILE_EXCLUSIVE_LOCK,
                0,
                1,
                0,
                &mut overlapped,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { file })
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        let mut overlapped: OVERLAPPED = unsafe { zeroed() };
        unsafe {
            // SAFETY: this handle owns the first-byte lock acquired above.
            let _ = UnlockFileEx(
                self.file.as_raw_handle() as HANDLE,
                0,
                1,
                0,
                &mut overlapped,
            );
        }
    }
}

#[derive(Clone, Copy)]
enum FileDisposition {
    Existing,
    OpenOrCreate,
    CreateNew,
}

fn open_private_file(
    path: &Path,
    integrity: &str,
    disposition: FileDisposition,
) -> io::Result<Option<File>> {
    let wide = wide_path(path)?;
    let security = SameUserSecurityAttributes::new(integrity)?;
    let handle = unsafe {
        // SAFETY: path and security attributes remain live for the call.
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            security.as_ptr(),
            match disposition {
                FileDisposition::Existing => OPEN_EXISTING,
                FileDisposition::OpenOrCreate => OPEN_ALWAYS,
                FileDisposition::CreateNew => CREATE_NEW,
            },
            FILE_ATTRIBUTE_HIDDEN | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_WRITE_THROUGH,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let error = io::Error::last_os_error();
        if matches!(disposition, FileDisposition::Existing)
            && error.kind() == io::ErrorKind::NotFound
        {
            return Ok(None);
        }
        return Err(error);
    }
    if let Err(error) = validate_private_handle(handle, integrity, false) {
        unsafe {
            // SAFETY: handle was returned by CreateFileW.
            CloseHandle(handle);
        }
        return Err(error);
    }
    let file = unsafe {
        // SAFETY: ownership of the validated handle transfers exactly once.
        File::from_raw_handle(handle)
    };
    Ok(Some(file))
}

fn strip_ascii_prefix<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        .then(|| &value[prefix.len()..])
}

fn invalid_state(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
