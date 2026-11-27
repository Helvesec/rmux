#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use common::{session_name, start_server, TestHarness};
use rmux_client::connect;
use rmux_proto::{
    encode_attach_message, encode_frame, AttachMessage, AttachSessionRequest, AttachedKeystroke,
    NewSessionExtRequest, Request, Response, TerminalSize,
};

#[test]
fn attach_half_close_delivers_complete_input_before_eof() -> Result<(), Box<dyn Error>> {
    complete_input_before_eof(None)
}

#[test]
fn attach_half_close_delivers_complete_prefix_before_truncated_tail() -> Result<(), Box<dyn Error>>
{
    // Partial header, complete header, then a partial payload. None of these
    // tails may prevent the preceding complete input from reaching the pane.
    for tail_len in [3, 5, 6] {
        complete_input_before_eof(Some(tail_len))?;
    }
    Ok(())
}

fn complete_input_before_eof(truncated_tail_len: Option<usize>) -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("half-close");
    let mut server = start_server(&harness)?;
    let mut setup = connect(harness.socket_path())?;
    let cases = [
        ("open", vec![AttachMessage::Data(b"y\r".to_vec())], false),
        ("data", vec![AttachMessage::Data(b"y\r".to_vec())], true),
        (
            "key",
            vec![AttachMessage::Keystroke(AttachedKeystroke::new(
                b"y\r".to_vec(),
            ))],
            true,
        ),
        (
            "coalesced",
            vec![
                AttachMessage::Data(vec![b'y']),
                AttachMessage::Data(vec![b'\r']),
            ],
            true,
        ),
    ];
    let mut observed = Vec::new();
    for (label, messages, half_close) in cases {
        let root = harness.socket_path().parent().unwrap();
        let ready = root.join(format!("{label}.ready"));
        let captured = root.join(format!("{label}.input"));
        let session = session_name(label);
        let created = setup.roundtrip(&Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(vec![
                "/bin/sh".into(),
                "-c".into(),
                format!(
                    "stty raw -echo; : >'{}'; dd bs=1 count=2 of='{}' 2>/dev/null; sleep 10",
                    ready.display(),
                    captured.display(),
                ),
            ]),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))?;
        assert!(matches!(created, Response::NewSession(_)), "{created:?}");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(ready.exists(), "pane must enter raw input mode");

        // The final complete frames and EOF are queued together before the
        // daemon upgrades this connection and starts its attach read burst.
        let mut stream = UnixStream::connect(harness.socket_path())?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut wire = encode_frame(&Request::AttachSession(AttachSessionRequest {
            target: session,
        }))?;
        for message in messages {
            wire.extend(encode_attach_message(&message)?);
        }
        if half_close {
            if let Some(tail_len) = truncated_tail_len {
                let tail = encode_attach_message(&AttachMessage::Data(b"uncommitted".to_vec()))?;
                wire.extend_from_slice(&tail[..tail_len]);
            }
        }
        stream.write_all(&wire)?;
        if half_close {
            stream.shutdown(Shutdown::Write)?;
            let mut output = Vec::new();
            stream.read_to_end(&mut output)?;
            assert!(!output.is_empty(), "daemon must answer the attach request");
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        let input = loop {
            let input = fs::read(&captured).unwrap_or_default();
            if input.len() == 2 || Instant::now() >= deadline {
                break input;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        observed.push((label, input));
    }
    server.shutdown()?;
    assert_eq!(
        observed,
        vec![
            ("open", b"y\r".to_vec()),
            ("data", b"y\r".to_vec()),
            ("key", b"y\r".to_vec()),
            ("coalesced", b"y\r".to_vec())
        ],
        "every complete input frame must reach the pane before attach EOF",
    );
    Ok(())
}
