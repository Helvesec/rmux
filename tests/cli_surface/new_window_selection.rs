use super::*;
use rmux_proto::{PaneTarget, ResolveTargetResponse, ResolveTargetType, Target};

// A native socket peer proves that CLI -S sends one server-owned operation and
// propagates its late identity failure, including the detached and alias paths.
#[test]
fn direct_new_window_selection_delegates_and_propagates_replacement() -> Result<(), Box<dyn Error>>
{
    for (command, flag) in [("new-window", "-S"), ("new-window", "-Sd"), ("neww", "-Sd")] {
        let harness = CliHarness::new("new-window-selection-dispatch")?;
        prepare_fake_server_socket_parent(harness.socket_path())?;
        let listener = UnlinkingUnixListener::bind(harness.socket_path())?;
        listener.listener().set_nonblocking(true)?;
        let server = thread::spawn(move || -> io::Result<()> {
            let deadline = Instant::now() + ATTACH_TIMEOUT;
            loop {
                let (mut stream, _) = match listener.listener().accept() {
                    Ok(accepted) => accepted,
                    Err(error)
                        if error.kind() == io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                // Darwin propagates O_NONBLOCK from a listening Unix socket
                // to accepted streams. The peer itself must block between
                // sequential handshake, option and command frames.
                stream.set_nonblocking(false)?;
                stream.set_read_timeout(Some(ATTACH_TIMEOUT))?;
                loop {
                    let request = match read_current_wire_request(&mut stream) {
                        Ok(request) => request,
                        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                        Err(error) => return Err(error),
                    };
                    let mut done = false;
                    let response = match request {
                        Request::Handshake(_) => Response::Handshake(HandshakeResponse {
                            wire_version: RMUX_WIRE_VERSION,
                            capabilities: vec!["protocol.capabilities".to_owned()],
                        }),
                        Request::ShowOptions(_) => Response::ShowOptions(ShowOptionsResponse {
                            scope: OptionScopeSelector::ServerGlobal,
                            output: CommandOutput::from_stdout(Vec::new()),
                        }),
                        Request::ResolveTarget(request) => {
                            assert_eq!(request.target_type, ResolveTargetType::Pane);
                            Response::ResolveTarget(ResolveTargetResponse {
                                target: Target::Pane(PaneTarget::with_window(
                                    SessionName::new("caller").unwrap(),
                                    0,
                                    0,
                                )),
                            })
                        }
                        Request::SourceFile(request) => {
                            assert_eq!(request.paths, ["-"]);
                            let text = request.stdin.expect("canonical command payload");
                            assert!(text.starts_with("new-window "), "{text}");
                            assert!(text.contains("found"), "{text}");
                            assert_eq!(request.target.unwrap().session_name().as_str(), "caller");
                            done = true;
                            Response::Error(ErrorResponse {
                                error: RmuxError::Server(
                                    "new-window -S target was replaced before execution".to_owned(),
                                ),
                            })
                        }
                        other => {
                            panic!("-S must not split lookup and selection into RPCs: {other:?}")
                        }
                    };
                    stream.write_all(&encode_frame(&response).map_err(io::Error::other)?)?;
                    if done {
                        return Ok(());
                    }
                }
            }
        });
        let output = harness.run(&[command, flag, "-t", "caller:", "-n", "found"])?;
        if let Err(error) = server.join().expect("selection peer completed") {
            return Err(format!(
                "{command} {flag}: selection peer failed: {error}; status={:?}; stdout={:?}; stderr={:?}",
                output.status.code(),
                stdout(&output),
                stderr(&output)
            )
            .into());
        }
        assert_eq!(output.status.code(), Some(1));
        assert!(stdout(&output).is_empty());
        assert!(
            stderr(&output).contains("target was replaced before execution"),
            "{}",
            stderr(&output)
        );
        assert!(!stderr(&output).starts_with("-:"), "{}", stderr(&output));
    }
    Ok(())
}
