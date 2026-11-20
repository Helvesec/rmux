use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::PathBuf;

use super::{start_read, CancellationState, SpecialFile};

pub(super) async fn read_special_discovered_after_classification(
    path: PathBuf,
    file: File,
    special: SpecialFile,
) -> io::Result<Vec<u8>> {
    start_read(path, special, Some(file)).await
}

pub(super) fn try_read_fifo(_state: &CancellationState) -> Option<io::Result<Vec<u8>>> {
    None
}

pub(super) fn run_helper_if_requested<I>(_arguments: I) -> Option<i32>
where
    I: IntoIterator<Item = OsString>,
{
    None
}
