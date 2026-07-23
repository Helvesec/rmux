use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use rmux_proto::{
    encode_frame, ErrorResponse, FrameDecoder, HandshakeRequest, HandshakeResponse,
    HasSessionRequest, HasSessionResponse, Request, Response, RmuxError, RMUX_FRAME_MAGIC,
};

use super::{is_absent_error, socket_path_from_parts, ClientError};

static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);
static RMUX_TMPDIR_ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn absent_error_detects_not_found() {
    let error = io::Error::new(io::ErrorKind::NotFound, "gone");
    assert!(is_absent_error(&error));
}

#[test]
fn absent_error_detects_connection_refused() {
    let error = io::Error::new(io::ErrorKind::ConnectionRefused, "nope");
    assert!(is_absent_error(&error));
}

#[test]
fn absent_error_rejects_permission_denied() {
    let error = io::Error::new(io::ErrorKind::PermissionDenied, "no");
    assert!(!is_absent_error(&error));
}

#[test]
fn socket_path_uses_custom_rmux_tmpdir() {
    let root = unique_temp_root("custom-rmux-tmpdir");
    fs::create_dir_all(&root).expect("create socket root");
    let expected_root = fs::canonicalize(&root).expect("canonical test root");

    let path = socket_path_from_parts(Some(root.as_os_str()), 42, OsStr::new("custom"))
        .expect("socket path");

    assert_eq!(path, expected_root.join("rmux-42").join("custom"));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn socket_label_is_appended_without_absolute_path_semantics() {
    let root = unique_temp_root("absolute-label");
    fs::create_dir_all(&root).expect("create socket root");
    let expected_root = fs::canonicalize(&root).expect("canonical test root");

    let path = socket_path_from_parts(Some(root.as_os_str()), 42, OsStr::new("/tmp/escaped"))
        .expect("socket path");

    let mut expected = expected_root
        .join("rmux-42")
        .into_os_string()
        .into_vec();
    expected.extend_from_slice(b"//tmp/escaped");
    assert_eq!(path, PathBuf::from(OsString::from_vec(expected)));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn socket_path_falls_back_when_rmux_tmpdir_is_empty() {
    let path =
        socket_path_from_parts(Some(OsStr::new("")), 7, OsStr::new("default")).expect("path");
    assert_eq!(
        path,
        fs::canonicalize("/tmp")
            .expect("canonical /tmp")
            .join("rmux-7")
            .join("default")
    );
}

#[test]
fn socket_path_falls_back_when_rmux_tmpdir_cannot_be_resolved() {
    let path = socket_path_from_parts(
        Some(OsStr::new("relative-rmux-test-path-that-does-not-exist")),
        7,
        OsStr::new("default"),
    )
    .expect("path");
    assert_eq!(
        path,
        fs::canonicalize("/tmp")
            .expect("canonical /tmp")
            .join("rmux-7")
            .join("default")
    );
}

#[test]
fn default_socket_path_uses_spec_layout() {
    let path = super::default_socket_path().expect("default socket path");
    let path_text = path.to_string_lossy();
    assert!(
        path_text.ends_with("/default"),
        "path should end with /default: {path_text}"
    );
    assert!(
        path_text.contains("/rmux-"),
        "path should contain /rmux-: {path_text}"
    );
}

#[test]
fn default_socket_path_matches_server_path() {
    let client_path = super::default_socket_path().expect("client socket path");
    let server_path = rmux_server::default_socket_path().expect("server socket path");
    assert_eq!(
        client_path, server_path,
        "client and server socket paths must be identical"
    );
}

#[test]
fn default_socket_path_matches_server_path_when_rmux_tmpdir_is_unresolved() {
    let _guard = RMUX_TMPDIR_ENV_LOCK.lock().expect("rmux tmpdir env lock");
    let original = std::env::var_os("RMUX_TMPDIR");
    std::env::set_var(
        "RMUX_TMPDIR",
        "relative-rmux-test-path-that-does-not-exist",
    );

    let client_path = super::default_socket_path().expect("client socket path");
    let server_path = rmux_server::default_socket_path().expect("server socket path");

    assert_eq!(client_path, server_path);
    let fallback_root = fs::canonicalize("/tmp").expect("canonical fallback socket root");
    assert!(
        client_path.starts_with(&fallback_root),
        "unresolved RMUX_TMPDIR must fall back to /tmp, got {}",
        client_path.display()
    );

    match original {
        Some(value) => std::env::set_var("RMUX_TMPDIR", value),
        None => std::env::remove_var("RMUX_TMPDIR"),
    }
}

#[test]
fn resolve_socket_path_prefers_socket_path_over_socket_name_and_rmux_env() {
    let _guard = RMUX_TMPDIR_ENV_LOCK.lock().expect("rmux tmpdir env lock");
    let original_rmux = std::env::var_os("RMUX");
    let original_tmux = std::env::var_os("TMUX");
    std::env::set_var("RMUX", "/tmp/from-rmux,1,0");
    std::env::remove_var("TMUX");

    let path = super::resolve_socket_path(
        Some(OsStr::new("named")),
        Some(Path::new("/tmp/from-flag")),
    )
    .expect("resolved socket path");

    assert_eq!(path, PathBuf::from("/tmp/from-flag"));
    match original_rmux {
        Some(value) => std::env::set_var("RMUX", value),
        None => std::env::remove_var("RMUX"),
    }
    match original_tmux {
        Some(value) => std::env::set_var("TMUX", value),
        None => std::env::remove_var("TMUX"),
    }
}

#[test]
fn resolve_socket_path_uses_rmux_env_before_default_label() {
    let _guard = RMUX_TMPDIR_ENV_LOCK.lock().expect("rmux tmpdir env lock");
    let original_rmux = std::env::var_os("RMUX");
    let original_tmux = std::env::var_os("TMUX");
    std::env::set_var("RMUX", "/tmp/rmux-1000/from-rmux,1,0");
    std::env::remove_var("TMUX");

    let path = super::resolve_socket_path(None, None).expect("resolved socket path");

    assert_eq!(path, PathBuf::from("/tmp/rmux-1000/from-rmux"));
    match original_rmux {
        Some(value) => std::env::set_var("RMUX", value),
        None => std::env::remove_var("RMUX"),
    }
    match original_tmux {
        Some(value) => std::env::set_var("TMUX", value),
        None => std::env::remove_var("TMUX"),
    }
}

#[test]
fn resolve_socket_path_uses_rmux_env_before_tmux_env() {
    let _guard = RMUX_TMPDIR_ENV_LOCK.lock().expect("rmux tmpdir env lock");
    let original_rmux = std::env::var_os("RMUX");
    let original_tmux = std::env::var_os("TMUX");
    std::env::set_var("RMUX", "/tmp/rmux-1000/from-rmux,1,0");
    std::env::set_var("TMUX", "/tmp/rmux-1000/from-tmux,1,0");

    let path = super::resolve_socket_path(None, None).expect("resolved socket path");

    assert_eq!(path, PathBuf::from("/tmp/rmux-1000/from-rmux"));
    match original_rmux {
        Some(value) => std::env::set_var("RMUX", value),
        None => std::env::remove_var("RMUX"),
    }
    match original_tmux {
        Some(value) => std::env::set_var("TMUX", value),
        None => std::env::remove_var("TMUX"),
    }
}

#[test]
fn resolve_socket_path_accepts_rmux_env_without_tmux_suffix() {
    let _guard = RMUX_TMPDIR_ENV_LOCK.lock().expect("rmux tmpdir env lock");
    let original_rmux = std::env::var_os("RMUX");
    let original_tmux = std::env::var_os("TMUX");
    std::env::set_var("RMUX", "/tmp/rmux-1000/from-rmux");
    std::env::remove_var("TMUX");

    let path = super::resolve_socket_path(None, None).expect("resolved socket path");

    assert_eq!(path, PathBuf::from("/tmp/rmux-1000/from-rmux"));
    match original_rmux {
        Some(value) => std::env::set_var("RMUX", value),
        None => std::env::remove_var("RMUX"),
    }
    match original_tmux {
        Some(value) => std::env::set_var("TMUX", value),
        None => std::env::remove_var("TMUX"),
    }
}

#[test]
fn resolve_socket_path_ignores_tmux_env_and_uses_default() {
    let _guard = RMUX_TMPDIR_ENV_LOCK.lock().expect("rmux tmpdir env lock");
    let original_rmux = std::env::var_os("RMUX");
    let original_tmux = std::env::var_os("TMUX");
    std::env::set_var("TMUX", "/tmp/explicit-rmux.sock,1,0");
    std::env::remove_var("RMUX");

    let path = super::resolve_socket_path(None, None).expect("resolved socket path");
    let default = super::default_socket_path().expect("default socket path");

    assert_eq!(path, default);
    match original_rmux {
        Some(value) => std::env::set_var("RMUX", value),
        None => std::env::remove_var("RMUX"),
    }
    match original_tmux {
        Some(value) => std::env::set_var("TMUX", value),
        None => std::env::remove_var("TMUX"),
    }
}

#[test]
fn resolve_socket_path_ignores_tmux_env_without_suffix() {
    let _guard = RMUX_TMPDIR_ENV_LOCK.lock().expect("rmux tmpdir env lock");
    let original_rmux = std::env::var_os("RMUX");
    let original_tmux = std::env::var_os("TMUX");
    std::env::remove_var("RMUX");
    std::env::set_var("TMUX", "/tmp/explicit-rmux.sock");

    let path = super::resolve_socket_path(None, None).expect("resolved socket path");
    let default = super::default_socket_path().expect("default socket path");

    assert_eq!(path, default);
    match original_rmux {
        Some(value) => std::env::set_var("RMUX", value),
        None => std::env::remove_var("RMUX"),
    }
    match original_tmux {
        Some(value) => std::env::set_var("TMUX", value),
        None => std::env::remove_var("TMUX"),
    }
}

#[test]
fn resolve_socket_path_ignores_empty_rmux_env() {
    let _guard = RMUX_TMPDIR_ENV_LOCK.lock().expect("rmux tmpdir env lock");
    let original_rmux = std::env::var_os("RMUX");
    let original_tmux = std::env::var_os("TMUX");
    std::env::set_var("RMUX", "");
    std::env::remove_var("TMUX");

    let path = super::resolve_socket_path(None, None).expect("resolved socket path");
    let default = super::default_socket_path().expect("default socket path");

    assert_eq!(path, default);
    match original_rmux {
        Some(value) => std::env::set_var("RMUX", value),
        None => std::env::remove_var("RMUX"),
    }
    match original_tmux {
        Some(value) => std::env::set_var("TMUX", value),
        None => std::env::remove_var("TMUX"),
    }
}

#[test]
fn resolve_socket_path_ignores_nonempty_malformed_rmux_env() {
    let _guard = RMUX_TMPDIR_ENV_LOCK.lock().expect("rmux tmpdir env lock");
    let original_rmux = std::env::var_os("RMUX");
    let original_tmux = std::env::var_os("TMUX");
    std::env::set_var("RMUX", "malformed-rmux-value");
    std::env::remove_var("TMUX");

    let path = super::resolve_socket_path(None, None).expect("resolved socket path");
    let default = super::default_socket_path().expect("default socket path");

    assert_eq!(path, default);
    match original_rmux {
        Some(value) => std::env::set_var("RMUX", value),
        None => std::env::remove_var("RMUX"),
    }
    match original_tmux {
        Some(value) => std::env::set_var("TMUX", value),
        None => std::env::remove_var("TMUX"),
    }
}

#[test]
fn connect_to_nonexistent_path_is_absent() {
    let result =
        super::connect_or_absent(Path::new("/tmp/rmux-nonexistent-test-socket-path/default"))
            .expect("should succeed with Absent");
    assert!(matches!(result, super::ConnectResult::Absent));
}

#[test]
fn connect_to_nonexistent_path_errors() {
    let error = super::connect(Path::new("/tmp/rmux-nonexistent-test-socket-path/default"));
    assert!(matches!(error, Err(ClientError::Io(_))));
}

#[test]
fn stale_socket_is_treated_as_absent() {
    let socket_path = unique_socket_path("stale");
    let parent = socket_path.parent().expect("socket parent");
    fs::create_dir_all(parent).expect("create test directory");

    let listener = UnixListener::bind(&socket_path).expect("bind stale socket");
    drop(listener);

    let result = super::connect_or_absent(&socket_path).expect("stale socket should not fail");
    assert!(matches!(result, super::ConnectResult::Absent));

    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_dir_all(parent);
}

#[test]
fn connect_with_timeout_preserves_timeout_errors() {
    let socket_path = Path::new("/tmp/rmux-connect-timeout-test/default");
    let error = super::connect_with_timeout_using(
        socket_path,
        Duration::from_millis(100),
        |_path, _timeout| Err(io::Error::new(io::ErrorKind::TimedOut, "connect timeout")),
    )
    .expect_err("timed-out connects must stay errors");
    assert!(matches!(
        error,
        ClientError::Io(error) if error.kind() == io::ErrorKind::TimedOut
    ));
}

#[test]
fn connect_or_absent_preserves_timeout_errors() {
    let socket_path = Path::new("/tmp/rmux-connect-or-absent-timeout-test/default");
    let error = super::connect_or_absent_with_timeout_using(
        socket_path,
        Duration::from_millis(100),
        |_path, _timeout| Err(io::Error::new(io::ErrorKind::TimedOut, "connect timeout")),
    )
    .expect_err("timed-out connects must not be treated as Absent");
    assert!(matches!(
        error,
        ClientError::Io(error) if error.kind() == io::ErrorKind::TimedOut
    ));
}

#[test]
fn completed_response_survives_darwin_timeout_restore_einval() {
    let response = Response::HasSession(HasSessionResponse { exists: false });
    let result = super::finish_unbounded_roundtrip(
        Ok(response.clone()),
        Err(ClientError::Io(io::Error::from(
            io::ErrorKind::InvalidInput,
        ))),
    );

    if cfg!(target_os = "macos") {
        assert_eq!(result.expect("completed response remains valid"), response);
    } else {
        assert!(matches!(
            result,
            Err(ClientError::Io(error)) if error.kind() == io::ErrorKind::InvalidInput
        ));
    }
}

#[test]
fn completed_response_does_not_hide_other_timeout_restore_errors() {
    let result = super::finish_unbounded_roundtrip(
        Ok(Response::HasSession(HasSessionResponse { exists: false })),
        Err(ClientError::Io(io::Error::from(
            io::ErrorKind::PermissionDenied,
        ))),
    )
    .expect_err("other restore errors must remain visible");

    assert!(matches!(
        result,
        ClientError::Io(error) if error.kind() == io::ErrorKind::PermissionDenied
    ));
}

#[test]
fn roundtrip_reads_partial_response_frames() {
    let request = Request::HasSession(HasSessionRequest {
        target: rmux_proto::SessionName::new("alpha").expect("valid session"),
    });
    let response = Response::HasSession(HasSessionResponse { exists: true });
    let (client_stream, server_stream) = UnixStream::pair().expect("create stream pair");
    let expected_request = request.clone();
    let expected_response = response.clone();

    let server = thread::spawn(move || {
        let stream = expect_request(server_stream, expected_request);
        send_fragmented_response(stream, expected_response, &[1, 2, 3, 5]);
    });

    let mut connection = super::Connection::new(client_stream).expect("connection with timeout");
    let actual = connection.roundtrip(&request).expect("roundtrip succeeds");
    assert_eq!(actual, response);

    server.join().expect("server thread joins");
}

#[test]
fn roundtrip_preserves_error_responses() {
    let request = Request::HasSession(HasSessionRequest {
        target: rmux_proto::SessionName::new("alpha").expect("valid session"),
    });
    let response = Response::Error(ErrorResponse {
        error: RmuxError::SessionNotFound("alpha".to_owned()),
    });
    let (client_stream, server_stream) = UnixStream::pair().expect("create stream pair");
    let expected_request = request.clone();
    let expected_response = response.clone();

    let server = thread::spawn(move || {
        let stream = expect_request(server_stream, expected_request);
        write_response(stream, expected_response);
    });

    let mut connection = super::Connection::new(client_stream).expect("connection with timeout");
    let actual = connection.roundtrip(&request).expect("roundtrip succeeds");
    assert_eq!(actual, response);

    server.join().expect("server thread joins");
}

#[test]
fn supports_capability_reads_handshake_capabilities() {
    let response = Response::Handshake(HandshakeResponse {
        wire_version: rmux_proto::RMUX_WIRE_VERSION,
        capabilities: vec!["stream.attach.resize_geometry".to_owned()],
    });
    let (client_stream, server_stream) = UnixStream::pair().expect("create stream pair");

    let server = thread::spawn(move || {
        let stream = expect_request(server_stream, Request::Handshake(HandshakeRequest::current()));
        write_response(stream, response);
    });

    let mut connection = super::Connection::new(client_stream).expect("connection with timeout");
    assert!(
        connection
            .supports_capability("stream.attach.resize_geometry")
            .expect("capability query succeeds")
    );

    server.join().expect("server thread joins");
}

#[test]
fn supports_capability_caches_handshake_capabilities() {
    let response = Response::Handshake(HandshakeResponse {
        wire_version: rmux_proto::RMUX_WIRE_VERSION,
        capabilities: vec![
            "daemon.status".to_owned(),
            "daemon.shutdown_if_idle".to_owned(),
        ],
    });
    let (client_stream, server_stream) = UnixStream::pair().expect("create stream pair");

    let server = thread::spawn(move || {
        let stream = expect_request(server_stream, Request::Handshake(HandshakeRequest::current()));
        write_response(stream, response);
    });

    let mut connection = super::Connection::new(client_stream).expect("connection with timeout");
    assert!(
        connection
            .supports_capability("daemon.status")
            .expect("first capability query succeeds")
    );
    assert!(
        connection
            .supports_capability("daemon.shutdown_if_idle")
            .expect("second capability query uses cache")
    );

    server.join().expect("server thread joins");
}

#[test]
fn supports_capability_treats_error_response_as_unsupported() {
    let response = Response::Error(ErrorResponse {
        error: RmuxError::UnknownCommand("handshake".to_owned()),
    });
    let (client_stream, server_stream) = UnixStream::pair().expect("create stream pair");

    let server = thread::spawn(move || {
        let stream = expect_request(server_stream, Request::Handshake(HandshakeRequest::current()));
        write_response(stream, response);
    });

    let mut connection = super::Connection::new(client_stream).expect("connection with timeout");
    assert!(
        !connection
            .supports_capability("stream.attach.resize_geometry")
            .expect("capability query succeeds")
    );

    server.join().expect("server thread joins");
}

#[test]
fn legacy_wire_v1_request_writer_uses_v1_shutdown_envelope() {
    let (client_stream, mut server_stream) = UnixStream::pair().expect("create stream pair");
    let mut connection = super::Connection::new(client_stream).expect("connection with timeout");

    connection
        .write_legacy_wire_request(&Request::KillServer(rmux_proto::KillServerRequest), 1)
        .expect("write legacy kill-server request");

    let mut buffer = [0_u8; 64];
    let bytes_read = server_stream
        .read(&mut buffer)
        .expect("read legacy shutdown frame");
    assert!(bytes_read >= 10, "legacy frame should include envelope and tag");
    assert_eq!(buffer[0], RMUX_FRAME_MAGIC);
    assert_eq!(buffer[1], 1, "legacy shutdown must use wire version 1");

    let length = u32::from_le_bytes(
        buffer[2..6]
            .try_into()
            .expect("single-byte wire version leaves length at offset 2"),
    );
    assert_eq!(length, 4, "empty KillServer payload only carries enum tag");
    assert_eq!(&buffer[6..10], &72_u32.to_le_bytes());
}

#[test]
fn legacy_wire_request_writer_can_target_wire_v3_shutdown() {
    let (client_stream, mut server_stream) = UnixStream::pair().expect("create stream pair");
    let mut connection = super::Connection::new(client_stream).expect("connection with timeout");

    connection
        .write_legacy_wire_request(&Request::KillServer(rmux_proto::KillServerRequest), 3)
        .expect("write wire-v3 kill-server request");

    let mut buffer = [0_u8; 64];
    let bytes_read = server_stream
        .read(&mut buffer)
        .expect("read legacy shutdown frame");
    assert!(bytes_read >= 10, "legacy frame should include envelope and tag");
    assert_eq!(buffer[0], RMUX_FRAME_MAGIC);
    assert_eq!(buffer[1], 3, "0.8 shutdown fallback must use wire version 3");

    let length = u32::from_le_bytes(
        buffer[2..6]
            .try_into()
            .expect("single-byte wire version leaves length at offset 2"),
    );
    assert_eq!(length, 4, "empty KillServer payload only carries enum tag");
    assert_eq!(&buffer[6..10], &72_u32.to_le_bytes());
}

#[test]
fn roundtrip_rejects_truncated_response_frames() {
    let request = Request::HasSession(HasSessionRequest {
        target: rmux_proto::SessionName::new("alpha").expect("valid session"),
    });
    let response = Response::HasSession(HasSessionResponse { exists: true });
    let (client_stream, server_stream) = UnixStream::pair().expect("create stream pair");
    let expected_request = request.clone();

    let server = thread::spawn(move || {
        let mut stream = expect_request(server_stream, expected_request);
        let frame = encode_frame(&response).expect("encode response");
        let truncated = &frame[..frame.len() - 1];
        stream
            .write_all(truncated)
            .expect("write truncated response frame");
    });

    let mut connection = super::Connection::new(client_stream).expect("connection with timeout");
    let error = connection
        .roundtrip(&request)
        .expect_err("response should fail");
    assert!(matches!(error, ClientError::UnexpectedEof));

    server.join().expect("server thread joins");
}

#[test]
fn connection_new_applies_socket_timeouts() {
    let (client_stream, _server_stream) = UnixStream::pair().expect("create stream pair");
    let connection = super::Connection::new(client_stream).expect("connection with timeout");

    assert_eq!(
        connection.stream.read_timeout().expect("read timeout"),
        Some(super::SOCKET_RESPONSE_TIMEOUT)
    );
    assert_eq!(
        connection.stream.write_timeout().expect("write timeout"),
        Some(super::SOCKET_WRITE_TIMEOUT)
    );
}

fn expect_request(mut stream: UnixStream, expected: Request) -> UnixStream {
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 128];

    loop {
        match decoder.next_frame::<Request>() {
            Ok(Some(actual)) => {
                assert_eq!(actual, expected);
                return stream;
            }
            Ok(None) => {}
            Err(error) => panic!("invalid request frame: {error}"),
        }

        let bytes_read = stream.read(&mut buffer).expect("read request bytes");
        assert!(bytes_read > 0, "client closed before request frame arrived");
        decoder.push_bytes(&buffer[..bytes_read]);
    }
}

fn send_fragmented_response(stream: UnixStream, response: Response, chunk_lengths: &[usize]) {
    let frame = encode_frame(&response).expect("encode response");
    let mut offset = 0;
    let mut stream = stream;

    for &chunk_length in chunk_lengths {
        if offset >= frame.len() {
            break;
        }

        let end = (offset + chunk_length).min(frame.len());
        stream
            .write_all(&frame[offset..end])
            .expect("write response fragment");
        offset = end;
    }

    if offset < frame.len() {
        stream
            .write_all(&frame[offset..])
            .expect("write remaining response bytes");
    }
}

fn write_response(stream: UnixStream, response: Response) {
    let frame = encode_frame(&response).expect("encode response");
    let mut stream = stream;
    stream.write_all(&frame).expect("write response frame");
}

fn unique_socket_path(label: &str) -> PathBuf {
    let unique_id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir()
        .join(format!("rmux-client-connection-tests-{label}"))
        .join(format!("{}-{unique_id}.sock", std::process::id()))
}

fn unique_temp_root(label: &str) -> PathBuf {
    let unique_id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "rmux-client-connection-tests-{label}-{}-{unique_id}",
        std::process::id()
    ))
}
