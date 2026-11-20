use super::*;
use rmux_proto::{
    encode_frame, CommandOutput, DisplayPanesResponse, FrameDecoder, ListClientsResponse, Request,
    ResolveTargetResponse, SessionName, Target, WindowTarget,
};
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::time::Duration;

#[test]
fn display_panes_cli_preserves_the_clients_canonical_session_spelling() {
    let canonical = SessionName::new("alpha\\literal").unwrap();
    let expected = canonical.clone();
    let socket =
        std::env::temp_dir().join(format!("display-canonical-{}.sock", std::process::id()));
    let listener = UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut decoder = FrameDecoder::new();
        let mut bytes = [0; 8192];
        loop {
            let request = loop {
                if let Some(request) = decoder.next_frame::<Request>().unwrap() {
                    break request;
                }
                let count = stream.read(&mut bytes).unwrap();
                assert_ne!(count, 0, "client closed before display-panes");
                decoder.push_bytes(&bytes[..count]);
            };
            let done = matches!(request, Request::DisplayPanes(_));
            let response =
                match request {
                    Request::ResolveTarget(_) => Response::ResolveTarget(ResolveTargetResponse {
                        target: Target::Session(SessionName::new("beta").unwrap()),
                    }),
                    Request::ListClients(request) => {
                        let output = request
                            .format
                            .unwrap()
                            .replace("#{client_pid}", "101")
                            .replace("#{client_name}", "/dev/ttys101")
                            .replace("#{client_tty}", "/dev/ttys101")
                            .replace("#{client_session}", canonical.as_str())
                            .replace("#{session_name}", canonical.as_str())
                            .replace("#{client_control_mode}", "0");
                        Response::ListClients(ListClientsResponse {
                            output: CommandOutput::from_stdout(output),
                            match_count: 1,
                        })
                    }
                    Request::DisplayPanes(request) => {
                        assert_eq!(request.target, canonical,
                        "the RPC must preserve the canonical name, including literal backslashes");
                        Response::DisplayPanes(DisplayPanesResponse {
                            target: WindowTarget::with_window(canonical.clone(), 0),
                            pane_count: 1,
                        })
                    }
                    other => panic!("unexpected request {other:?}"),
                };
            stream.write_all(&encode_frame(&response).unwrap()).unwrap();
            if done {
                break;
            }
        }
    });
    let command = crate::cli_args::parse(["rmux", "display-panes", "-bN", "-t", "101"])
        .unwrap()
        .command
        .unwrap();
    let startup = StartupOptions::new(
        true,
        rmux_client::AutoStartConfig::disabled(),
        super::super::StartupEndpoint::resolved(socket.clone()),
    );
    let result = dispatch(
        command,
        &socket,
        startup,
        ClientTerminalContext::default(),
        false,
        &mut None,
    );
    let joined = server.join();
    std::fs::remove_file(socket).unwrap();
    joined.unwrap();
    assert!(result.is_ok(), "{result:?}; expected {expected}");
}
