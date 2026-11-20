use std::io;
use std::path::PathBuf;

#[cfg(windows)]
use std::fs::{File, OpenOptions};
#[cfg(windows)]
use std::io::Write;
#[cfg(windows)]
use std::path::Path;

mod limit;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as platform;

#[cfg(unix)]
pub(crate) fn run_internal_fifo_reader_helper<I>(arguments: I) -> Option<i32>
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    platform::run_internal_fifo_reader_helper(arguments)
}
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;

#[cfg(unix)]
pub(crate) async fn read(path: PathBuf) -> io::Result<Vec<u8>> {
    platform::read(path).await
}

#[cfg(windows)]
pub(crate) async fn read(path: PathBuf) -> io::Result<Vec<u8>> {
    if let Some(kind) = platform::special_path(&path)? {
        return platform::read(path, kind).await;
    }

    tokio::task::spawn_blocking(move || limit::read_regular_file(File::open(path)?))
        .await
        .map_err(|error| io::Error::other(format!("buffer file reader failed: {error}")))?
}

#[cfg(unix)]
pub(crate) async fn write(path: PathBuf, content: Vec<u8>, append: bool) -> io::Result<()> {
    platform::write(path, content, append).await
}

#[cfg(windows)]
pub(crate) async fn write(path: PathBuf, content: Vec<u8>, append: bool) -> io::Result<()> {
    if let Some(kind) = platform::special_path(&path)? {
        return platform::write(path, content, append, kind).await;
    }

    tokio::task::spawn_blocking(move || write_regular_file(&path, &content, append))
        .await
        .map_err(|error| io::Error::other(format!("buffer file writer failed: {error}")))?
}

#[cfg(windows)]
fn write_regular_file(destination: &Path, content: &[u8], append: bool) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.create(true).write(true);
    if append {
        options.append(true);
    } else {
        options.truncate(true);
    }
    options.open(destination)?.write_all(content)
}
