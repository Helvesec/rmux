//! Executable fixtures that a test may run while other tests fork.
//!
//! The one copy for every crate whose tests write a program and then run it:
//! a library includes it with `#[path]` from its test configuration, an
//! integration test binary from its root, each as a module named
//! `executable_fixture`.
//!
//! Linux refuses to execute a file that any process still holds open for
//! writing (ETXTBSY). A child that another test thread forks inherits every
//! descriptor of this process, close-on-exec ones included, until its own exec
//! closes them, and under load that exec can come late. A program this process
//! wrote itself could therefore fail to start after the write had returned.
//! So on Unix a short-lived `sh` child writes the file, and this process never
//! opens an executable fixture for writing.

use std::io;
use std::path::Path;

/// Writes `contents` to `path` and, on Unix, gives the file `mode`.
pub(crate) fn write_executable(
    path: &Path,
    contents: impl AsRef<[u8]>,
    mode: u32,
) -> io::Result<()> {
    write_executable_while(path, contents.as_ref(), mode, || {})
}

/// [`write_executable`], calling `while_writing` while the file is being
/// written, at the point where a fork from another test thread would copy
/// whatever this process then holds open.
#[cfg(unix)]
pub(crate) fn write_executable_while(
    path: &Path,
    contents: &[u8],
    mode: u32,
    while_writing: impl FnOnce(),
) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};

    // An empty environment and absolute programs: a test that changes PATH
    // must not choose the program that writes another test's fixture.
    let mut writer = Command::new("/bin/sh")
        .args(["-c", "exec /bin/cat > \"$1\"", "sh"])
        .arg(path)
        .env_clear()
        .stdin(Stdio::piped())
        .spawn()?;
    let fed = writer
        .stdin
        .take()
        .expect("the writer has a piped stdin")
        .write_all(contents);
    while_writing();
    let status = writer.wait()?;
    fed?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "writing executable fixture {} failed: {status}",
            path.display()
        )));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// [`write_executable`]: outside Unix no inherited descriptor keeps the file
/// busy, and `mode` has no meaning.
#[cfg(not(unix))]
pub(crate) fn write_executable_while(
    path: &Path,
    contents: &[u8],
    _mode: u32,
    while_writing: impl FnOnce(),
) -> io::Result<()> {
    std::fs::write(path, contents)?;
    while_writing();
    Ok(())
}
