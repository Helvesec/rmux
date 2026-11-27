// An example root resolves `mod` beside itself, like a crate root. The inline
// module keeps its children under the example's own directory, in normal layout.
#[cfg(windows)]
mod conpty_spike {
    pub(crate) mod cli;
    pub(crate) mod win32;
}

#[cfg(windows)]
use std::fs::File;
#[cfg(windows)]
use std::io;
#[cfg(windows)]
use std::io::Read;
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, OwnedHandle};
#[cfg(windows)]
use std::time::{Duration, Instant};

#[cfg(windows)]
use tokio::io::AsyncReadExt;
#[cfg(windows)]
use tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(windows)]
use windows_sys::Win32::Foundation::HANDLE;
#[cfg(windows)]
use windows_sys::Win32::System::Console::COORD;

#[cfg(windows)]
use conpty_spike::cli::{emit_payload, emit_payload_arg, Args, Mode};
#[cfg(windows)]
use conpty_spike::win32::{
    anonymous_blocking_pipe, anonymous_overlapped_pipe, current_process_handle_count, spawn_child,
    spawn_cmd_control, wait_for_process, AttributeList, OwnedHpcon, PipeDirection,
};

#[cfg(not(windows))]
fn main() {
    eprintln!("conpty_spike is only available on Windows");
}

#[cfg(windows)]
#[tokio::main(flavor = "multi_thread")]
async fn main() -> io::Result<()> {
    if let Some(bytes) = emit_payload_arg() {
        return emit_payload(bytes);
    }

    let args = Args::parse();
    match args.mode {
        Mode::TokioNamedPipe => run_tokio_named_pipe(args).await,
        Mode::BlockingAnonymousPipe => run_blocking_anonymous_pipe(args).await,
        Mode::PlainCreateProcess => run_plain_create_process(args),
        Mode::PlainStartupInfoEx => run_plain_startupinfoex(args),
    }
}

#[cfg(windows)]
async fn run_tokio_named_pipe(args: Args) -> io::Result<()> {
    let before_handles = current_process_handle_count()?;
    let started = Instant::now();
    let mut conpty =
        TokioNamedPipeConpty::spawn(args.cols, args.rows, args.lines, args.payload_bytes).await?;
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
    print_payload(&args);
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
    println!("  output_preview: {}", preview_bytes(&output));

    Ok(())
}

#[cfg(windows)]
async fn run_blocking_anonymous_pipe(args: Args) -> io::Result<()> {
    let before_handles = current_process_handle_count()?;
    let started = Instant::now();
    let mut conpty =
        BlockingAnonymousPipeConpty::spawn(args.cols, args.rows, args.lines, args.payload_bytes)?;
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
    print_payload(&args);
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
    println!("  output_preview: {}", preview_bytes(&output));

    Ok(())
}

#[cfg(windows)]
fn run_plain_create_process(args: Args) -> io::Result<()> {
    run_process_control("plain-create-process", args, false)
}

#[cfg(windows)]
fn run_plain_startupinfoex(args: Args) -> io::Result<()> {
    run_process_control("plain-startupinfoex", args, true)
}

#[cfg(windows)]
fn run_process_control(mode: &str, args: Args, extended_startup: bool) -> io::Result<()> {
    let before_handles = current_process_handle_count()?;
    let started = Instant::now();
    let (process, _thread) = spawn_cmd_control(args.lines, args.payload_bytes, extended_startup)?;
    let launch_elapsed = started.elapsed();
    let status = wait_for_process(&process)?;
    drop(process);
    let after_handles = current_process_handle_count()?;

    println!("rmux ConPTY spike");
    println!("  mode: {mode}");
    print_payload(&args);
    println!("  exit code: {}", status);
    println!("  launch_ms: {:.3}", millis(launch_elapsed));
    println!(
        "  handle_delta: {}",
        i64::from(after_handles) - i64::from(before_handles)
    );

    Ok(())
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
    input: NamedPipeClient,
    output: NamedPipeClient,
}

#[cfg(windows)]
impl TokioNamedPipeConpty {
    async fn spawn(
        cols: i16,
        rows: i16,
        lines: u32,
        payload_bytes: Option<usize>,
    ) -> io::Result<Self> {
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
        let (process, thread) = spawn_child(attributes.as_mut_ptr(), lines, payload_bytes)?;
        Ok(Self {
            hpc,
            attributes,
            process,
            thread,
            input: input.client,
            output: output.client,
        })
    }

    fn wait(&mut self) -> io::Result<u32> {
        wait_for_process(&self.process)
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
    fn spawn(cols: i16, rows: i16, lines: u32, payload_bytes: Option<usize>) -> io::Result<Self> {
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
        let (process, thread) = spawn_child(attributes.as_mut_ptr(), lines, payload_bytes)?;
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
        wait_for_process(&self.process)
    }
}

#[cfg(windows)]
fn print_payload(args: &Args) {
    if let Some(bytes) = args.payload_bytes {
        println!("  command: conpty_spike.exe --emit-payload");
        println!("  payload_bytes requested: {bytes}");
    } else {
        println!("  command: cmd.exe");
        println!("  lines requested: {}", args.lines);
    }
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
fn preview_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(160)
        .flat_map(|byte| std::ascii::escape_default(*byte))
        .map(char::from)
        .collect()
}
