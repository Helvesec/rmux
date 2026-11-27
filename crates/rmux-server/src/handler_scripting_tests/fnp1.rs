use super::*;
#[path = "fkey1.rs"]
mod fkey1;
#[path = "fne1.rs"]
mod fne1;
#[path = "fnp2.rs"]
mod fnp2;
#[path = "fnp3.rs"]
mod fnp3;
#[path = "fnp4.rs"]
mod fnp4;
#[path = "fopen1.rs"]
mod fopen1;
#[path = "fstdin1.rs"]
mod fstdin1;
#[path = "fnp1_lifecycle.rs"]
mod lifecycle;
#[path = "fnp1_print.rs"]
mod print;

async fn setup() -> RequestHandler {
    let handler = RequestHandler::new();
    run_hook_probe_queue(&handler, "set-option -g status off").await;
    #[cfg(unix)]
    run_hook_probe_queue(&handler, "set-option -g default-shell /bin/sh").await;
    create_background_identity_session(&handler, session_name("fnp")).await;
    handler
}

async fn queue(
    handler: &RequestHandler,
    command: &str,
) -> Result<rmux_proto::CommandOutput, RmuxError> {
    eprintln!("resize-test queue enter command={command:?}");
    let before_resize = if command.starts_with("resize-pane ") {
        Some(window(handler).await)
    } else {
        None
    };
    let result = handler
        .execute_parsed_commands_for_test(
            std::process::id(),
            CommandParser::new().parse(command).unwrap(),
        )
        .await;
    eprintln!("resize-test queue return command={command:?} result={result:?}");
    if let Some(before) = before_resize {
        let after = window(handler).await;
        eprintln!(
            "resize-test geometry unchanged={} before={before:?} after={after:?}",
            before == after
        );
    }
    result
}

async fn offer(
    handler: &RequestHandler,
    args: &[&str],
    capability: bool,
) -> RuntimeCommandExecutionResponse {
    let mut request =
        RuntimeCommandExecutionRequest::new(args.iter().map(|s| (*s).to_owned()).collect(), None);
    if capability {
        request = request.requiring_floating();
    }
    execution_result(
        handler
            .handle(Request::RuntimeCommandExecution(Box::new(request)))
            .await,
    )
}

async fn window(handler: &RequestHandler) -> rmux_core::Window {
    handler
        .state
        .lock()
        .await
        .sessions
        .session(&session_name("fnp"))
        .unwrap()
        .window_at(0)
        .unwrap()
        .clone()
}

async fn cleanup(handler: &RequestHandler) {
    assert!(matches!(
        handler
            .handle(Request::KillServer(rmux_proto::KillServerRequest))
            .await,
        Response::KillServer(_)
    ));
}

#[tokio::test]
async fn fnp1_direct_creation_is_not_a_split_and_detached_does_not_reselect() {
    let handler = setup().await;
    queue(&handler, "split-window -h -d -l 1 -t fnp:0.0")
        .await
        .unwrap();
    let before = window(&handler).await;
    assert_eq!(before.pane(1).unwrap().geometry().cols(), 1);
    assert!(queue(&handler, "split-window -h -t fnp:0.1").await.is_err());
    queue(&handler, "newp -t fnp:0.1 -x30 -y8 -X3 -Y2")
        .await
        .unwrap();
    let a = window(&handler).await;
    assert_eq!(a.active_pane_index(), 2);
    for old in before.panes() {
        assert_eq!(a.pane(old.index()).unwrap().id(), old.id());
        assert_eq!(a.pane(old.index()).unwrap().geometry(), old.geometry());
    }
    queue(&handler, "new-pane -d -t fnp:0.0 -x19 -y5 -X-1 -Y-2")
        .await
        .unwrap();
    let b = window(&handler).await;
    assert_eq!(b.active_pane_index(), 2);
    assert_eq!(b.last_pane_index(), a.last_pane_index());
    assert_eq!(
        b.floating_stack().placements().last().unwrap().pane_id(),
        b.pane(3).unwrap().id()
    );
    queue(&handler, "select-pane -t fnp:0.2").await.unwrap();
    assert_eq!(
        window(&handler)
            .await
            .floating_stack()
            .placements()
            .last()
            .unwrap()
            .pane_id(),
        a.pane(2).unwrap().id()
    );
    let printed = offer(
        &handler,
        &[
            "newp",
            "-dP",
            "-t",
            "fnp:0.0",
            "-x",
            "10",
            "-y",
            "4",
            "-X",
            "-10",
            "-Y",
            "24",
            "-F",
            "#{pane_floating_flag}|#{pane_width}x#{pane_height}@#{pane_left},#{pane_top}",
        ],
        true,
    )
    .await;
    assert!(
        matches!(printed, RuntimeCommandExecutionResponse::Complete { ref stdout, error: None, exit_status: 0, .. } if stdout == b"1|10x4@-10,24\n"),
        "{printed:?}"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn fnp1_paired_bounds_and_syntax_rejections_are_atomic() {
    let handler = setup().await;
    for (geometry, valid) in [
        ("-x79 -y8 -X0 -Y0", true),
        ("-x80 -y8 -X0 -Y0", false),
        ("-x30 -y23 -X0 -Y0", true),
        ("-x30 -y24 -X0 -Y0", false),
        ("-x30 -y8 -X-30 -Y0", true),
        ("-x30 -y8 -X-31 -Y0", false),
        ("-x30 -y8 -X80 -Y0", true),
        ("-x30 -y8 -X81 -Y0", false),
        ("-x30 -y8 -X0 -Y-8", true),
        ("-x30 -y8 -X0 -Y-9", false),
        ("-x30 -y8 -X0 -Y24", true),
        ("-x30 -y8 -X0 -Y25", false),
        ("-x0 -y8 -X0 -Y0", false),
        // FNP2: 30% of the 80-column fixture window is 24 cells.
        ("-x30% -y8 -X0 -Y0", true),
        ("-x30 -y0 -X0 -Y0", false),
        ("-xNaN -y8 -X0 -Y0", false),
        // FOPEN1: a single explicit axis is now a valid form, the other cascades.
        ("-x30 -y8 -X0", true),
        ("-x30 -y8 -X0 -Y0 -x30", false),
        ("-x30 -y8 -X0 -Y0 -I", false),
        // FKEY1: `-k` is one of this command's own options in the source
        // (`cmd-split-window.c:41`), and it keeps the float when its command
        // ends. It is accepted here and writes the pane's own policy.
        ("-x30 -y8 -X0 -Y0 -k", true),
        ("-x30 -y8 -X0 -Y0 ''", false),
        ("-x30 -y8 -X0 -Y0 -F x", false),
        ("-x2147483648 -y8 -X0 -Y0", false),
        ("-x30 -y8 -X0 -Y0 -t fnp:0.0", false),
    ] {
        let before = window(&handler).await;
        let result = queue(&handler, &format!("new-pane -d -t fnp:0.0 {geometry}")).await;
        assert_eq!(result.is_ok(), valid, "{geometry}: {result:?}");
        let after = window(&handler).await;
        assert_eq!(
            after.panes().len(),
            before.panes().len() + usize::from(valid)
        );
        assert_eq!(after.active_pane_index(), before.active_pane_index());
        if !valid {
            assert_eq!(after.floating_stack(), before.floating_stack());
        }
    }
    // An omitted target is no longer a refusal: FOPEN1 resolves it to the pane
    // one is on, the way tmux declares this target. What is refused is a target
    // that names nothing.
    let before = window(&handler).await;
    assert!(queue(&handler, "new-pane -d -x30 -y8 -X0 -Y0")
        .await
        .is_ok());
    assert_eq!(
        window(&handler).await.panes().len(),
        before.panes().len() + 1
    );
    assert!(queue(&handler, "new-pane -t %999 -x30 -y8 -X0 -Y0")
        .await
        .is_err());
    cleanup(&handler).await;
}

#[tokio::test]
async fn fnp1_offer_capabilities_aliases_and_batch_refusal_precede_effects() {
    let handler = setup().await;
    let args = ["newp", "-t", "fnp:0.0", "-x30", "-y8", "-X0", "-Y0"];
    assert!(matches!(
        offer(&handler, &args, false).await,
        RuntimeCommandExecutionResponse::Unsupported(_)
    ));
    set_alias(&handler, "probe=new-pane -t fnp:0.0 -x30 -y8 -X0 -Y0").await;
    assert!(matches!(
        offer(&handler, &["probe"], false).await,
        RuntimeCommandExecutionResponse::Unsupported(_)
    ));
    let batch = [
        "set-environment",
        "-g",
        "FNP_BAD",
        "yes",
        ";",
        "newp",
        "-t",
        "fnp:0.0",
        "-x30",
        "-y8",
        "-X0",
        "-Y0",
    ];
    assert!(matches!(
        offer(&handler, &batch, true).await,
        RuntimeCommandExecutionResponse::Unsupported(_)
    ));
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .environment
            .global_value("FNP_BAD"),
        None
    );
    assert_eq!(window(&handler).await.panes().len(), 1);
    assert!(matches!(
        offer(&handler, &["probe"], true).await,
        RuntimeCommandExecutionResponse::Complete {
            error: None,
            exit_status: 0,
            ..
        }
    ));
    set_alias(&handler, "probe=set-environment -g FNP_BAD changed").await;
    assert!(matches!(
        offer(&handler, &["probe"], true).await,
        RuntimeCommandExecutionResponse::Unsupported(_)
    ));
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .environment
            .global_value("FNP_BAD"),
        None
    );
    cleanup(&handler).await;
}
