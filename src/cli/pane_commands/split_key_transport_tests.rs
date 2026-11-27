use super::*;
#[cfg(not(target_has_atomic = "64"))]
use portable_atomic::AtomicU64;
use rmux_proto::{encode_frame, ErrorResponse, FrameDecoder, Request, RmuxError};
use std::io::Read;
use std::os::unix::net::UnixListener;
#[cfg(target_has_atomic = "64")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

static NEXT: AtomicU64 = AtomicU64::new(0);

fn split_args(arguments: &[&str]) -> SplitWindowArgs {
    let cli = crate::cli_args::parse(
        ["rmux", "split-window"]
            .into_iter()
            .chain(arguments.iter().copied()),
    )
    .unwrap();
    let Some(crate::cli_args::Command::SplitWindow(args)) = cli.command else {
        panic!("split arguments")
    };
    args
}

fn exchange(response: Option<Vec<u8>>) -> Result<i32, ExitFailure> {
    let path = std::env::temp_dir().join(format!(
        "sc2-wire-{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let listener = UnixListener::bind(&path).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut decoder = FrameDecoder::new();
        let mut bytes = [0; 8192];
        let request = loop {
            if let Some(request) = decoder.next_frame::<Request>().unwrap() {
                break request;
            }
            let n = stream.read(&mut bytes).unwrap();
            assert!(n > 0);
            decoder.push_bytes(&bytes[..n]);
        };
        let Request::SplitWindowCommandV1(offer) = request else {
            panic!("must not send old offer or bool request: {request:?}")
        };
        assert_eq!(offer.arguments, ["split-window", "-k"]);
        assert_eq!(
            offer.handshake.required_capabilities,
            [rmux_proto::CAPABILITY_SPLIT_WINDOW_KEY_INPUT_V1]
        );
        if let Some(response) = response {
            stream.write_all(&response).unwrap();
        }
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        assert_eq!(
            stream.read(&mut bytes).unwrap(),
            0,
            "no additional request after result"
        );
        listener.set_nonblocking(true).unwrap();
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "no retry connection"
        );
    });
    let args = split_args(&["-k"]);
    let result = run(args, &path);
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
    result
}

#[test]
fn sc2_transport_refusal_ambiguity_and_post_effect_error_never_replay() {
    for response in [
        Response::Error(ErrorResponse {
            error: RmuxError::Decode(
                "invalid value: integer `132`, expected variant index 0 <= i < 132".into(),
            ),
        }),
        Response::SplitWindowCommandV1(Box::new(SplitWindowCommandV1Response::NotAdmitted {
            error: RmuxError::Server("missing capability".into()),
        })),
        Response::SplitWindowCommandV1(Box::new(SplitWindowCommandV1Response::Complete {
            stdout: vec![],
            error: Some(RmuxError::Server("failure after commit".into())),
            exit_status: 1,
        })),
    ] {
        assert!(exchange(Some(encode_frame(&response).unwrap())).is_err());
    }
    assert!(exchange(None).is_err());
    assert!(exchange(Some(vec![rmux_proto::RMUX_FRAME_MAGIC, 9, 20, 0])).is_err());
    let mut old = encode_frame(&Response::SplitWindowCommandV1(Box::new(
        SplitWindowCommandV1Response::Complete {
            stdout: vec![],
            error: None,
            exit_status: 0,
        },
    )))
    .unwrap();
    old[1] = 8;
    assert!(exchange(Some(old)).is_err());
}

#[test]
fn sc2_transport_success_is_one_offer_and_one_terminal_result() {
    let response =
        Response::SplitWindowCommandV1(Box::new(SplitWindowCommandV1Response::Complete {
            stdout: vec![],
            error: None,
            exit_status: 0,
        }));
    assert_eq!(exchange(Some(encode_frame(&response).unwrap())).unwrap(), 0);
}

#[test]
fn sc2_batch_refuses_old_peer_before_the_first_mutating_command() {
    let path = std::env::temp_dir().join(format!(
        "sc2-batch-{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let listener = UnixListener::bind(&path).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut decoder = FrameDecoder::new();
        let mut bytes = [0; 8192];
        let request = loop {
            if let Some(request) = decoder.next_frame::<Request>().unwrap() {
                break request;
            }
            let n = stream.read(&mut bytes).unwrap();
            assert!(n > 0);
            decoder.push_bytes(&bytes[..n]);
        };
        assert!(matches!(request, Request::Handshake(_)), "{request:?}");
        stream
            .write_all(
                &encode_frame(&Response::Error(ErrorResponse {
                    error: RmuxError::Server("old peer has no handshake capability".into()),
                }))
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            stream.read(&mut bytes).unwrap(),
            0,
            "no mutation before refusal"
        );
        listener.set_nonblocking(true).unwrap();
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
    });
    let command =
        crate::cli_args::parse(["rmux", "set-environment", "-g", "SC2_EFFECT", "forbidden"])
            .unwrap()
            .command
            .unwrap();
    let startup = crate::cli::startup::StartupOptions::new(
        true,
        rmux_client::AutoStartConfig::default_files(true, None),
        crate::cli::startup::StartupEndpoint::resolved(path.clone()),
    );
    let result = crate::cli::dispatch::dispatch_command_queue(
        vec![
            command,
            crate::cli_args::Command::SplitWindow(split_args(&["-k"])),
        ],
        &path,
        startup,
        rmux_proto::ClientTerminalContext::default(),
    );
    assert!(result.is_err());
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn sc2_cli_reconstructs_literal_values_without_turning_them_into_commands() {
    let args = split_args(&["-k", "-F", "-k\\;", "--", "printf 'x'\\;"]);
    let parsed = rmux_core::command_parser::CommandParser::new()
        .parse_arguments(arguments(&args).unwrap())
        .unwrap();
    assert_eq!(parsed.commands().len(), 1);
    assert_eq!(
        parsed.commands()[0].arguments().last().unwrap().as_string(),
        Some("printf 'x';")
    );
}
