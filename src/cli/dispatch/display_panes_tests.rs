use super::*;
use rmux_proto::{
    encode_frame, CommandOutput, DisplayPanesResponse, FrameDecoder, ListClientsResponse, Request,
    ResolveTargetResponse, SessionName, Target, WindowTarget,
};
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::time::Duration;

#[test]
fn display_panes_cli_client_target_serializes_the_resolved_clients_session() {
    for (index, client) in ["101", "/dev/ttys101", "ttys101:"].into_iter().enumerate() {
        let path = std::env::temp_dir().join(format!(
            "display-client-{}-{index}.sock",
            std::process::id()
        ));
        let listener = UnixListener::bind(&path).unwrap();
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
                let response = match request {
                    Request::ResolveTarget(_) => Response::ResolveTarget(ResolveTargetResponse {
                        target: Target::Session(SessionName::new("beta").unwrap()),
                    }),
                    Request::ListClients(request) => {
                        let format = request.format.expect("client session format");
                        let mut output = String::new();
                        for (pid, name, session) in [
                            ("101", "/dev/ttys101", "alpha"),
                            ("202", "/dev/ttys202", "beta"),
                        ] {
                            output.push_str(
                                &format
                                    .replace("#{client_pid}", pid)
                                    .replace("#{client_name}", name)
                                    .replace("#{client_tty}", name)
                                    .replace("#{client_session}", session)
                                    .replace("#{session_name}", session)
                                    .replace("#{client_control_mode}", "0"),
                            );
                        }
                        Response::ListClients(ListClientsResponse {
                            output: CommandOutput::from_stdout(output),
                            match_count: 2,
                        })
                    }
                    Request::DisplayPanes(request) => {
                        assert_eq!(
                            request.target,
                            SessionName::new("alpha").unwrap(),
                            "-t alpha client must not serialize the implicit beta session"
                        );
                        assert_eq!(request.target_client.as_deref(), Some(client));
                        Response::DisplayPanes(DisplayPanesResponse {
                            target: WindowTarget::with_window(
                                SessionName::new("alpha").unwrap(),
                                0,
                            ),
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
        let command = crate::cli_args::parse(["rmux", "display-panes", "-bN", "-t", client])
            .unwrap()
            .command
            .unwrap();
        let startup = StartupOptions::new(
            true,
            rmux_client::AutoStartConfig::disabled(),
            super::super::StartupEndpoint::resolved(path.clone()),
        );
        let result = dispatch(
            command,
            &path,
            startup,
            ClientTerminalContext::default(),
            false,
            &mut None,
        );
        let joined = server.join();
        std::fs::remove_file(path).unwrap();
        joined.unwrap();
        assert!(result.is_ok(), "{result:?}");
    }
}
