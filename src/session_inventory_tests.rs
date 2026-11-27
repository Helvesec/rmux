//! Canonical session-inventory scenario shared by every compiled dispatcher.
//!
//! This module is the single owner of the scenario harness. Each dispatcher
//! instantiates the shared scenarios in its own module with
//! [`canonical_inventory_tests!`].

#[cfg(not(target_has_atomic = "64"))]
use portable_atomic::AtomicU64;
use rmux_proto::{
    encode_frame, CommandOutput, FrameDecoder, ListSessionsResponse, Request, Response, SessionName,
};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(target_has_atomic = "64")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);

struct TestSocket(std::path::PathBuf);

impl Drop for TestSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub(crate) fn names() -> Vec<SessionName> {
    [
        "ordinary",
        "a\\b",
        "a\\\\b",
        "line\nbreak",
        "café界",
        "bidi\u{202e}name",
    ]
    .into_iter()
    .map(|raw| SessionName::new(raw).unwrap())
    .collect()
}

fn request(stream: &mut UnixStream, decoder: &mut FrameDecoder) -> Request {
    let mut bytes = [0; 8192];
    loop {
        if let Some(request) = decoder.next_frame().unwrap() {
            return request;
        }
        let count = stream.read(&mut bytes).unwrap();
        assert!(count > 0, "client closed before request");
        decoder.push_bytes(&bytes[..count]);
    }
}

fn respond(stream: &mut UnixStream, response: Response) {
    stream.write_all(&encode_frame(&response).unwrap()).unwrap();
}

pub(crate) fn exchange<T>(
    dispatcher: &str,
    output: String,
    serve_targets: bool,
    run: impl FnOnce(&std::path::Path) -> T,
) -> T {
    let path = std::env::temp_dir().join(format!(
        "rmux-inventory-{dispatcher}-{}-{}.sock",
        std::process::id(),
        NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
    ));
    let _socket = TestSocket(path.clone());
    let listener = UnixListener::bind(&path).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut decoder = FrameDecoder::new();
        let first = request(&mut stream, &mut decoder);
        // The full CLI probes whether the daemon implements queued list-windows.
        let first = if matches!(first, Request::Handshake(_)) {
            let mut handshake = rmux_proto::HandshakeResponse::current();
            handshake.capabilities.clear();
            respond(&mut stream, Response::Handshake(handshake));
            request(&mut stream, &mut decoder)
        } else {
            first
        };
        let Request::ListSessions(listing) = first else {
            panic!("expected inventory, got {first:?}");
        };
        assert_eq!(listing.format.as_deref(), Some("#{session_name}"));
        respond(
            &mut stream,
            Response::ListSessions(ListSessionsResponse {
                output: CommandOutput::from_stdout(output),
            }),
        );
        if serve_targets {
            for name in names() {
                let next = request(&mut stream, &mut decoder);
                match next {
                    Request::ListPanes(listing) => {
                        assert_eq!(listing.target, name);
                        assert_eq!(listing.target_window_index, None);
                        respond(
                            &mut stream,
                            Response::ListPanes(rmux_proto::ListPanesResponse {
                                output: CommandOutput::from_stdout(Vec::new()),
                            }),
                        );
                    }
                    Request::ListWindows(listing) => {
                        assert_eq!(listing.target, name);
                        respond(
                            &mut stream,
                            Response::ListWindows(rmux_proto::ListWindowsResponse {
                                windows: Vec::new(),
                                output: CommandOutput::from_stdout(Vec::new()),
                            }),
                        );
                    }
                    other => panic!("expected inventory target, got {other:?}"),
                }
            }
        }
    });
    let result = run(&path);
    let joined = server.join();
    std::fs::remove_file(path).unwrap();
    joined.unwrap();
    result
}

pub(crate) fn inventory() -> String {
    names().iter().map(|name| format!("{name}\n")).collect()
}

/// Generates the scenarios every dispatcher shares. Invoke it inside the
/// dispatcher's own `canonical_inventory_tests` module: `super::` then names
/// that dispatcher's `list_session_names` and
/// `canonical_inventory_run_all_panes`.
macro_rules! canonical_inventory_tests {
    ($dispatcher:literal) => {
        use crate::session_inventory_tests::{exchange, inventory, names};

        #[test]
        fn canonical_inventory_cli_reader_preserves_distinct_keys() {
            let result = exchange($dispatcher, inventory(), false, |path| {
                let mut connection = rmux_client::connect(path).unwrap();
                super::list_session_names(&mut connection).unwrap()
            });
            assert_eq!(result, names());
            assert_ne!(result[1], result[2]);
        }

        #[test]
        fn canonical_inventory_cli_empty_listing_stays_empty() {
            exchange($dispatcher, "\n".into(), false, |path| {
                let mut connection = rmux_client::connect(path).unwrap();
                assert!(super::list_session_names(&mut connection)
                    .unwrap()
                    .is_empty());
            });
        }

        #[test]
        fn canonical_inventory_cli_all_panes_retains_targets() {
            let result = exchange($dispatcher, inventory(), true, |path| {
                super::canonical_inventory_run_all_panes(path)
            });
            assert_eq!(result, 0);
        }
    };
}
pub(crate) use canonical_inventory_tests;
