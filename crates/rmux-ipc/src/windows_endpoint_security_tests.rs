use super::*;
use crate::endpoint::current_integrity_label;
use std::os::windows::fs::symlink_file;
use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL,
};

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
