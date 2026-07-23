use std::ffi::OsString;
use std::io;

use super::CancellationState;

pub(super) fn try_read_fifo(_state: &CancellationState) -> Option<io::Result<Vec<u8>>> {
    None
}

pub(super) fn run_helper_if_requested<I>(_arguments: I) -> Option<i32>
where
    I: IntoIterator<Item = OsString>,
{
    None
}
