use super::*;
use crate::endpoint::current_integrity_label;
use crate::endpoint_for_label;
use std::mem::size_of;
use std::os::windows::fs::symlink_file;
use std::process::Command;
use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, GENERIC_READ, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
use windows_sys::Win32::Security::{
    GetLengthSid, SetTokenInformation, TokenIntegrityLevel, SID_AND_ATTRIBUTES,
    TOKEN_ADJUST_DEFAULT, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL,
};
use windows_sys::Win32::System::SystemServices::SE_GROUP_INTEGRITY;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const LOW_INTEGRITY_HELPER_ENV: &str = "RMUX_TEST_LOW_INTEGRITY_ENDPOINT";

fn unique_path(label: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::path::PathBuf::from(
        std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA for native Windows test"),
    )
    .join(format!(
        "RMUX-security-test-{label}-{}-{nanos}",
        std::process::id()
    ))
}

#[test]
fn private_directory_has_the_validated_owner_dacl_and_integrity() {
    let path = unique_path("directory");
    let integrity = current_integrity_label().expect("current integrity");
    ensure_private_directory(&path, integrity).expect("create and validate private directory");
    std::fs::remove_dir(path).expect("remove private directory");
}

#[test]
fn inherited_or_permissive_state_file_is_rejected() {
    let path = unique_path("permissive-file");
    std::fs::write(&path, b"not private state").expect("create ordinary file");
    let handle = open_reparse_safe(&path).expect("open ordinary file");
    let error = validate_private_handle(handle, current_integrity_label().unwrap(), false)
        .expect_err("ordinary inherited DACL must not pass private-state validation");
    unsafe {
        CloseHandle(handle);
    }
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    std::fs::remove_file(path).expect("remove ordinary file");
}

#[test]
fn reparse_point_state_is_rejected_when_symlink_creation_is_available() {
    let target = unique_path("reparse-target");
    let link = unique_path("reparse-link");
    std::fs::write(&target, b"target").expect("create symlink target");
    if symlink_file(&target, &link).is_err() {
        let _ = std::fs::remove_file(target);
        return;
    }

    let handle = open_reparse_safe(&link).expect("open reparse point itself");
    let error = validate_private_handle(handle, current_integrity_label().unwrap(), false)
        .expect_err("reparse point must fail closed");
    unsafe {
        CloseHandle(handle);
    }
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    let _ = std::fs::remove_file(link);
    let _ = std::fs::remove_file(target);
}

#[test]
fn low_integrity_endpoint_discovery_uses_a_writable_known_folder() {
    let label = format!("low-integrity-endpoint-{}", std::process::id());
    let status = Command::new(std::env::current_exe().expect("current test executable"))
        .arg("--exact")
        .arg("windows_endpoint_security::tests::low_integrity_endpoint_discovery_helper")
        .arg("--nocapture")
        .env(LOW_INTEGRITY_HELPER_ENV, label)
        .status()
        .expect("run low-integrity endpoint helper");
    assert!(
        status.success(),
        "low-integrity endpoint helper failed with {status}"
    );
}

#[test]
fn low_integrity_endpoint_discovery_helper() -> io::Result<()> {
    let Some(label) = std::env::var_os(LOW_INTEGRITY_HELPER_ENV) else {
        return Ok(());
    };
    lower_current_process_to_low_integrity()?;
    if current_integrity_label()? != "low" {
        return Err(io::Error::other(
            "low-integrity endpoint helper did not lower its token",
        ));
    }
    endpoint_for_label(label)?;
    Ok(())
}

fn lower_current_process_to_low_integrity() -> io::Result<()> {
    let mut token = std::ptr::null_mut();
    let opened = unsafe {
        // SAFETY: GetCurrentProcess returns a valid pseudo-handle and token is
        // a writable out pointer.
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_DEFAULT | TOKEN_QUERY,
            &mut token,
        )
    };
    if opened == 0 {
        return Err(io::Error::last_os_error());
    }

    let mut sid = std::ptr::null_mut();
    let converted = unsafe {
        // SAFETY: the SID string is NUL-terminated and sid is a writable out
        // pointer released below with LocalFree.
        ConvertStringSidToSidW(wide_null("S-1-16-4096").as_ptr(), &mut sid)
    };
    if converted == 0 {
        let error = io::Error::last_os_error();
        unsafe {
            // SAFETY: OpenProcessToken returned this live handle.
            CloseHandle(token);
        }
        return Err(error);
    }

    let label = TOKEN_MANDATORY_LABEL {
        Label: SID_AND_ATTRIBUTES {
            Sid: sid,
            Attributes: SE_GROUP_INTEGRITY as u32,
        },
    };
    let changed = unsafe {
        // SAFETY: token is live, label references the allocated SID, and the
        // byte count covers both the label header and SID.
        SetTokenInformation(
            token,
            TokenIntegrityLevel,
            (&raw const label).cast(),
            size_of::<TOKEN_MANDATORY_LABEL>() as u32 + GetLengthSid(sid),
        )
    };
    let error = io::Error::last_os_error();
    unsafe {
        // SAFETY: the SID and token were allocated/opened above and ownership
        // is released exactly once.
        LocalFree(sid);
        CloseHandle(token);
    }
    if changed == 0 {
        return Err(error);
    }
    Ok(())
}

fn open_reparse_safe(path: &Path) -> io::Result<HANDLE> {
    let wide = wide_path(path)?;
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(handle)
    }
}
