#[cfg(windows)]
use std::ffi::OsString;
#[cfg(windows)]
use std::fs::File;
#[cfg(windows)]
use std::io;
#[cfg(windows)]
use std::io::Read;
#[cfg(windows)]
use std::mem::size_of;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
#[cfg(windows)]
use std::ptr::{null, null_mut};
#[cfg(windows)]
use std::time::{Duration, Instant};

#[cfg(windows)]
use tokio::io::AsyncReadExt;
#[cfg(windows)]
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, PipeMode, ServerOptions,
};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{GetLastError, HANDLE, S_OK, WAIT_FAILED, WAIT_OBJECT_0};
#[cfg(windows)]
use windows_sys::Win32::System::Console::{ClosePseudoConsole, CreatePseudoConsole, COORD, HPCON};
#[cfg(windows)]
use windows_sys::Win32::System::Pipes::CreatePipe;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetExitCodeProcess,
    GetProcessHandleCount, InitializeProcThreadAttributeList, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
    STARTUPINFOEXW, STARTUPINFOW,
};

#[cfg(not(windows))]
fn main() {
    eprintln!("conpty_spike is only available on Windows");
}

#[cfg(windows)]
#[tokio::main(flavor = "multi_thread")]
async fn main() -> io::Result<()> {
    let args = Args::parse();
    match args.mode {
        Mode::TokioNamedPipe => run_tokio_named_pipe(args).await,
        Mode::BlockingAnonymousPipe => run_blocking_anonymous_pipe(args).await,
    }
}

#[cfg(windows)]
async fn run_tokio_named_pipe(args: Args) -> io::Result<()> {
    let before_handles = current_process_handle_count()?;
    let started = Instant::now();
    let mut conpty = TokioNamedPipeConpty::spawn(args.cols, args.rows, args.lines).await?;
    let launch_elapsed = started.elapsed();

    let mut first_byte_latency = None;
    let mut bytes_read = 0_usize;
    let mut output = Vec::with_capacity(64 * 1024);
    let mut buffer = [0_u8; 8192];
    let read_started = Instant::now();

    loop {
        let read =
            match tokio::time::timeout(Duration::from_millis(750), conpty.output.read(&mut buffer))
                .await
            {
                Ok(result) => result?,
                Err(_) => break,
            };
        if read == 0 {
            break;
        }
        if first_byte_latency.is_none() {
            first_byte_latency = Some(read_started.elapsed());
        }
        bytes_read += read;
        output.extend_from_slice(&buffer[..read]);
    }

    let read_elapsed = read_started.elapsed();
    let throughput_mib_s = mib_per_second(bytes_read, read_elapsed);
    let status = conpty.wait()?;
    drop(conpty);
    let after_handles = current_process_handle_count()?;

    println!("rmux ConPTY spike");
    println!("  mode: tokio-named-pipe-overlapped");
    println!("  command: cmd.exe");
    println!("  lines requested: {}", args.lines);
    println!("  exit code: {}", status);
    println!("  launch_ms: {:.3}", millis(launch_elapsed));
    println!(
        "  first_byte_ms: {:.3}",
        millis(first_byte_latency.unwrap_or_default())
    );
    println!("  read_ms: {:.3}", millis(read_elapsed));
    println!("  bytes: {bytes_read}");
    println!("  throughput_mib_s: {throughput_mib_s:.3}");
    println!(
        "  handle_delta: {}",
        i64::from(after_handles) - i64::from(before_handles)
    );
    println!(
        "  saw_ready_marker: {}",
        String::from_utf8_lossy(&output).contains("RMUX_CONPTY_READY")
    );

    Ok(())
}

#[cfg(windows)]
async fn run_blocking_anonymous_pipe(args: Args) -> io::Result<()> {
    let before_handles = current_process_handle_count()?;
    let started = Instant::now();
    let mut conpty = BlockingAnonymousPipeConpty::spawn(args.cols, args.rows, args.lines)?;
    let launch_elapsed = started.elapsed();

    let read_started = Instant::now();
    let output_reader = conpty.take_output_reader()?;
    let reader = tokio::task::spawn_blocking(move || read_to_end(output_reader, read_started));
    let status = conpty.wait()?;
    drop(conpty);

    let (output, first_byte_latency, read_elapsed) =
        match tokio::time::timeout(Duration::from_secs(5), reader).await {
            Ok(join_result) => join_result.map_err(join_error)??,
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "blocking reader did not finish within spike timeout",
                ));
            }
        };

    let bytes_read = output.len();
    let throughput_mib_s = mib_per_second(bytes_read, read_elapsed);
    let after_handles = current_process_handle_count()?;

    println!("rmux ConPTY spike");
    println!("  mode: blocking-anonymous-pipe");
    println!("  command: cmd.exe");
    println!("  lines requested: {}", args.lines);
    println!("  exit code: {}", status);
    println!("  launch_ms: {:.3}", millis(launch_elapsed));
    println!(
        "  first_byte_ms: {:.3}",
        millis(first_byte_latency.unwrap_or_default())
    );
    println!("  read_ms: {:.3}", millis(read_elapsed));
    println!("  bytes: {bytes_read}");
    println!("  throughput_mib_s: {throughput_mib_s:.3}");
    println!(
        "  handle_delta: {}",
        i64::from(after_handles) - i64::from(before_handles)
    );
    println!(
        "  saw_ready_marker: {}",
        String::from_utf8_lossy(&output).contains("RMUX_CONPTY_READY")
    );

    Ok(())
}

#[cfg(windows)]
#[derive(Debug)]
struct Args {
    mode: Mode,
    lines: u32,
    cols: i16,
    rows: i16,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug)]
enum Mode {
    TokioNamedPipe,
    BlockingAnonymousPipe,
}

#[cfg(windows)]
impl Args {
    fn parse() -> Self {
        let mut mode = Mode::TokioNamedPipe;
        let mut lines = 10_000;
        let mut cols = 120;
        let mut rows = 40;
        let mut args = std::env::args().skip(1);

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--mode" => mode = parse_mode(&mut args),
                "--lines" => lines = parse_next(&mut args, "--lines"),
                "--cols" => cols = parse_next(&mut args, "--cols"),
                "--rows" => rows = parse_next(&mut args, "--rows"),
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                _ => {
                    eprintln!("unknown argument: {arg}");
                    print_usage();
                    std::process::exit(2);
                }
            }
        }

        Self {
            mode,
            lines,
            cols,
            rows,
        }
    }
}

#[cfg(windows)]
struct TokioNamedPipeConpty {
    #[allow(dead_code)]
    hpc: OwnedHpcon,
    #[allow(dead_code)]
    attributes: AttributeList,
    process: OwnedHandle,
    #[allow(dead_code)]
    thread: OwnedHandle,
    #[allow(dead_code)]
    input_server: NamedPipeServer,
    #[allow(dead_code)]
    output_server: NamedPipeServer,
    #[allow(dead_code)]
    input: NamedPipeClient,
    output: NamedPipeClient,
}

#[cfg(windows)]
impl TokioNamedPipeConpty {
    async fn spawn(cols: i16, rows: i16, lines: u32) -> io::Result<Self> {
        let input = anonymous_overlapped_pipe(PipeDirection::ClientToServer)?;
        let output = anonymous_overlapped_pipe(PipeDirection::ServerToClient)?;
        input.server.connect().await?;
        output.server.connect().await?;
        let hpc = OwnedHpcon::create(
            COORD { X: cols, Y: rows },
            input.server.as_raw_handle() as HANDLE,
            output.server.as_raw_handle() as HANDLE,
        )?;
        let mut attributes = AttributeList::with_pseudoconsole(hpc.raw())?;
        let (process, thread) = spawn_cmd(attributes.as_mut_ptr(), lines)?;
        Ok(Self {
            hpc,
            attributes,
            process,
            thread,
            input_server: input.server,
            output_server: output.server,
            input: input.client,
            output: output.client,
        })
    }

    fn wait(&mut self) -> io::Result<u32> {
        let wait = unsafe { WaitForSingleObject(self.process.as_raw_handle() as HANDLE, 5_000) };
        if wait == WAIT_FAILED {
            return Err(last_os_error());
        }
        if wait != WAIT_OBJECT_0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "ConPTY child did not exit within spike timeout",
            ));
        }

        let mut exit_code = 0_u32;
        let ok =
            unsafe { GetExitCodeProcess(self.process.as_raw_handle() as HANDLE, &mut exit_code) };
        if ok == 0 {
            return Err(last_os_error());
        }
        Ok(exit_code)
    }
}

#[cfg(windows)]
struct BlockingAnonymousPipeConpty {
    #[allow(dead_code)]
    hpc: OwnedHpcon,
    #[allow(dead_code)]
    attributes: AttributeList,
    process: OwnedHandle,
    #[allow(dead_code)]
    thread: OwnedHandle,
    #[allow(dead_code)]
    input_writer: OwnedHandle,
    output_reader: Option<File>,
}

#[cfg(windows)]
impl BlockingAnonymousPipeConpty {
    fn spawn(cols: i16, rows: i16, lines: u32) -> io::Result<Self> {
        let input = anonymous_blocking_pipe()?;
        let output = anonymous_blocking_pipe()?;
        let hpc = OwnedHpcon::create(
            COORD { X: cols, Y: rows },
            input.read.as_raw_handle() as HANDLE,
            output.write.as_raw_handle() as HANDLE,
        )?;
        drop(input.read);
        drop(output.write);

        let mut attributes = AttributeList::with_pseudoconsole(hpc.raw())?;
        let (process, thread) = spawn_cmd(attributes.as_mut_ptr(), lines)?;
        Ok(Self {
            hpc,
            attributes,
            process,
            thread,
            input_writer: input.write,
            output_reader: Some(output.read.into()),
        })
    }

    fn take_output_reader(&mut self) -> io::Result<File> {
        self.output_reader
            .take()
            .ok_or_else(|| io::Error::other("output reader already taken"))
    }

    fn wait(&mut self) -> io::Result<u32> {
        let wait = unsafe { WaitForSingleObject(self.process.as_raw_handle() as HANDLE, 5_000) };
        if wait == WAIT_FAILED {
            return Err(last_os_error());
        }
        if wait != WAIT_OBJECT_0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "ConPTY child did not exit within spike timeout",
            ));
        }

        let mut exit_code = 0_u32;
        let ok =
            unsafe { GetExitCodeProcess(self.process.as_raw_handle() as HANDLE, &mut exit_code) };
        if ok == 0 {
            return Err(last_os_error());
        }
        Ok(exit_code)
    }
}

#[cfg(windows)]
struct BlockingPipePair {
    read: OwnedHandle,
    write: OwnedHandle,
}

#[cfg(windows)]
fn anonymous_blocking_pipe() -> io::Result<BlockingPipePair> {
    let mut read = null_mut();
    let mut write = null_mut();
    let created = unsafe { CreatePipe(&mut read, &mut write, null(), 64 * 1024) };
    if created == 0 {
        return Err(last_os_error());
    }

    let read = unsafe { OwnedHandle::from_raw_handle(read as _) };
    let write = unsafe { OwnedHandle::from_raw_handle(write as _) };
    Ok(BlockingPipePair { read, write })
}

#[cfg(windows)]
struct PipePair {
    server: NamedPipeServer,
    client: NamedPipeClient,
}

#[cfg(windows)]
#[derive(Clone, Copy)]
enum PipeDirection {
    ClientToServer,
    ServerToClient,
}

#[cfg(windows)]
fn anonymous_overlapped_pipe(direction: PipeDirection) -> io::Result<PipePair> {
    let name = format!(
        r"\\.\pipe\rmux-conpty-spike-{}-{}",
        std::process::id(),
        unique_suffix()
    );
    let mut options = ServerOptions::new();
    options
        .pipe_mode(PipeMode::Byte)
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .in_buffer_size(64 * 1024)
        .out_buffer_size(64 * 1024);

    match direction {
        PipeDirection::ClientToServer => {
            options.access_inbound(true).access_outbound(false);
        }
        PipeDirection::ServerToClient => {
            options.access_inbound(false).access_outbound(true);
        }
    }

    let server = options.create(&name)?;
    let mut client_options = ClientOptions::new();
    client_options.pipe_mode(PipeMode::Byte);
    match direction {
        PipeDirection::ClientToServer => {
            client_options.read(false).write(true);
        }
        PipeDirection::ServerToClient => {
            client_options.read(true).write(false);
        }
    }
    let client = client_options.open(&name)?;
    Ok(PipePair { server, client })
}

#[cfg(windows)]
struct OwnedHpcon(HPCON);

#[cfg(windows)]
impl OwnedHpcon {
    fn create(size: COORD, input: HANDLE, output: HANDLE) -> io::Result<Self> {
        let mut hpc = 0_isize;
        let hr = unsafe { CreatePseudoConsole(size, input, output, 0, &mut hpc) };
        if hr != S_OK {
            return Err(hresult_error(hr));
        }
        Ok(Self(hpc))
    }

    fn raw(&self) -> HPCON {
        self.0
    }
}

#[cfg(windows)]
impl Drop for OwnedHpcon {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe { ClosePseudoConsole(self.0) };
        }
    }
}

#[cfg(windows)]
struct AttributeList {
    storage: Vec<usize>,
}

#[cfg(windows)]
impl AttributeList {
    fn with_pseudoconsole(hpc: HPCON) -> io::Result<Self> {
        let mut size = 0_usize;
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size);
        }
        if size == 0 {
            return Err(last_os_error());
        }

        let slots = size.div_ceil(size_of::<usize>());
        let mut storage = vec![0_usize; slots];
        let list = storage.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        let initialized = unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) };
        if initialized == 0 {
            return Err(last_os_error());
        }

        let updated = unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                &hpc as *const HPCON as *const _,
                size_of::<HPCON>(),
                null_mut(),
                null(),
            )
        };
        if updated == 0 {
            unsafe { DeleteProcThreadAttributeList(list) };
            return Err(last_os_error());
        }

        Ok(Self { storage })
    }

    fn as_mut_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST
    }
}

#[cfg(windows)]
impl Drop for AttributeList {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.as_mut_ptr()) };
    }
}

#[cfg(windows)]
fn spawn_cmd(
    attributes: LPPROC_THREAD_ATTRIBUTE_LIST,
    lines: u32,
) -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.lpAttributeList = attributes;

    let mut process_info = PROCESS_INFORMATION::default();
    let cmd_path = std::env::var_os("COMSPEC")
        .unwrap_or_else(|| OsString::from("C:\\Windows\\System32\\cmd.exe"));
    let command = format!(
        "\"{}\" /C \"echo RMUX_CONPTY_READY&for /L %i in (1,1,{lines}) do @echo rmux-spike-%i\"",
        cmd_path.to_string_lossy()
    );
    let program = wide_null(cmd_path);
    let mut command_line = wide_null(OsString::from(command));
    let created = unsafe {
        CreateProcessW(
            program.as_ptr(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            null(),
            null(),
            &startup.StartupInfo as *const STARTUPINFOW,
            &mut process_info,
        )
    };
    if created == 0 {
        return Err(last_os_error());
    }

    let process = unsafe { OwnedHandle::from_raw_handle(process_info.hProcess as _) };
    let thread = unsafe { OwnedHandle::from_raw_handle(process_info.hThread as _) };
    Ok((process, thread))
}

#[cfg(windows)]
fn current_process_handle_count() -> io::Result<u32> {
    let mut count = 0_u32;
    let ok = unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) };
    if ok == 0 {
        return Err(last_os_error());
    }
    Ok(count)
}

#[cfg(windows)]
fn parse_next<T>(args: &mut impl Iterator<Item = String>, name: &str) -> T
where
    T: std::str::FromStr,
{
    args.next()
        .unwrap_or_else(|| {
            eprintln!("missing value for {name}");
            std::process::exit(2);
        })
        .parse()
        .unwrap_or_else(|_| {
            eprintln!("invalid value for {name}");
            std::process::exit(2);
        })
}

#[cfg(windows)]
fn parse_mode(args: &mut impl Iterator<Item = String>) -> Mode {
    let value = args.next().unwrap_or_else(|| {
        eprintln!("missing value for --mode");
        std::process::exit(2);
    });

    match value.as_str() {
        "tokio-named-pipe-overlapped" => Mode::TokioNamedPipe,
        "blocking-anonymous-pipe" => Mode::BlockingAnonymousPipe,
        _ => {
            eprintln!("invalid mode: {value}");
            print_usage();
            std::process::exit(2);
        }
    }
}

#[cfg(windows)]
fn print_usage() {
    eprintln!(
        "usage: cargo run -p rmux-pty --example conpty_spike -- [--mode MODE] [--lines N] [--cols N] [--rows N]\n\nmodes:\n  tokio-named-pipe-overlapped\n  blocking-anonymous-pipe"
    );
}

#[cfg(windows)]
fn read_to_end(
    mut output_reader: File,
    read_started: Instant,
) -> io::Result<(Vec<u8>, Option<Duration>, Duration)> {
    let mut first_byte_latency = None;
    let mut output = Vec::with_capacity(64 * 1024);
    let mut buffer = [0_u8; 8192];

    loop {
        let read = output_reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        if first_byte_latency.is_none() {
            first_byte_latency = Some(read_started.elapsed());
        }
        output.extend_from_slice(&buffer[..read]);
    }

    Ok((output, first_byte_latency, read_started.elapsed()))
}

#[cfg(windows)]
fn join_error(error: tokio::task::JoinError) -> io::Error {
    io::Error::other(format!("blocking reader task failed: {error}"))
}

#[cfg(windows)]
fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[cfg(windows)]
fn wide_null(value: OsString) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

#[cfg(windows)]
fn mib_per_second(bytes: usize, elapsed: Duration) -> f64 {
    if elapsed.is_zero() {
        return 0.0;
    }
    bytes as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64()
}

#[cfg(windows)]
fn hresult_error(hr: i32) -> io::Error {
    io::Error::from_raw_os_error(hr)
}

#[cfg(windows)]
fn last_os_error() -> io::Error {
    let code = unsafe { GetLastError() };
    io::Error::from_raw_os_error(code as i32)
}
