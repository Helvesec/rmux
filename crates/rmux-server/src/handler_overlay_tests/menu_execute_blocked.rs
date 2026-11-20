use super::*;
use crate::pane_io::{forward_attach, LiveAttachInputContext};
use rmux_core::{input::InputParser, Screen};
use rmux_proto::{encode_attach_message, AttachFrameDecoder, AttachMessage};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::watch;

// The measured tmux 3.7b surface restores the pane while wait-for is still
// blocked. Receiving a queued clear after the command finishes is insufficient.
#[tokio::test]
async fn popup_repaint_menu_execute_restores_before_wait_for_release() {
    for nested in [false, true] {
        for mouse in [false, true] {
            let handler = Arc::new(RequestHandler::new());
            let name = session_name("menu-blocked-repaint");
            let pid = 920_246;
            let peer = rmux_ipc::PeerIdentity {
                pid,
                uid: crate::server_access::current_owner_uid(),
                user: handler.server_owner_identity(),
            };
            let admission = handler
                .server_access_admission_for_peer(&peer)
                .expect("the server owner is admitted for fixture setup");
            let setup_access = handler.begin_authenticated_peer_access(&peer, admission);
            run_overlay_command(
                &handler,
                pid,
                "new-session -d -s menu-blocked-repaint 'printf BLOCKING_FINAL; exec sleep 60'; \
                 set -g status off; set -g automatic-rename off",
            )
            .await;
            timeout(Duration::from_secs(5), async {
                loop {
                    let parsed = handler
                        .parse_control_commands("capture-pane -p -t menu-blocked-repaint")
                        .await
                        .unwrap();
                    let captured = handler
                        .execute_parsed_commands_for_test(pid, parsed)
                        .await
                        .unwrap();
                    if String::from_utf8_lossy(captured.stdout()).contains("BLOCKING_FINAL") {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("the pane reaches its final output before opening the menu");
            let (tx, mut rx) = mpsc::unbounded_channel();
            handler.register_attach(pid, name.clone(), tx).await;
            // The menu now runs under the registered attach's own write authority.
            drop(setup_access);
            let (identity, backlog, closing, epoch) = {
                let attaches = handler.active_attach.lock().await;
                let active = &attaches.by_pid[&pid];
                (
                    active.identity(pid),
                    Arc::clone(&active.control_backlog),
                    Arc::clone(&active.closing),
                    Arc::clone(&active.persistent_overlay_epoch),
                )
            };
            handler.refresh_attached_client(pid, &name).await;
            let target = timeout(Duration::from_secs(2), async {
                loop {
                    let control = rx.recv().await.expect("initial attach control");
                    crate::pane_io::release_attach_control_backlog(
                        &backlog,
                        control.received_backlog_units(),
                    );
                    if let AttachControl::Switch(target) = control.materialize_received() {
                        break *target.into_target();
                    }
                }
            })
            .await
            .expect("initial pane refresh");
            handler.clear_attached_render_refresh_pending(pid).await;
            let (stream, mut peer) = UnixStream::pair().unwrap();
            let (shutdown, shutdown_rx) = watch::channel(());
            let attach = tokio::spawn(forward_attach(
                stream,
                target,
                Vec::new(),
                shutdown_rx,
                rx,
                backlog,
                closing,
                epoch,
                LiveAttachInputContext::new(Arc::clone(&handler), identity),
                false,
            ));
            let mut decoder = AttachFrameDecoder::new();
            let mut frames = read_output(&mut peer, &mut decoder, b"BLOCKING_FINAL").await;
            if nested {
                run_overlay_command(&handler, pid, "display-popup -N -T Underlying").await;
                frames.extend(read_output(&mut peer, &mut decoder, b"Underlying").await);
            }
            run_overlay_command(
                &handler,
                pid,
                "display-menu -M -T Selection -x 0 -y 0 Go g 'wait-for blocked'",
            )
            .await;
            frames.extend(read_output(&mut peer, &mut decoder, b"Selection").await);
            let rect = {
                let attaches = handler.active_attach.lock().await;
                match attaches.by_pid[&pid].overlay.as_ref().unwrap() {
                    ClientOverlayState::Menu(menu) => menu.rect,
                    ClientOverlayState::Popup(popup) => popup.nested_menu.as_ref().unwrap().rect,
                }
            };
            let input = if mouse {
                sgr_mouse(3, rect.x + 1, rect.y + 1)
            } else {
                b"g".to_vec()
            };
            peer.write_all(&encode_attach_message(&AttachMessage::Data(input)).unwrap())
                .await
                .unwrap();
            timeout(Duration::from_secs(2), async {
                while handler.wait_for_counts("blocked").0 != 1 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("selection enters wait-for without any release signal");
            let restored = if nested {
                b"Underlying".as_slice()
            } else {
                b"BLOCKING_FINAL".as_slice()
            };
            frames.extend(read_output(&mut peer, &mut decoder, restored).await);
            assert_eq!(handler.wait_for_counts("blocked").0, 1);
            let mut screen = Screen::new(TerminalSize { cols: 80, rows: 24 }, 0);
            InputParser::new().parse(&frames, &mut screen);
            let visible = String::from_utf8(
                screen.capture_transcript(Default::default(), Default::default()),
            )
            .unwrap();
            assert!(!visible.contains("Selection"), "{visible:?}");
            assert!(
                visible.contains(std::str::from_utf8(restored).unwrap()),
                "{visible:?}"
            );
            // Only now may the command finish. No status tick, pane output or
            // command completion can make the assertion above pass.
            run_overlay_command(&handler, pid, "wait-for -S blocked").await;
            shutdown.send(()).unwrap();
            timeout(Duration::from_secs(2), attach)
                .await
                .expect("attach exits after release")
                .unwrap()
                .unwrap();
            run_overlay_command(&handler, pid, "kill-session -t menu-blocked-repaint").await;
        }
    }
}

async fn read_output(
    peer: &mut UnixStream,
    decoder: &mut AttachFrameDecoder,
    needle: &[u8],
) -> Vec<u8> {
    timeout(Duration::from_secs(2), async {
        let mut output = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            let count = peer.read(&mut buffer).await.unwrap();
            assert!(count > 0, "attach closed before repaint");
            decoder.push_bytes(&buffer[..count]);
            while let Some(message) = decoder.next_message().unwrap() {
                if let AttachMessage::Data(bytes) | AttachMessage::Render(bytes) = message {
                    output.extend(bytes);
                }
            }
            if output.windows(needle.len()).any(|part| part == needle) {
                return output;
            }
        }
    })
    .await
    .expect("the underlying surface must be transmitted before releasing wait-for")
}
