use std::io;

use windows_sys::Win32::Foundation::{GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    GetConsoleMode, GetStdHandle, SetConsoleMode, WriteConsoleW, ENABLE_VIRTUAL_TERMINAL_INPUT,
    STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};

use super::console_coordination::{ConsoleIoCoordinator, ATTACH_CONSOLE_IO};
use super::windows_version::{current_windows_version, supports_scoped_vt_input, WindowsVersion};

// Keep each WriteConsoleW request comfortably below the console host's
// internal 64-KiB-class buffers. Large terminal strings (notably OSC 52)
// otherwise turn one transient console allocation limit into a fatal attach
// output error.
const MAX_WRITE_CONSOLE_CODE_UNITS: usize = 16 * 1024;

#[derive(Clone, Copy, Debug)]
pub(super) struct ScopedVtInputPassthrough {
    handles: ConsoleHandles<HANDLE>,
}

impl ScopedVtInputPassthrough {
    /// Enables the bridge only for the Windows builds where it was measured to
    /// make ConPTY expose terminal input-reporting traffic, and only when the
    /// process's actual stdin and stdout are both console handles.
    pub(super) fn for_output(output_handle: HANDLE) -> Option<Self> {
        let handles = ATTACH_CONSOLE_IO
            .synchronized(|| {
                eligible_console_handles(&Win32ConsoleApi, current_windows_version(), output_handle)
            })
            .ok()
            .flatten()?;
        Some(Self { handles })
    }

    pub(super) fn write_wide(&self, wide: &[u16]) -> io::Result<()> {
        let result =
            coordinated_scoped_write(&ATTACH_CONSOLE_IO, &Win32ConsoleApi, self.handles, wide)?;
        result.map_err(scoped_failure_to_io)
    }
}

pub(super) fn write_console_wide(handle: HANDLE, wide: &[u16]) -> io::Result<()> {
    Win32ConsoleApi
        .write_console(handle, wide)
        .map_err(win32_failure_to_io)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ConsoleHandles<Handle> {
    input: Handle,
    output: Handle,
}

trait ScopedConsoleApi {
    type Handle: Copy + Eq;
    type Error;

    fn std_handle(&self, handle_id: u32) -> Result<Option<Self::Handle>, Self::Error>;
    fn console_mode(&self, handle: Self::Handle) -> Result<u32, Self::Error>;
    fn set_console_mode(&self, handle: Self::Handle, mode: u32) -> Result<(), Self::Error>;
    fn write_console(&self, handle: Self::Handle, wide: &[u16]) -> Result<(), Self::Error>;
}

fn eligible_console_handles<Api>(
    api: &Api,
    version: Option<WindowsVersion>,
    writer_output: Api::Handle,
) -> Option<ConsoleHandles<Api::Handle>>
where
    Api: ScopedConsoleApi,
{
    let version = version?;
    if !supports_scoped_vt_input(version) {
        return None;
    }

    let input = api.std_handle(STD_INPUT_HANDLE).ok().flatten()?;
    let output = api.std_handle(STD_OUTPUT_HANDLE).ok().flatten()?;
    if output != writer_output {
        return None;
    }
    api.console_mode(input).ok()?;
    api.console_mode(output).ok()?;
    Some(ConsoleHandles { input, output })
}

#[derive(Debug, Eq, PartialEq)]
enum ScopedWriteFailure<Error> {
    Snapshot(Error),
    Enable(Error),
    Write(Error),
    Restore(Error),
}

fn coordinated_scoped_write<Api>(
    coordinator: &ConsoleIoCoordinator,
    api: &Api,
    handles: ConsoleHandles<Api::Handle>,
    wide: &[u16],
) -> io::Result<Result<(), ScopedWriteFailure<Api::Error>>>
where
    Api: ScopedConsoleApi,
{
    coordinator.synchronized(|| scoped_write(api, handles, wide))
}

fn scoped_write<Api>(
    api: &Api,
    handles: ConsoleHandles<Api::Handle>,
    wide: &[u16],
) -> Result<(), ScopedWriteFailure<Api::Error>>
where
    Api: ScopedConsoleApi,
{
    let original_mode = api
        .console_mode(handles.input)
        .map_err(ScopedWriteFailure::Snapshot)?;
    if original_mode & ENABLE_VIRTUAL_TERMINAL_INPUT != 0 {
        return api
            .write_console(handles.output, wide)
            .map_err(ScopedWriteFailure::Write);
    }

    api.set_console_mode(handles.input, original_mode | ENABLE_VIRTUAL_TERMINAL_INPUT)
        .map_err(ScopedWriteFailure::Enable)?;
    let restore = InputModeRestore::new(api, handles.input, original_mode);
    let write_result = api.write_console(handles.output, wide);
    let restore_result = restore.restore();

    // A failed restoration is always fatal and takes precedence over a write
    // error because otherwise the input thread could continue in VT mode.
    restore_result.map_err(ScopedWriteFailure::Restore)?;
    write_result.map_err(ScopedWriteFailure::Write)
}

struct InputModeRestore<'a, Api>
where
    Api: ScopedConsoleApi,
{
    api: &'a Api,
    handle: Api::Handle,
    mode: u32,
    armed: bool,
}

impl<'a, Api> InputModeRestore<'a, Api>
where
    Api: ScopedConsoleApi,
{
    const fn new(api: &'a Api, handle: Api::Handle, mode: u32) -> Self {
        Self {
            api,
            handle,
            mode,
            armed: true,
        }
    }

    fn restore(mut self) -> Result<(), Api::Error> {
        let result = self.api.set_console_mode(self.handle, self.mode);
        if result.is_ok() {
            self.armed = false;
        }
        result
    }
}

impl<Api> Drop for InputModeRestore<'_, Api>
where
    Api: ScopedConsoleApi,
{
    fn drop(&mut self) {
        if self.armed {
            let _ = self.api.set_console_mode(self.handle, self.mode);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Win32ConsoleFailure {
    Os(u32),
    WriteZero,
}

#[derive(Clone, Copy, Debug)]
struct Win32ConsoleApi;

impl ScopedConsoleApi for Win32ConsoleApi {
    type Handle = HANDLE;
    type Error = Win32ConsoleFailure;

    fn std_handle(&self, handle_id: u32) -> Result<Option<Self::Handle>, Self::Error> {
        let handle = unsafe {
            // SAFETY: GetStdHandle accepts the documented STD_* identifiers.
            GetStdHandle(handle_id)
        };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Ok(None);
        }
        Ok(Some(handle))
    }

    fn console_mode(&self, handle: Self::Handle) -> Result<u32, Self::Error> {
        let mut mode = 0;
        let ok = unsafe {
            // SAFETY: handle is borrowed and mode points to writable storage.
            GetConsoleMode(handle, &mut mode)
        };
        if ok == 0 {
            return Err(last_win32_failure());
        }
        Ok(mode)
    }

    fn set_console_mode(&self, handle: Self::Handle, mode: u32) -> Result<(), Self::Error> {
        let ok = unsafe {
            // SAFETY: handle was validated as a console input handle and mode
            // is an exact snapshot or that snapshot plus one documented bit.
            SetConsoleMode(handle, mode)
        };
        if ok == 0 {
            return Err(last_win32_failure());
        }
        Ok(())
    }

    fn write_console(&self, handle: Self::Handle, wide: &[u16]) -> Result<(), Self::Error> {
        let mut written = 0;
        while written < wide.len() {
            let chunk_len = console_write_chunk_len(&wide[written..]) as u32;
            let mut chars_written = 0;
            let ok = unsafe {
                // SAFETY: handle is a validated console output handle and the
                // slice contains initialized UTF-16 code units.
                WriteConsoleW(
                    handle,
                    wide[written..].as_ptr().cast(),
                    chunk_len,
                    &mut chars_written,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(last_win32_failure());
            }
            if chars_written == 0 {
                return Err(Win32ConsoleFailure::WriteZero);
            }
            written += chars_written as usize;
        }
        Ok(())
    }
}

fn console_write_chunk_len(remaining: &[u16]) -> usize {
    let mut chunk_len = remaining.len().min(MAX_WRITE_CONSOLE_CODE_UNITS);
    if chunk_len < remaining.len()
        && chunk_len > 1
        && (0xD800..=0xDBFF).contains(&remaining[chunk_len - 1])
    {
        chunk_len -= 1;
    }
    chunk_len
}

fn last_win32_failure() -> Win32ConsoleFailure {
    let code = unsafe {
        // SAFETY: GetLastError reads thread-local Win32 error state.
        GetLastError()
    };
    Win32ConsoleFailure::Os(code)
}

fn scoped_failure_to_io(failure: ScopedWriteFailure<Win32ConsoleFailure>) -> io::Error {
    let failure = match failure {
        ScopedWriteFailure::Snapshot(failure)
        | ScopedWriteFailure::Enable(failure)
        | ScopedWriteFailure::Write(failure)
        | ScopedWriteFailure::Restore(failure) => failure,
    };
    win32_failure_to_io(failure)
}

fn win32_failure_to_io(failure: Win32ConsoleFailure) -> io::Error {
    match failure {
        Win32ConsoleFailure::Os(code) => io::Error::from_raw_os_error(code as i32),
        Win32ConsoleFailure::WriteZero => io::Error::new(
            io::ErrorKind::WriteZero,
            "WriteConsoleW wrote zero UTF-16 code units",
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;
    use std::time::Duration;

    use windows_sys::Win32::System::Console::{
        ENABLE_MOUSE_INPUT, ENABLE_VIRTUAL_TERMINAL_INPUT, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    use super::super::windows_version::SCOPED_VT_INPUT_MIN_BUILD;
    use super::{
        console_write_chunk_len, coordinated_scoped_write, eligible_console_handles, scoped_write,
        ConsoleHandles, ConsoleIoCoordinator, ScopedConsoleApi, ScopedWriteFailure, WindowsVersion,
        MAX_WRITE_CONSOLE_CODE_UNITS,
    };

    const INPUT: u8 = 1;
    const OUTPUT: u8 = 2;

    #[test]
    fn console_writes_are_bounded_without_splitting_surrogate_pairs() {
        let short = vec![b'x' as u16; 32];
        assert_eq!(console_write_chunk_len(&short), short.len());

        let oversized = vec![b'x' as u16; MAX_WRITE_CONSOLE_CODE_UNITS + 1];
        assert_eq!(
            console_write_chunk_len(&oversized),
            MAX_WRITE_CONSOLE_CODE_UNITS
        );

        let mut boundary_pair = oversized;
        boundary_pair[MAX_WRITE_CONSOLE_CODE_UNITS - 1] = 0xD83D;
        boundary_pair[MAX_WRITE_CONSOLE_CODE_UNITS] = 0xDE00;
        assert_eq!(
            console_write_chunk_len(&boundary_pair),
            MAX_WRITE_CONSOLE_CODE_UNITS - 1
        );
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Event {
        GetMode(u8),
        SetMode(u8, u32),
        Write(u8),
    }

    #[derive(Debug)]
    struct FakeState {
        input_mode: Result<u32, &'static str>,
        output_mode: Result<u32, &'static str>,
        events: Vec<Event>,
        set_calls: usize,
        fail_set_call: Option<usize>,
        fail_write: bool,
        slow_write: bool,
    }

    #[derive(Clone, Debug)]
    struct FakeApi {
        state: Arc<Mutex<FakeState>>,
        stdin: Option<u8>,
        stdout: Option<u8>,
    }

    impl FakeApi {
        fn new(
            input_mode: Result<u32, &'static str>,
            output_mode: Result<u32, &'static str>,
        ) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeState {
                    input_mode,
                    output_mode,
                    events: Vec::new(),
                    set_calls: 0,
                    fail_set_call: None,
                    fail_write: false,
                    slow_write: false,
                })),
                stdin: Some(INPUT),
                stdout: Some(OUTPUT),
            }
        }

        fn events(&self) -> Vec<Event> {
            self.state.lock().expect("fake state").events.clone()
        }

        fn clear_events(&self) {
            self.state.lock().expect("fake state").events.clear();
        }
    }

    impl ScopedConsoleApi for FakeApi {
        type Handle = u8;
        type Error = &'static str;

        fn std_handle(&self, handle_id: u32) -> Result<Option<Self::Handle>, Self::Error> {
            match handle_id {
                STD_INPUT_HANDLE => Ok(self.stdin),
                STD_OUTPUT_HANDLE => Ok(self.stdout),
                _ => Ok(None),
            }
        }

        fn console_mode(&self, handle: Self::Handle) -> Result<u32, Self::Error> {
            let mut state = self.state.lock().expect("fake state");
            state.events.push(Event::GetMode(handle));
            match handle {
                INPUT => state.input_mode,
                OUTPUT => state.output_mode,
                _ => Err("unknown handle"),
            }
        }

        fn set_console_mode(&self, handle: Self::Handle, mode: u32) -> Result<(), Self::Error> {
            let mut state = self.state.lock().expect("fake state");
            state.set_calls += 1;
            state.events.push(Event::SetMode(handle, mode));
            if state.fail_set_call == Some(state.set_calls) {
                return Err("set failed");
            }
            if handle == INPUT {
                state.input_mode = Ok(mode);
            }
            Ok(())
        }

        fn write_console(&self, handle: Self::Handle, _wide: &[u16]) -> Result<(), Self::Error> {
            let (fail, slow) = {
                let mut state = self.state.lock().expect("fake state");
                state.events.push(Event::Write(handle));
                (state.fail_write, state.slow_write)
            };
            if slow {
                thread::sleep(Duration::from_millis(15));
            }
            if fail {
                Err("write failed")
            } else {
                Ok(())
            }
        }
    }

    fn version(build: u32) -> WindowsVersion {
        WindowsVersion {
            major: 10,
            minor: 0,
            build,
        }
    }

    #[test]
    fn eligibility_requires_supported_build_and_two_console_handles() {
        let api = FakeApi::new(Ok(ENABLE_MOUSE_INPUT), Ok(0));
        assert_eq!(eligible_console_handles(&api, None, OUTPUT), None);
        assert_eq!(
            eligible_console_handles(&api, Some(version(19_045)), OUTPUT),
            None
        );
        assert!(api.events().is_empty(), "old builds must not probe handles");

        assert_eq!(
            eligible_console_handles(&api, Some(version(SCOPED_VT_INPUT_MIN_BUILD)), OUTPUT,),
            Some(ConsoleHandles {
                input: INPUT,
                output: OUTPUT,
            })
        );

        let pipe_input = FakeApi::new(Err("pipe"), Ok(0));
        assert_eq!(
            eligible_console_handles(
                &pipe_input,
                Some(version(SCOPED_VT_INPUT_MIN_BUILD)),
                OUTPUT,
            ),
            None
        );
        let pipe_output = FakeApi::new(Ok(0), Err("pipe"));
        assert_eq!(
            eligible_console_handles(
                &pipe_output,
                Some(version(SCOPED_VT_INPUT_MIN_BUILD)),
                OUTPUT,
            ),
            None
        );
        assert_eq!(
            eligible_console_handles(&api, Some(version(SCOPED_VT_INPUT_MIN_BUILD)), 9,),
            None
        );
    }

    #[test]
    fn scoped_write_snapshots_enables_writes_and_restores_exactly() {
        let original = ENABLE_MOUSE_INPUT;
        let api = FakeApi::new(Ok(original), Ok(0));
        let handles = ConsoleHandles {
            input: INPUT,
            output: OUTPUT,
        };
        scoped_write(&api, handles, &[0x1b, b'[' as u16]).expect("scoped write");
        assert_eq!(
            api.events(),
            vec![
                Event::GetMode(INPUT),
                Event::SetMode(INPUT, original | ENABLE_VIRTUAL_TERMINAL_INPUT),
                Event::Write(OUTPUT),
                Event::SetMode(INPUT, original),
            ]
        );
        assert_eq!(
            api.state.lock().expect("fake state").input_mode,
            Ok(original)
        );
    }

    #[test]
    fn write_failure_still_restores_the_snapshot() {
        let api = FakeApi::new(Ok(7), Ok(0));
        api.state.lock().expect("fake state").fail_write = true;
        let error = scoped_write(
            &api,
            ConsoleHandles {
                input: INPUT,
                output: OUTPUT,
            },
            &[1],
        )
        .expect_err("write must fail");
        assert_eq!(error, ScopedWriteFailure::Write("write failed"));
        assert_eq!(api.state.lock().expect("fake state").input_mode, Ok(7));
    }

    #[test]
    fn snapshot_failure_never_changes_mode_or_writes() {
        let api = FakeApi::new(Err("snapshot failed"), Ok(0));
        let error = scoped_write(
            &api,
            ConsoleHandles {
                input: INPUT,
                output: OUTPUT,
            },
            &[1],
        )
        .expect_err("snapshot must fail");
        assert_eq!(error, ScopedWriteFailure::Snapshot("snapshot failed"));
        assert_eq!(api.events(), vec![Event::GetMode(INPUT)]);
    }

    #[test]
    fn enable_failure_never_writes_or_attempts_a_restore() {
        let api = FakeApi::new(Ok(11), Ok(0));
        api.state.lock().expect("fake state").fail_set_call = Some(1);
        let error = scoped_write(
            &api,
            ConsoleHandles {
                input: INPUT,
                output: OUTPUT,
            },
            &[1],
        )
        .expect_err("enable must fail");
        assert_eq!(error, ScopedWriteFailure::Enable("set failed"));
        assert_eq!(
            api.events(),
            vec![
                Event::GetMode(INPUT),
                Event::SetMode(INPUT, 11 | ENABLE_VIRTUAL_TERMINAL_INPUT),
            ]
        );
        assert_eq!(api.state.lock().expect("fake state").input_mode, Ok(11));
    }

    #[test]
    fn restoration_failure_is_fatal_even_when_write_also_fails() {
        let api = FakeApi::new(Ok(9), Ok(0));
        {
            let mut state = api.state.lock().expect("fake state");
            state.fail_write = true;
            state.fail_set_call = Some(2);
        }
        let error = scoped_write(
            &api,
            ConsoleHandles {
                input: INPUT,
                output: OUTPUT,
            },
            &[1],
        )
        .expect_err("restore must fail");
        assert_eq!(error, ScopedWriteFailure::Restore("set failed"));
        assert_eq!(
            api.state.lock().expect("fake state").input_mode,
            Ok(9),
            "RAII fallback retries restoration before the fatal error escapes"
        );
    }

    #[test]
    fn already_enabled_input_mode_is_not_rewritten() {
        let api = FakeApi::new(Ok(3 | ENABLE_VIRTUAL_TERMINAL_INPUT), Ok(0));
        scoped_write(
            &api,
            ConsoleHandles {
                input: INPUT,
                output: OUTPUT,
            },
            &[1],
        )
        .expect("write succeeds");
        assert_eq!(
            api.events(),
            vec![Event::GetMode(INPUT), Event::Write(OUTPUT)]
        );
    }

    #[test]
    fn concurrent_scoped_writes_cannot_interleave_mode_windows() {
        let api = FakeApi::new(Ok(5), Ok(0));
        api.state.lock().expect("fake state").slow_write = true;
        let coordinator = Arc::new(ConsoleIoCoordinator::new());
        let start = Arc::new(Barrier::new(3));
        let handles = ConsoleHandles {
            input: INPUT,
            output: OUTPUT,
        };
        let workers = (0..2)
            .map(|_| {
                let api = api.clone();
                let coordinator = Arc::clone(&coordinator);
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    start.wait();
                    coordinated_scoped_write(&coordinator, &api, handles, &[1])
                        .expect("coordinator")
                        .expect("write");
                })
            })
            .collect::<Vec<_>>();
        start.wait();
        for worker in workers {
            worker.join().expect("worker");
        }

        let events = api.events();
        let group = |offset: usize| &events[offset..offset + 4];
        for offset in [0, 4] {
            assert_eq!(group(offset)[0], Event::GetMode(INPUT));
            assert!(matches!(group(offset)[1], Event::SetMode(INPUT, _)));
            assert_eq!(group(offset)[2], Event::Write(OUTPUT));
            assert_eq!(group(offset)[3], Event::SetMode(INPUT, 5));
        }
        api.clear_events();
    }
}
