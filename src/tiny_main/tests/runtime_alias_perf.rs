use super::*;
#[cfg(not(target_has_atomic = "64"))]
use portable_atomic::AtomicU64;
use rmux_proto::{
    encode_frame, CommandOutput, FrameDecoder, HandshakeResponse, Request,
    RuntimeCommandExecutionResponse, ShowOptionsResponse, CAPABILITY_CLI_RUNTIME_COMMAND_EXPANSION,
};
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
#[cfg(target_has_atomic = "64")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

static NEXT: AtomicU64 = AtomicU64::new(0);

// Exercise the public dispatch decision, not only its final execution helper:
// the regression added an earlier full-argv expansion outside that helper.
fn dispatch(arguments: &[&str], aliases: &str, canonical: Option<&str>) -> RuntimeAliasDispatch {
    dispatch_with_capability(arguments, aliases, canonical, true)
}

fn dispatch_with_capability(
    arguments: &[&str],
    aliases: &str,
    canonical: Option<&str>,
    supports_expansion: bool,
) -> RuntimeAliasDispatch {
    let path = std::env::temp_dir().join(format!(
        "rmux-alias-perf-{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let listener = UnixListener::bind(&path).unwrap();
    let expected: Vec<String> = arguments.iter().map(|s| (*s).to_owned()).collect();
    let aliases = aliases.to_owned();
    let canonical = canonical.map(str::to_owned);
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut decoder = FrameDecoder::new();
        let mut bytes = [0; 8192];
        let mut requests = Vec::new();
        loop {
            let request = loop {
                if let Some(request) = decoder.next_frame::<Request>().unwrap() {
                    break Some(request);
                }
                let n = stream.read(&mut bytes).unwrap();
                if n == 0 {
                    break None;
                }
                decoder.push_bytes(&bytes[..n]);
            };
            let Some(request) = request else { break };
            let response = match request {
                Request::Handshake(_) => {
                    requests.push("handshake");
                    Response::Handshake(HandshakeResponse {
                        wire_version: RMUX_WIRE_VERSION,
                        capabilities: if supports_expansion {
                            vec![CAPABILITY_CLI_RUNTIME_COMMAND_EXPANSION.to_owned()]
                        } else {
                            vec![]
                        },
                    })
                }
                Request::ShowOptions(request) => {
                    requests.push("aliases");
                    assert_eq!(request.name.as_deref(), Some("command-alias"));
                    assert!(request.value_only);
                    let output = if request.value_only {
                        decode_command_alias_definitions(aliases.as_bytes())
                            .unwrap()
                            .join("\n")
                    } else {
                        aliases.clone()
                    };
                    Response::ShowOptions(ShowOptionsResponse {
                        scope: OptionScopeSelector::ServerGlobal,
                        output: CommandOutput::from_stdout(output),
                    })
                }
                Request::SourceFile(_) => {
                    requests.push("expand");
                    // A full-argv expansion with no matching alias is the
                    // deterministic performance failure on the uncorrected head.
                    let canonical = canonical
                        .as_ref()
                        .expect("unaliased argv must not be expanded before execution");
                    Response::SourceFile(SourceFileResponse::from_output(
                        CommandOutput::from_stdout(canonical.clone()),
                    ))
                }
                Request::RuntimeCommandExecutionV2(offer) => {
                    requests.push("execute");
                    assert_eq!(offer.arguments, expected);
                    Response::RuntimeCommandExecution(Box::new(
                        RuntimeCommandExecutionResponse::Complete {
                            command_name: expected[0].clone(),
                            stdout: vec![],
                            error: None,
                            exit_status: 0,
                        },
                    ))
                }
                request => panic!("unexpected request: {request:?}"),
            };
            stream.write_all(&encode_frame(&response).unwrap()).unwrap();
        }
        assert_eq!(requests.iter().filter(|r| **r == "aliases").count(), 1);
        assert_eq!(
            requests.iter().filter(|r| **r == "expand").count(),
            usize::from(canonical.is_some())
        );
        requests
    });
    let mut argv = vec![
        OsString::from("rmux"),
        OsString::from("-S"),
        path.as_os_str().to_owned(),
    ];
    argv.extend(arguments.iter().map(|arg| OsString::from(*arg)));
    let TinyInvocation::Direct(command) = TinyInvocation::parse(&argv) else {
        panic!("direct invocation");
    };
    let mut connection = TinyCommandConnection::disconnected(&path);
    let result = command.runtime_alias_dispatch(&argv, &mut connection);
    drop(connection);
    let requests = server.join();
    std::fs::remove_file(path).unwrap();
    let requests = requests.unwrap();
    let result = result.unwrap();
    assert_eq!(
        requests.iter().filter(|r| **r == "execute").count(),
        usize::from(matches!(result, RuntimeAliasDispatch::Complete(0)))
    );
    result
}

#[test]
fn unaliased_heavy_status_format_is_transferred_only_once() {
    let format = "#{session_name}:#{window_index}:#{pane_index}:#{window_panes}:#{?pane_active,active,inactive}|".repeat(80);
    for aliases in [
        "",
        "command-alias[20] 'display-message-extra=split-window -k'\n",
    ] {
        assert!(matches!(
            dispatch(
                &["display-message", "-p", "-F", &format, "-t", "perf"],
                aliases,
                None
            ),
            RuntimeAliasDispatch::Complete(0)
        ));
    }
}

#[test]
fn sibling_unaliased_commands_skip_argument_expansion() {
    for arguments in [
        &["list-sessions"][..],
        &["has-session", "-t", "perf"],
        &["list-windows", "-t", "perf"],
        &["list-panes", "-t", "perf"],
        &["capture-pane", "-p"],
        &["resize-pane", "-t", "perf", "-x", "100"],
        &["split-window", "-d", "-t", "perf"],
        &["set-option", "-g", "status", "on"],
        &["set-window-option", "-g", "automatic-rename", "off"],
        &["show-options", "-g"],
        &["show-window-options", "-g"],
        &["new-window", "-d", "-t", "perf"],
        &["rename-window", "-t", "perf:0", "renamed"],
        &["select-window", "-t", "perf:0"],
        &["kill-pane", "-t", "perf:0.0"],
        &["join-pane", "-d", "-s", "perf:1.0", "-t", "perf:0.0"],
        &["kill-session", "-t", "perf"],
    ] {
        assert!(matches!(
            dispatch(arguments, "", None),
            RuntimeAliasDispatch::Complete(0)
        ));
    }
}

#[test]
fn aliased_commands_keep_explicit_admission_before_execution() {
    for canonical in [
        "split-window -dk",
        "new-pane -d -x30 -y8 -X3 -Y4",
        "set-pane-placement floating",
        "set-environment -g BEFORE yes ; split-window -k",
    ] {
        assert!(matches!(
            dispatch(
                &["list-sessions"],
                "command-alias[20] 'list-sessions=probe'\n",
                Some(canonical)
            ),
            RuntimeAliasDispatch::FullHelper
        ));
    }
    assert!(matches!(
        dispatch(
            &["list-sessions"],
            "command-alias[20] 'list-sessions=display-message -p hello'\n",
            Some("display-message -p hello")
        ),
        RuntimeAliasDispatch::Complete(0)
    ));
}

#[test]
fn legacy_server_without_expansion_keeps_direct_or_helper_routing() {
    assert!(matches!(
        dispatch_with_capability(&["list-sessions"], "", None, false),
        RuntimeAliasDispatch::Direct
    ));
    assert!(matches!(
        dispatch_with_capability(
            &["list-sessions"],
            "command-alias[20] 'list-sessions=display-message -p hello'\n",
            None,
            false
        ),
        RuntimeAliasDispatch::FullHelper
    ));
}

#[test]
fn multiline_alias_values_conservatively_keep_preflight() {
    // Value-only output loses array entry boundaries; a false positive must
    // retain read-only expansion rather than mistake it for a missing alias.
    assert!(matches!(
        dispatch(
            &["list-sessions"],
            "command-alias[20] \"unrelated=display-message -p \\nlist-sessions=split-window -k\"\n",
            Some("list-sessions")
        ),
        RuntimeAliasDispatch::Complete(0)
    ));
}

// r18 refuses an older wire through the full client before any execution
// offer; the read-only preflight must keep that classification.
fn dispatch_after_preflight_replies(
    replies: Vec<Response>,
) -> Result<RuntimeAliasDispatch, String> {
    let path = std::env::temp_dir().join(format!(
        "rmux-alias-wire-{}-{}.sock",
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
        let mut requests = Vec::new();
        loop {
            while let Some(request) = decoder.next_frame::<Request>().unwrap() {
                requests.push(request);
            }
            if requests.len() == 2 {
                break;
            }
            let n = stream.read(&mut bytes).unwrap();
            assert_ne!(n, 0, "preflight closed before pipelining both reads");
            decoder.push_bytes(&bytes[..n]);
        }
        assert!(matches!(requests[0], Request::Handshake(_)));
        assert!(matches!(requests[1], Request::ShowOptions(_)));
        for reply in replies {
            stream.write_all(&encode_frame(&reply).unwrap()).unwrap();
        }
        // Neither an execution offer nor an argv expansion may follow.
        loop {
            let n = stream.read(&mut bytes).unwrap();
            if n == 0 {
                break;
            }
            decoder.push_bytes(&bytes[..n]);
        }
        assert!(decoder.next_frame::<Request>().unwrap().is_none());
    });
    let argv = [
        OsString::from("rmux"),
        OsString::from("-S"),
        path.as_os_str().to_owned(),
        OsString::from("list-sessions"),
    ];
    let TinyInvocation::Direct(command) = TinyInvocation::parse(&argv) else {
        panic!("direct invocation");
    };
    let mut connection = TinyCommandConnection::disconnected(&path);
    let result = command.runtime_alias_dispatch(&argv, &mut connection);
    drop(connection);
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
    result
}

fn wire_error(got: u32) -> Response {
    Response::Error(ErrorResponse {
        error: RmuxError::UnsupportedWireVersion {
            got,
            minimum: RMUX_WIRE_VERSION,
            maximum: RMUX_WIRE_VERSION,
        },
    })
}

#[test]
fn previous_wire_refusal_during_alias_preflight_keeps_full_helper() {
    let handshake = Response::Handshake(HandshakeResponse {
        wire_version: RMUX_WIRE_VERSION,
        capabilities: vec![CAPABILITY_CLI_RUNTIME_COMMAND_EXPANSION.to_owned()],
    });
    for replies in [
        vec![wire_error(1)],
        vec![wire_error(RMUX_WIRE_VERSION - 1)],
        vec![handshake, wire_error(1)],
    ] {
        assert!(matches!(
            dispatch_after_preflight_replies(replies),
            Ok(RuntimeAliasDispatch::FullHelper)
        ));
    }
    // A current or future wire is not a previous version: keep it terminal.
    for got in [RMUX_WIRE_VERSION, RMUX_WIRE_VERSION + 1] {
        assert!(dispatch_after_preflight_replies(vec![wire_error(got)]).is_err());
    }
}

#[test]
fn exiting_daemon_keeps_startup_fallback_without_retrying_ordinary_commands() {
    for command_name in [
        "start-server",
        "new-session",
        "attach-session",
        "list-sessions",
    ] {
        for after_handshake in [false, true] {
            let path = std::env::temp_dir().join(format!(
                "rmux-alias-exit-{}-{}.sock",
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
                loop {
                    if let Some(request) = decoder.next_frame::<Request>().unwrap() {
                        match request {
                            Request::Handshake(_) if after_handshake => {
                                let reply = Response::Handshake(HandshakeResponse {
                                    wire_version: RMUX_WIRE_VERSION,
                                    capabilities: vec![
                                        CAPABILITY_CLI_RUNTIME_COMMAND_EXPANSION.to_owned()
                                    ],
                                });
                                stream.write_all(&encode_frame(&reply).unwrap()).unwrap();
                            }
                            Request::Handshake(_) => break,
                            Request::SourceFile(_) if command_name != "list-sessions" => break,
                            Request::ShowOptions(_) if command_name == "list-sessions" => break,
                            request => panic!("unexpected request before shutdown: {request:?}"),
                        }
                    } else {
                        let n = stream.read(&mut bytes).unwrap();
                        assert_ne!(n, 0, "client closed before its read-only probe");
                        decoder.push_bytes(&bytes[..n]);
                    }
                }
                // Model a daemon exiting without answering the read-only probe.
            });
            let mut argv = vec![
                OsString::from("rmux"),
                OsString::from("-S"),
                path.as_os_str().to_owned(),
                OsString::from(command_name),
            ];
            if command_name == "new-session" {
                argv.push(OsString::from("-d"));
            }
            let TinyInvocation::Direct(command) = TinyInvocation::parse(&argv) else {
                panic!("direct invocation");
            };
            let mut connection = TinyCommandConnection::disconnected(&path);
            let result = command.runtime_alias_dispatch(&argv, &mut connection);
            drop(connection);
            server.join().unwrap();
            std::fs::remove_file(path).unwrap();
            if command_name == "list-sessions" {
                assert!(
                    result.is_err(),
                    "an ordinary command must not restart the daemon"
                );
            } else {
                assert!(matches!(result, Ok(RuntimeAliasDispatch::FullHelper)));
            }
        }
    }
}
