use super::*;
use std::{sync::Arc, time::Duration};
use tokio::sync::Notify;

#[path = "fnp1_print_clients.rs"]
mod clients;

#[path = "fnp1_print_intersession.rs"]
mod intersession;

#[derive(Default)]
struct PrintPause {
    reached: Notify,
    release: Notify,
}
tokio::task_local! { static BEFORE_PRINT: Arc<PrintPause>; }

impl RequestHandler {
    pub(in crate::handler) async fn pause_before_new_pane_print_for_test(&self) {
        if let Ok(pause) = BEFORE_PRINT.try_with(Arc::clone) {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }
}

async fn exercise(retire: bool) {
    for (alias, linked) in [(false, false), (true, false), (false, true)] {
        let handler = setup().await;
        if linked {
            create_background_identity_session(&handler, session_name("linked")).await;
            queue(&handler, "link-window -d -s fnp:0 -t linked:1")
                .await
                .unwrap();
        }
        let (session, index, target) = if linked {
            ("linked", 1, "linked:1.0")
        } else {
            ("fnp", 0, "fnp:0.0")
        };
        if alias {
            queue(
                &handler,
                "set-option -s command-alias[77] 'printchild=new-pane'",
            )
            .await
            .unwrap();
        }
        let mut args = vec![
            if alias { "printchild" } else { "newp" },
            "-d",
            "-t",
            target,
            "-x19",
            "-y5",
            "-X3",
            "-Y2",
            "-P",
        ];
        if !alias {
            args.extend(["-F", "#{pane_id}"]);
        }
        let pause = Arc::new(PrintPause::default());
        let waiting = Arc::clone(&pause);
        let cloned = handler.clone();
        let pending = tokio::spawn(
            BEFORE_PRINT.scope(waiting, async move { offer(&cloned, &args, true).await }),
        );
        tokio::time::timeout(Duration::from_secs(3), pause.reached.notified())
            .await
            .unwrap();
        let slot = PaneTarget::with_window(session_name(session), index, 1);
        let created = window(&handler).await.pane(1).unwrap().id();
        let pid = {
            let state = handler.state.lock().await;
            #[cfg(unix)]
            println!(
                "native PTY: {:?}",
                state
                    .pane_tty_path_in_window(slot.session_name(), index, 1)
                    .unwrap()
            );
            state
                .pane_pid_in_window(slot.session_name(), index, 1)
                .unwrap()
        };
        assert_ne!(pid, 0, "created child has a native process");
        {
            let mut state = handler.state.lock().await;
            super::super::frs1::swap_fixture_identities(
                &mut state.sessions,
                &rmux_proto::WindowTarget::with_window(session_name(session), index),
                0,
                1,
            );
        }
        assert_eq!(
            window(&handler).await.pane(0).unwrap().id(),
            created,
            "the print witness must move the created identity to another slot"
        );
        if retire {
            queue(&handler, &format!("kill-pane -t {session}:{index}.0"))
                .await
                .unwrap();
            queue(
                &handler,
                &format!("new-pane -d -t {target} -x19 -y5 -X3 -Y2"),
            )
            .await
            .unwrap();
            let replacement = window(&handler).await.pane(1).unwrap().id();
            assert_ne!(replacement, created, "same slot is genuinely reused");
            assert!(handler.state.lock().await.pane_lifecycle(created).is_none());
        } else {
            let state = handler.state.lock().await;
            assert_eq!(
                state
                    .pane_pid_in_window(slot.session_name(), index, 0)
                    .unwrap(),
                pid
            );
        }
        pause.release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(3), pending)
            .await
            .unwrap()
            .unwrap();
        println!(
            "retire={retire} alias={alias} linked={linked} created=%{} pid={pid} result={result:?}",
            created.as_u32()
        );
        cleanup(&handler).await;
        let RuntimeCommandExecutionResponse::Complete { stdout, error, .. } = result else {
            panic!("not executed: {result:?}");
        };
        if retire {
            assert!(error.is_some(), "must not print replacement: {stdout:?}");
            assert!(stdout.is_empty());
        } else {
            assert!(error.is_none(), "{error:?}");
            let expected = if alias {
                b"fnp:0.0\n".to_vec()
            } else {
                format!("%{}\n", created.as_u32()).into_bytes()
            };
            assert_eq!(stdout, expected, "print the committed PaneId after swap");
        }
    }
}

#[tokio::test]
async fn fnp1_r1_print_follows_created_identity_after_swap() {
    exercise(false).await;
}

#[tokio::test]
async fn fnp1_r1_print_rejects_retired_identity_after_slot_reuse() {
    exercise(true).await;
}

#[tokio::test]
async fn fnp1_print_keeps_invoking_control_client_with_newer_peer() {
    let handler = setup().await;
    let caller = 429_001;
    let peer = caller + 1;
    let (_caller_id, _caller_events) =
        register_control_for_session(&handler, caller, session_name("fnp")).await;
    let (_peer_id, _peer_events) =
        register_control_for_session(&handler, peer, session_name("fnp")).await;
    for (pid, size) in [(caller, "91x31"), (peer, "117x41")] {
        let parsed = handler
            .parse_control_commands(&format!("refresh-client -C {size}"))
            .await
            .unwrap();
        let refreshed = handler.execute_control_commands(pid, parsed).await;
        assert!(refreshed.error.is_none(), "{refreshed:?}");
    }
    let format = "#{client_name}|#{client_pid}|#{client_width}";
    let baseline = handler
        .execute_control_commands(
            caller,
            handler
                .parse_control_commands(&format!("display-message -p -t fnp:0.0 '{format}'"))
                .await
                .unwrap(),
        )
        .await;
    assert!(baseline.error.is_none(), "{baseline:?}");
    // tmux 3.7b: A's name, pid and declared width survive the arrival of B.
    assert_eq!(
        baseline.stdout,
        format!("client-{caller}|{caller}|91\n").as_bytes()
    );
    let result = handler
        .execute_control_commands(
            caller,
            handler
                .parse_control_commands(&format!(
                    "new-pane -d -t fnp:0.0 -x19 -y5 -X3 -Y2 -P -F '{format}'"
                ))
                .await
                .unwrap(),
        )
        .await;
    cleanup(&handler).await;
    assert!(result.error.is_none(), "{result:?}");
    assert_eq!(
        result.stdout, baseline.stdout,
        "print must keep caller A, not newer peer B"
    );
}
