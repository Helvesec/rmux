mod common;

use std::error::Error;
use std::fs;
use std::io;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use common::{session_name, start_server, TestHarness};
use rmux_client::{attach_with_terminal, connect, drive_attach_stream, AttachTransition};
use rmux_proto::{
    encode_attach_message, encode_frame, AttachFrameDecoder, AttachMessage, AttachSessionRequest,
    AttachSessionResponse, AttachedKeystroke, NewSessionRequest, Request, Response, TerminalSize,
};
use rmux_pty::PtyPair;
use rustix::termios::{
    tcgetattr, tcsetattr, LocalModes, OptionalActions, SpecialCodeIndex, Termios,
};

const READ_TIMEOUT: Duration = Duration::from_secs(1);
const ATTACH_OUTPUT_TIMEOUT: Duration = Duration::from_secs(5);
static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

#[test]
fn begin_attach_upgrades_a_live_connection() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("begin-attach");
    let mut server = start_server(&harness)?;
    let mut connection = connect(harness.socket_path())?;

    let created = connection.roundtrip(&Request::NewSession(NewSessionRequest {
        session_name: session_name("alpha"),
        detached: true,
        size: None,
        environment: None,
    }))?;
    assert!(matches!(created, Response::NewSession(_)));

    let connection = connect(harness.socket_path())?;
    let upgrade = connection.begin_attach(session_name("alpha"))?;

    let attach = match upgrade {
        AttachTransition::Upgraded(attach) => attach,
        other => panic!("unexpected attach transition: {other:?}"),
    };
    let stream = attach.into_stream();
    assert_eq!(stream.read_timeout()?, None);
    assert_eq!(stream.write_timeout()?, None);

    server.shutdown()?;
    Ok(())
}

#[test]
fn drive_attach_stream_forwards_data_and_resize_messages() -> Result<(), Box<dyn Error>> {
    let (client_stream, mut server_stream) = UnixStream::pair()?;
    let (mut input_writer, input_reader) = UnixStream::pair()?;
    let (mut output_reader, output_writer) = UnixStream::pair()?;
    let (resize_tx, resize_rx) = mpsc::channel();
    server_stream.set_read_timeout(Some(READ_TIMEOUT))?;
    output_reader.set_read_timeout(Some(READ_TIMEOUT))?;

    let attach_thread = thread::spawn(move || {
        drive_attach_stream(client_stream, input_reader, output_writer, resize_rx)
    });

    resize_tx.send(TerminalSize { cols: 80, rows: 24 })?;
    input_writer.write_all(b"typed")?;
    input_writer.flush()?;

    let messages = read_attach_messages(&mut server_stream, 2)?;
    assert_eq!(messages.len(), 2);
    assert!(messages.contains(&AttachMessage::Resize(TerminalSize { cols: 80, rows: 24 })));
    assert!(
        messages.contains(&AttachMessage::Keystroke(AttachedKeystroke::new(
            b"typed".to_vec()
        )))
    );

    let frame = encode_attach_message(&AttachMessage::Data(b"screen".to_vec()))?;
    server_stream.write_all(&frame)?;
    let mut stdout = [0_u8; 6];
    output_reader.read_exact(&mut stdout)?;
    assert_eq!(&stdout, b"screen");

    resize_tx.send(TerminalSize {
        cols: 120,
        rows: 40,
    })?;
    assert_eq!(
        read_attach_messages(&mut server_stream, 1)?,
        vec![AttachMessage::Resize(TerminalSize {
            cols: 120,
            rows: 40,
        })]
    );

    write_attach_stop(&mut server_stream)?;
    drop(input_writer);
    drop(server_stream);
    attach_thread
        .join()
        .map_err(|_| std::io::Error::other("attach thread panicked"))??;
    Ok(())
}

#[test]
fn drive_attach_stream_exits_when_the_server_stops_with_input_still_open(
) -> Result<(), Box<dyn Error>> {
    let (client_stream, mut server_stream) = UnixStream::pair()?;
    let (_input_writer, input_reader) = UnixStream::pair()?;
    let (_output_reader, output_writer) = UnixStream::pair()?;
    let (_resize_tx, resize_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();

    thread::spawn(move || {
        let result = drive_attach_stream(client_stream, input_reader, output_writer, resize_rx);
        let _ = result_tx.send(result);
    });

    write_attach_stop(&mut server_stream)?;
    drop(server_stream);
    let attach_result = result_rx
        .recv_timeout(READ_TIMEOUT)
        .map_err(|error| io::Error::new(io::ErrorKind::TimedOut, error))?;
    attach_result?;
    Ok(())
}

#[test]
fn begin_attach_keeps_post_response_attach_bytes_on_the_stream() -> Result<(), Box<dyn Error>> {
    let socket_path = unique_socket_path("begin-attach-boundary");
    let listener = UnixListener::bind(&socket_path)?;
    let (release_server_tx, release_server_rx) = mpsc::channel();
    let expected_request = Request::AttachSession(AttachSessionRequest {
        target: session_name("alpha"),
    });

    let server = thread::spawn(move || -> Result<(), String> {
        let (mut stream, _addr) = listener.accept().map_err(|error| error.to_string())?;
        assert_eq!(
            read_request(&mut stream).map_err(|error| error.to_string())?,
            expected_request
        );

        let mut response = encode_frame(&Response::AttachSession(AttachSessionResponse {
            session_name: session_name("alpha"),
        }))
        .map_err(|error| error.to_string())?;
        response.extend_from_slice(
            &encode_attach_message(&AttachMessage::Data(b"ready".to_vec()))
                .map_err(|error| error.to_string())?,
        );
        stream
            .write_all(&response)
            .and_then(|()| stream.flush())
            .map_err(|error| error.to_string())?;
        release_server_rx
            .recv()
            .map_err(|error| error.to_string())?;
        Ok(())
    });

    let connection = connect(&socket_path)?;
    let upgrade = connection.begin_attach(session_name("alpha"))?;
    let attach = match upgrade {
        AttachTransition::Upgraded(attach) => attach,
        other => panic!("unexpected attach transition: {other:?}"),
    };
    let mut stream = attach.into_stream();
    stream.set_read_timeout(Some(READ_TIMEOUT))?;

    assert_eq!(
        read_attach_messages(&mut stream, 1)?,
        vec![AttachMessage::Data(b"ready".to_vec())]
    );

    release_server_tx
        .send(())
        .map_err(|_| std::io::Error::other("server release receiver closed"))?;
    server
        .join()
        .map_err(|_| std::io::Error::other("server thread panicked"))?
        .map_err(std::io::Error::other)?;
    fs::remove_file(&socket_path)?;
    Ok(())
}

#[test]
fn attach_with_terminal_restores_termios_after_repeated_detach() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("attach-lifecycle");
    let mut server = start_server(&harness)?;
    let mut setup_connection = connect(harness.socket_path())?;

    for session in ["alpha", "beta"] {
        let created = setup_connection.roundtrip(&Request::NewSession(NewSessionRequest {
            session_name: session_name(session),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))?;
        assert!(matches!(created, Response::NewSession(_)));
    }

    let terminal_pair = PtyPair::open()?;
    let terminal = terminal_pair.slave().try_clone()?;
    let original_termios = prepare_canonical_termios(&terminal)?;

    run_attach_cycle(harness.socket_path(), &terminal)?;
    assert_termios_eq(&original_termios, &tcgetattr(&terminal)?);

    run_attach_cycle(harness.socket_path(), &terminal)?;
    assert_termios_eq(&original_termios, &tcgetattr(&terminal)?);

    server.shutdown()?;
    Ok(())
}

fn read_attach_messages(
    stream: &mut UnixStream,
    count: usize,
) -> Result<Vec<AttachMessage>, Box<dyn Error>> {
    let mut decoder = AttachFrameDecoder::new();
    let mut buffer = [0_u8; 256];
    let mut messages = Vec::with_capacity(count);

    loop {
        if let Some(message) = decoder.next_message()? {
            messages.push(message);
            if messages.len() == count {
                return Ok(messages);
            }

            continue;
        }

        let bytes_read = stream.read(&mut buffer)?;
        if bytes_read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!(
                    "attach stream closed after {} of {count} messages",
                    messages.len()
                ),
            )
            .into());
        }

        decoder.push_bytes(&buffer[..bytes_read]);
    }
}

fn read_request(stream: &mut UnixStream) -> Result<Request, Box<dyn Error>> {
    let mut decoder = rmux_proto::FrameDecoder::new();
    let mut buffer = [0_u8; 256];

    loop {
        if let Some(request) = decoder.next_frame()? {
            return Ok(request);
        }

        let bytes_read = stream.read(&mut buffer)?;
        if bytes_read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed before a request frame arrived",
            )
            .into());
        }

        decoder.push_bytes(&buffer[..bytes_read]);
    }
}

fn write_attach_stop(stream: &mut UnixStream) -> Result<(), Box<dyn Error>> {
    stream.write_all(&encode_attach_message(&AttachMessage::Data(
        b"\x1b[?1049l".to_vec(),
    ))?)?;
    stream.flush()?;
    Ok(())
}

fn unique_socket_path(label: &str) -> PathBuf {
    let unique_id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "rmux-client-test-{label}-{}-{unique_id}.sock",
        std::process::id()
    ))
}

fn run_attach_cycle(
    socket_path: &std::path::Path,
    terminal: &rmux_pty::PtySlave,
) -> Result<(), Box<dyn Error>> {
    let connection = connect(socket_path)?;
    let upgrade = connection.begin_attach(session_name("alpha"))?;
    let attach = match upgrade {
        AttachTransition::Upgraded(attach) => attach,
        other => panic!("unexpected attach transition: {other:?}"),
    };
    let stream = attach.into_stream();
    let (mut input_writer, input_reader) = UnixStream::pair()?;
    let (mut output_reader, output_writer) = UnixStream::pair()?;
    output_reader.set_read_timeout(Some(Duration::from_millis(200)))?;

    let terminal_for_thread = terminal.try_clone()?;
    let attach_thread = thread::spawn(move || {
        attach_with_terminal(stream, &terminal_for_thread, input_reader, output_writer)
    });

    wait_for_raw_mode(terminal)?;
    wait_for_attach_output(&mut output_reader, ATTACH_OUTPUT_TIMEOUT)?;
    input_writer.write_all(b"\x02d")?;
    input_writer.flush()?;

    let attach_result = attach_thread
        .join()
        .map_err(|_| io::Error::other("attach thread panicked"))?;
    attach_result?;
    wait_for_output_eof(&mut output_reader, Duration::from_secs(1))?;
    drop(input_writer);
    Ok(())
}

fn wait_for_attach_output(
    reader: &mut UnixStream,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut output_buffer = [0_u8; 256];

    loop {
        match reader.read(&mut output_buffer) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "attach output closed before the first render",
                )
                .into());
            }
            Ok(_) => return Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) && Instant::now() < deadline => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out waiting for attach output",
                )
                .into());
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn prepare_canonical_termios<Fd>(fd: &Fd) -> Result<Termios, Box<dyn Error>>
where
    Fd: std::os::fd::AsFd,
{
    let mut termios = tcgetattr(fd)?;
    termios.local_modes |= LocalModes::ICANON | LocalModes::ECHO;
    termios.special_codes[SpecialCodeIndex::VMIN] = 4;
    termios.special_codes[SpecialCodeIndex::VTIME] = 9;
    tcsetattr(fd, OptionalActions::Now, &termios)?;
    Ok(tcgetattr(fd)?)
}

fn wait_for_raw_mode<Fd>(fd: &Fd) -> Result<(), Box<dyn Error>>
where
    Fd: std::os::fd::AsFd,
{
    for _ in 0..20 {
        let termios = tcgetattr(fd)?;
        if !termios.local_modes.contains(LocalModes::ICANON)
            && !termios.local_modes.contains(LocalModes::ECHO)
        {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }

    Err(io::Error::new(io::ErrorKind::TimedOut, "terminal never entered raw mode").into())
}

fn assert_termios_eq(expected: &Termios, actual: &Termios) {
    assert_eq!(actual.input_modes, expected.input_modes);
    assert_eq!(actual.output_modes, expected.output_modes);
    assert_eq!(actual.control_modes, expected.control_modes);
    assert_eq!(
        comparable_local_modes(actual.local_modes),
        comparable_local_modes(expected.local_modes)
    );
    #[cfg(target_os = "linux")]
    assert_eq!(actual.line_discipline, expected.line_discipline);
    assert_eq!(
        format!("{:?}", actual.special_codes),
        format!("{:?}", expected.special_codes)
    );
    assert_eq!(actual.input_speed(), expected.input_speed());
    assert_eq!(actual.output_speed(), expected.output_speed());
}

fn comparable_local_modes(mut modes: LocalModes) -> LocalModes {
    #[cfg(target_os = "macos")]
    modes.remove(LocalModes::PENDIN);

    modes
}

fn wait_for_output_eof(reader: &mut UnixStream, timeout: Duration) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut output_buffer = [0_u8; 256];

    loop {
        match reader.read(&mut output_buffer) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) && Instant::now() < deadline => {}
            Err(error) => return Err(error.into()),
        }
    }
}
