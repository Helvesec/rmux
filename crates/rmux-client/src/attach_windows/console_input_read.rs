use std::io;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Console::{
    GetNumberOfConsoleInputEvents, ReadConsoleInputW, INPUT_RECORD,
};

use super::console_coordination::ConsoleIoCoordinator;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConsoleInputRead {
    NoEvents,
    Records { records_read: usize, drained: bool },
}

pub(super) trait ConsoleInputApi {
    fn event_count(&self, handle: HANDLE) -> io::Result<u32>;

    fn read_records(&self, handle: HANDLE, records: &mut [INPUT_RECORD]) -> io::Result<usize>;
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Win32ConsoleInput;

impl ConsoleInputApi for Win32ConsoleInput {
    fn event_count(&self, handle: HANDLE) -> io::Result<u32> {
        let mut event_count = 0;
        let ok = unsafe {
            // SAFETY: `event_count` points to writable storage and `handle` is
            // borrowed only for the duration of the query.
            GetNumberOfConsoleInputEvents(handle, &mut event_count)
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(event_count)
    }

    fn read_records(&self, handle: HANDLE, records: &mut [INPUT_RECORD]) -> io::Result<usize> {
        let capacity = u32::try_from(records.len()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "console input record capacity exceeds Win32's u32 limit",
            )
        })?;
        let mut records_read = 0;
        let ok = unsafe {
            // SAFETY: `records` points to writable INPUT_RECORD storage,
            // `records_read` is a valid output pointer, and `handle` is
            // borrowed for the duration of the call.
            ReadConsoleInputW(handle, records.as_mut_ptr(), capacity, &mut records_read)
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        usize::try_from(records_read).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Win32 returned an invalid console input record count",
            )
        })
    }
}

pub(super) fn read_console_input_batch_with<Api>(
    coordinator: &ConsoleIoCoordinator,
    api: &Api,
    handle: HANDLE,
    records: &mut [INPUT_RECORD],
) -> io::Result<ConsoleInputRead>
where
    Api: ConsoleInputApi,
{
    coordinator.synchronized(|| {
        // `WaitForSingleObject` and its readability probe happen before the
        // attach input-read lease is acquired. Teardown may therefore flush
        // the console before this critical section starts. Revalidate while
        // holding the same coordinator used by flush and mode restoration so
        // an empty buffer never reaches blocking `ReadConsoleInputW`.
        if api.event_count(handle)? == 0 {
            return Ok(ConsoleInputRead::NoEvents);
        }

        let records_read = api.read_records(handle, records)?;
        if records_read > records.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "console input API returned more records than the supplied buffer",
            ));
        }
        let drained = api.event_count(handle).is_ok_and(|pending| pending == 0);
        Ok(ConsoleInputRead::Records {
            records_read,
            drained,
        })
    })?
}
