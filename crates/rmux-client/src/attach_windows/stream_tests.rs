use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::io::FromRawHandle;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rmux_proto::{
    encode_attach_message, AttachFrameDecoder, AttachMessage, AttachShellCommand,
    AttachedKeystroke, AttachedWindowsConsoleKey, TerminalSize,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Pipes::CreatePipe;

use super::super::action::{run_attach_action, AttachActionExecutor};
use super::super::output_worker::AttachOutputWorker;
use super::super::terminal_cleanup::fallback_attach_stop_sequence;
use super::*;
use crate::attach_lock_state::AttachLockState;

#[tokio::test]
async fn lock_request_runs_action_and_sends_unlock() -> Result<(), Box<dyn std::error::Error>> {
    let mut scenario = AttachScenario::new(RecordingActions::default());
    let actions = scenario.actions.clone();
    let mut server = scenario.take_server();

    write_server_message(&mut server, AttachMessage::Lock("echo locked".to_owned())).await?;

    assert_eq!(
        read_client_message(&mut server).await?,
        AttachMessage::Unlock
    );

    write_server_message(&mut server, AttachMessage::DetachKill).await?;
    scenario.join().await?;

    assert_eq!(actions.calls(), vec!["lock:echo locked", "detach-kill"]);
    Ok(())
}

#[tokio::test]
async fn lock_shell_request_runs_action_and_sends_unlock() -> Result<(), Box<dyn std::error::Error>>
{
    let mut scenario = AttachScenario::new(RecordingActions::default());
    let actions = scenario.actions.clone();
    let mut server = scenario.take_server();

    write_server_message(
        &mut server,
        AttachMessage::LockShellCommand(AttachShellCommand::new(
            "echo locked".to_owned(),
            "pwsh.exe".to_owned(),
            r"C:\work".to_owned(),
        )),
    )
    .await?;

    assert_eq!(
        read_client_message(&mut server).await?,
        AttachMessage::Unlock
    );

    write_server_message(&mut server, AttachMessage::DetachKill).await?;
    scenario.join().await?;

    assert_eq!(actions.calls(), vec!["lock:echo locked", "detach-kill"]);
    Ok(())
}

#[tokio::test]
async fn suspend_request_runs_action_and_sends_unlock() -> Result<(), Box<dyn std::error::Error>> {
    let mut scenario = AttachScenario::new(RecordingActions::default());
    let actions = scenario.actions.clone();
    let mut server = scenario.take_server();

    write_server_message(&mut server, AttachMessage::Suspend).await?;

    assert_eq!(
        read_client_message(&mut server).await?,
        AttachMessage::Unlock
    );

    write_server_message(&mut server, AttachMessage::DetachKill).await?;
    scenario.join().await?;

    assert_eq!(actions.calls(), vec!["suspend", "detach-kill"]);
    Ok(())
}

#[tokio::test]
async fn lock_and_suspend_unlock_paths_rearm_attach_screen(
) -> Result<(), Box<dyn std::error::Error>> {
    let screen_tracker = AttachScreenTracker::default();
    let mut scenario = AttachScenario::with_screen_tracker(
        RecordingActions::default(),
        true,
        screen_tracker.clone(),
    );
    let actions = scenario.actions.clone();
    let mut server = scenario.take_server();
    let requests = [
        AttachMessage::Lock("legacy".to_owned()),
        AttachMessage::LockShellCommand(AttachShellCommand::new(
            "structured".to_owned(),
            "pwsh.exe".to_owned(),
            r"C:\work".to_owned(),
        )),
        AttachMessage::Suspend,
    ];

    for request in requests {
        screen_tracker.mark_stopped();
        write_server_message(&mut server, request).await?;
        assert_eq!(
            read_client_message(&mut server).await?,
            AttachMessage::Unlock
        );
        assert!(
            !screen_tracker.was_stopped(),
            "successful resume must make a later EOF/reset abnormal again"
        );
    }

    write_server_message(&mut server, AttachMessage::DetachKill).await?;
    scenario.join().await?;
    assert_eq!(
        actions.calls(),
        vec!["lock:legacy", "lock:structured", "suspend", "detach-kill"]
    );
    Ok(())
}

#[tokio::test]
async fn lock_completion_unlocks_while_preserving_a_concurrent_final_stop(
) -> Result<(), Box<dyn std::error::Error>> {
    let (client_stream, mut server) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(client_stream);
    let (input_tx, input_rx) = mpsc::channel(1);
    let (_resize_tx, resize_rx) = mpsc::unbounded_channel();
    let (action_tx, action_rx) = std::sync::mpsc::channel();
    let (completion_tx, completion_rx) = mpsc::unbounded_channel();
    let locked = Arc::new(AttachLockState::default());
    let screen_tracker = AttachScreenTracker::default();
    let client_tracker = screen_tracker.clone();
    let client = tokio::spawn(async move {
        drive_async_attach(
            reader,
            writer,
            Vec::new(),
            SharedOutput::default(),
            client_tracker,
            AttachAsyncChannels::new(input_rx, resize_rx, action_tx, completion_rx, locked, true),
        )
        .await
    });

    write_server_message(
        &mut server,
        AttachMessage::Data(ALT_SCREEN_EXIT_FALLBACK.to_vec()),
    )
    .await?;
    write_server_message(&mut server, AttachMessage::Lock("block".to_owned())).await?;

    let (action_rx, action) = receive_attach_action(action_rx).await?;
    assert!(matches!(action, AttachAction::LegacyLock(command) if command == "block"));
    let lock_prelude = wait_for_stop_generation(&screen_tracker, None).await?;

    write_server_message(
        &mut server,
        AttachMessage::Data(ALT_SCREEN_EXIT_FALLBACK.to_vec()),
    )
    .await?;
    let final_stop = wait_for_stop_generation(&screen_tracker, Some(lock_prelude)).await?;

    completion_tx.send(Ok(AttachActionOutcome::Unlock))?;
    assert_eq!(
        read_client_message(&mut server).await?,
        AttachMessage::Unlock,
        "a completed lock must always be acknowledged"
    );
    assert_eq!(screen_tracker.current_stop_generation(), Some(final_stop));
    input_tx
        .send(super::super::input::AttachInput::bytes(
            b"after-lock".to_vec(),
        ))
        .await?;
    assert_eq!(
        read_client_message(&mut server).await?,
        AttachMessage::Keystroke(AttachedKeystroke::new(b"after-lock".to_vec())),
        "a newer stop must not leave the attach input lock wedged"
    );
    assert!(
        !client.is_finished(),
        "a final stop may be followed by a detach action in a later frame"
    );

    let command = AttachShellCommand::new(
        "echo bye".to_owned(),
        "pwsh.exe".to_owned(),
        r"C:\work".to_owned(),
    );
    write_server_message(
        &mut server,
        AttachMessage::DetachExecShellCommand(command.clone()),
    )
    .await?;
    let (_action_rx, final_action) = receive_attach_action(action_rx).await?;
    assert!(
        matches!(final_action, AttachAction::DetachExec(received) if received.command() == command.command() && received.shell() == command.shell() && received.cwd() == command.cwd())
    );
    completion_tx.send(Ok(AttachActionOutcome::Exit))?;
    timeout(client).await???;
    Ok(())
}

#[tokio::test]
async fn lock_completion_preserves_a_newer_resumable_stop() -> Result<(), Box<dyn std::error::Error>>
{
    let (client_stream, mut server) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(client_stream);
    let (_input_tx, input_rx) = mpsc::channel(1);
    let (_resize_tx, resize_rx) = mpsc::unbounded_channel();
    let (action_tx, action_rx) = std::sync::mpsc::channel();
    let (completion_tx, completion_rx) = mpsc::unbounded_channel();
    let locked = Arc::new(AttachLockState::default());
    let screen_tracker = AttachScreenTracker::default();
    let client_tracker = screen_tracker.clone();
    let client = tokio::spawn(async move {
        drive_async_attach(
            reader,
            writer,
            Vec::new(),
            SharedOutput::default(),
            client_tracker,
            AttachAsyncChannels::new(input_rx, resize_rx, action_tx, completion_rx, locked, true),
        )
        .await
    });

    write_server_message(
        &mut server,
        AttachMessage::Data(ALT_SCREEN_EXIT_FALLBACK.to_vec()),
    )
    .await?;
    write_server_message(&mut server, AttachMessage::Lock("block".to_owned())).await?;
    let (action_rx, first_action) = receive_attach_action(action_rx).await?;
    assert!(matches!(
        first_action,
        AttachAction::LegacyLock(command) if command == "block"
    ));
    let lock_stop = wait_for_stop_generation(&screen_tracker, None).await?;

    write_server_message(
        &mut server,
        AttachMessage::Data(ALT_SCREEN_EXIT_FALLBACK.to_vec()),
    )
    .await?;
    write_server_message(&mut server, AttachMessage::Suspend).await?;
    let (action_rx, second_action) = receive_attach_action(action_rx).await?;
    assert!(matches!(second_action, AttachAction::Suspend));
    let suspend_stop = wait_for_stop_generation(&screen_tracker, Some(lock_stop)).await?;

    completion_tx.send(Ok(AttachActionOutcome::Unlock))?;
    assert_eq!(
        read_client_message(&mut server).await?,
        AttachMessage::Unlock
    );
    assert_eq!(
        screen_tracker.current_stop_generation(),
        Some(suspend_stop),
        "the older completion must not consume the newer resumable stop"
    );
    assert!(
        !client.is_finished(),
        "the attach must await the newer action"
    );

    completion_tx.send(Ok(AttachActionOutcome::Unlock))?;
    assert_eq!(
        read_client_message(&mut server).await?,
        AttachMessage::Unlock
    );
    assert!(
        !screen_tracker.was_stopped(),
        "the matching completion must rearm the attach"
    );

    write_server_message(&mut server, AttachMessage::DetachKill).await?;
    let (_action_rx, final_action) = receive_attach_action(action_rx).await?;
    assert!(matches!(final_action, AttachAction::DetachKill));
    completion_tx.send(Ok(AttachActionOutcome::Exit))?;
    timeout(client).await???;
    Ok(())
}

#[tokio::test]
async fn detach_exec_runs_action_before_exit() -> Result<(), Box<dyn std::error::Error>> {
    let mut scenario = AttachScenario::new(RecordingActions::default());
    let actions = scenario.actions.clone();
    let mut server = scenario.take_server();

    write_server_message(
        &mut server,
        AttachMessage::DetachExec("echo bye".to_owned()),
    )
    .await?;
    scenario.join().await?;

    assert_eq!(actions.calls(), vec!["detach-exec:echo bye"]);
    Ok(())
}

#[tokio::test]
async fn detach_exec_shell_runs_action_before_exit() -> Result<(), Box<dyn std::error::Error>> {
    let mut scenario = AttachScenario::new(RecordingActions::default());
    let actions = scenario.actions.clone();
    let mut server = scenario.take_server();

    write_server_message(
        &mut server,
        AttachMessage::DetachExecShellCommand(AttachShellCommand::new(
            "echo bye".to_owned(),
            "pwsh.exe".to_owned(),
            r"C:\work".to_owned(),
        )),
    )
    .await?;
    scenario.join().await?;

    assert_eq!(actions.calls(), vec!["detach-exec:echo bye"]);
    Ok(())
}

#[tokio::test]
async fn every_exclusive_terminal_action_waits_for_prior_output(
) -> Result<(), Box<dyn std::error::Error>> {
    let cases = [
        (
            AttachMessage::Lock("legacy-lock".to_owned()),
            "lock:legacy-lock",
            true,
        ),
        (
            AttachMessage::LockShellCommand(AttachShellCommand::new(
                "structured-lock".to_owned(),
                "pwsh.exe".to_owned(),
                r"C:\work".to_owned(),
            )),
            "lock:structured-lock",
            true,
        ),
        (AttachMessage::Suspend, "suspend", true),
        (AttachMessage::DetachKill, "detach-kill", false),
        (
            AttachMessage::DetachExec("legacy-detach".to_owned()),
            "detach-exec:legacy-detach",
            false,
        ),
        (
            AttachMessage::DetachExecShellCommand(AttachShellCommand::new(
                "structured-detach".to_owned(),
                "pwsh.exe".to_owned(),
                r"C:\work".to_owned(),
            )),
            "detach-exec:structured-detach",
            false,
        ),
    ];

    for (message, expected_call, resumable) in cases {
        assert_exclusive_action_waits_for_output(message, expected_call, resumable).await?;
    }
    Ok(())
}

#[tokio::test]
async fn buffered_vt_tail_is_flushed_before_exclusive_action(
) -> Result<(), Box<dyn std::error::Error>> {
    let (client_stream, mut server) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(client_stream);
    let (_input_tx, input_rx) = mpsc::channel(8);
    let (_resize_tx, resize_rx) = mpsc::unbounded_channel();
    let locked = Arc::new(AttachLockState::default());
    let actions = RecordingActions::default();
    let client_actions = actions.clone();
    let (action_tx, action_rx) = std::sync::mpsc::channel();
    let (completion_tx, completion_rx) = mpsc::unbounded_channel();
    let action_worker: std::thread::JoinHandle<std::result::Result<(), crate::ClientError>> =
        std::thread::spawn(move || {
            let mut actions = client_actions;
            while let Ok(action) = action_rx.recv() {
                if completion_tx
                    .send(run_attach_action(&mut actions, action))
                    .is_err()
                {
                    return Ok(());
                }
            }
            Ok(())
        });
    let (output, captured, fence_flushed) = BufferedVtTailOutput::new();
    let client = tokio::spawn(async move {
        drive_async_attach_with_output_fence(
            reader,
            writer,
            Vec::new(),
            output,
            AttachScreenTracker::default(),
            AttachAsyncChannels::new(input_rx, resize_rx, action_tx, completion_rx, locked, true),
            BufferedVtTailOutput::flush_output_fence,
        )
        .await
    });

    write_server_message(
        &mut server,
        AttachMessage::Data(b"visible-before-tail\x1b[?".to_vec()),
    )
    .await?;
    write_server_message(&mut server, AttachMessage::Lock("barrier".to_owned())).await?;

    actions
        .wait_for_call("lock:barrier", Duration::from_secs(1))
        .await?;
    assert!(fence_flushed.load(Ordering::SeqCst));
    assert_eq!(
        *captured.lock().expect("captured output mutex poisoned"),
        b"visible-before-tail\x1b[?",
        "the action fence must finalize the VT scanner's pending tail"
    );
    assert_eq!(
        read_client_message(&mut server).await?,
        AttachMessage::Unlock
    );
    write_server_message(&mut server, AttachMessage::DetachKill).await?;
    timeout(client).await???;
    action_worker
        .join()
        .map_err(|_| io::Error::other("action worker panicked"))??;
    Ok(())
}

async fn assert_exclusive_action_waits_for_output(
    message: AttachMessage,
    expected_call: &str,
    resumable: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (client_stream, mut server) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(client_stream);
    let (_input_tx, input_rx) = mpsc::channel(8);
    let (_resize_tx, resize_rx) = mpsc::unbounded_channel();
    let locked = Arc::new(AttachLockState::default());
    let client_locked = Arc::clone(&locked);
    let actions = RecordingActions::default();
    let client_actions = actions.clone();
    let (action_tx, action_rx) = std::sync::mpsc::channel();
    let (completion_tx, completion_rx) = mpsc::unbounded_channel();
    let action_worker: std::thread::JoinHandle<std::result::Result<(), crate::ClientError>> =
        std::thread::spawn(move || {
            let mut actions = client_actions;
            while let Ok(action) = action_rx.recv() {
                if completion_tx
                    .send(run_attach_action(&mut actions, action))
                    .is_err()
                {
                    return Ok(());
                }
            }
            Ok(())
        });
    let (output, write_started_rx, release_tx) = BlockingOutput::new();
    let captured = Arc::clone(&output.bytes);
    let client = tokio::spawn(async move {
        drive_async_attach(
            reader,
            writer,
            Vec::new(),
            output,
            AttachScreenTracker::default(),
            AttachAsyncChannels::new(
                input_rx,
                resize_rx,
                action_tx,
                completion_rx,
                client_locked,
                true,
            ),
        )
        .await
    });

    write_server_message(
        &mut server,
        AttachMessage::Data(b"output-before-action".to_vec()),
    )
    .await?;
    wait_for_blocking_output_start(write_started_rx).await?;
    write_server_message(&mut server, message).await?;
    wait_for_attach_lock(&locked).await?;
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        actions.calls().is_empty(),
        "{expected_call} overtook output blocked in the writer"
    );

    release_tx.send(()).expect("release blocked output");
    actions
        .wait_for_call(expected_call, Duration::from_secs(1))
        .await?;
    assert_eq!(
        *captured.lock().expect("output mutex poisoned"),
        b"output-before-action"
    );

    if resumable {
        assert_eq!(
            read_client_message(&mut server).await?,
            AttachMessage::Unlock
        );
        write_server_message(&mut server, AttachMessage::DetachKill).await?;
    }
    timeout(client).await???;
    action_worker
        .join()
        .map_err(|_| io::Error::other("action worker panicked"))??;
    Ok(())
}

#[tokio::test]
async fn closed_input_and_resize_channels_still_process_server_detach(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut scenario = AttachScenario::new(RecordingActions::default());
    let actions = scenario.actions.clone();
    let mut server = scenario.take_server();

    write_server_message(&mut server, AttachMessage::DetachKill).await?;
    scenario.join().await?;

    assert_eq!(actions.calls(), vec!["detach-kill"]);
    Ok(())
}

#[tokio::test]
async fn data_frames_are_hidden_while_lock_command_runs() -> Result<(), Box<dyn std::error::Error>>
{
    let actions = RecordingActions {
        lock_blocks_for: Duration::from_millis(80),
        ..RecordingActions::default()
    };
    let mut scenario = AttachScenario::new(actions);
    let mut server = scenario.take_server();

    write_server_message(&mut server, AttachMessage::Lock("pause".to_owned())).await?;
    write_server_message(&mut server, AttachMessage::Data(b"hidden".to_vec())).await?;

    assert_eq!(
        read_client_message(&mut server).await?,
        AttachMessage::Unlock
    );

    write_server_message(&mut server, AttachMessage::DetachKill).await?;
    let output = scenario.join().await?;

    assert!(
        output.is_empty(),
        "locked attach output should be suppressed, got {output:?}"
    );
    Ok(())
}

#[tokio::test]
async fn keystrokes_received_while_locked_are_dropped_after_unlock(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut scenario = AttachScenario::new(RecordingActions {
        lock_blocks_for: Duration::from_millis(80),
        ..RecordingActions::default()
    });
    let mut server = scenario.take_server();
    let input_tx = scenario.input_tx();

    write_server_message(&mut server, AttachMessage::Lock("pause".to_owned())).await?;
    scenario
        .actions
        .wait_for_call("lock:pause", Duration::from_secs(1))
        .await?;
    input_tx
        .send(super::super::input::AttachInput::bytes(b"secret".to_vec()))
        .await
        .expect("send locked input");

    assert_eq!(
        read_client_message(&mut server).await?,
        AttachMessage::Unlock
    );
    input_tx
        .send(super::super::input::AttachInput::bytes(b"visible".to_vec()))
        .await
        .expect("send unlocked input");

    assert_eq!(
        read_client_message(&mut server).await?,
        AttachMessage::Keystroke(AttachedKeystroke::new(b"visible".to_vec()))
    );

    write_server_message(&mut server, AttachMessage::DetachKill).await?;
    scenario.join().await?;
    Ok(())
}

#[tokio::test]
async fn windows_console_key_metadata_is_sent_with_single_chunk_input(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut scenario = AttachScenario::new(RecordingActions::default());
    let mut server = scenario.take_server();
    let input_tx = scenario.input_tx();
    let key = AttachedWindowsConsoleKey::new(0x44, 0x20, 0x04, 0x0008, 1);

    input_tx
        .send(super::super::input::AttachInput::with_windows_console_key(
            vec![0x04],
            key,
        ))
        .await
        .expect("send Ctrl-D input");

    assert_eq!(
        read_client_message(&mut server).await?,
        AttachMessage::Keystroke(AttachedKeystroke::new(vec![0x04]).with_windows_console_key(key))
    );

    write_server_message(&mut server, AttachMessage::DetachKill).await?;
    scenario.join().await?;
    Ok(())
}

#[tokio::test]
async fn repeated_windows_console_keys_are_sent_as_separate_structured_frames(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut scenario = AttachScenario::new(RecordingActions::default());
    let mut server = scenario.take_server();
    let input_tx = scenario.input_tx();
    let repeated_key = AttachedWindowsConsoleKey::new(0xba, 0x27, b';' as u16, 0x0008, 3);
    let logical_key = AttachedWindowsConsoleKey::new(0xba, 0x27, b';' as u16, 0x0008, 1);

    input_tx
        .send(super::super::input::AttachInput::with_windows_console_key(
            b";".to_vec(),
            repeated_key,
        ))
        .await
        .expect("send repeated Ctrl+; input");

    assert_eq!(
        read_client_messages(&mut server, 3).await?,
        vec![
            AttachMessage::Keystroke(
                AttachedKeystroke::new(b";".to_vec()).with_windows_console_key(logical_key)
            );
            3
        ]
    );

    write_server_message(&mut server, AttachMessage::DetachKill).await?;
    scenario.join().await?;
    Ok(())
}

#[test]
fn repeated_input_counter_preserves_maximum_u16_without_expansion() {
    let key = AttachedWindowsConsoleKey::new(0xba, 0x27, b';' as u16, 0x0008, u16::MAX);
    let input = super::super::input::AttachInput::with_windows_console_key(b";".to_vec(), key);
    let mut pending = PendingRepeatedAttachInput::new(input);

    assert_eq!(pending.remaining, u16::MAX);
    assert_eq!(pending.input.payload(), b";");
    for _ in 1..u16::MAX {
        assert!(!pending.consume_one());
    }
    assert!(pending.consume_one());
}

#[tokio::test]
async fn maximum_console_key_repeat_does_not_starve_server_detach(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut scenario = AttachScenario::new(RecordingActions::default());
    let actions = scenario.actions.clone();
    let server = scenario.take_server();
    let input_tx = scenario.input_tx();
    let repeated_key = AttachedWindowsConsoleKey::new(0xba, 0x27, b';' as u16, 0x0008, u16::MAX);
    let (mut server_reader, mut server_writer) = tokio::io::split(server);
    let (first_repeat_tx, first_repeat_rx) = tokio::sync::oneshot::channel();

    let drain = tokio::spawn(async move {
        let mut decoder = AttachFrameDecoder::new();
        let mut buffer = [0_u8; 4096];
        let mut emitted_repeats = 0_u32;
        let mut first_repeat_tx = Some(first_repeat_tx);

        loop {
            let bytes_read = server_reader
                .read(&mut buffer)
                .await
                .expect("read repeated attach frames");
            if bytes_read == 0 {
                break;
            }
            decoder.push_bytes(&buffer[..bytes_read]);
            while let Some(message) = decoder
                .next_message()
                .expect("decode repeated attach frame")
            {
                if matches!(message, AttachMessage::Keystroke(_)) {
                    emitted_repeats += 1;
                    if let Some(first_repeat_tx) = first_repeat_tx.take() {
                        let _ = first_repeat_tx.send(());
                    }
                }
            }
        }

        emitted_repeats
    });

    input_tx
        .send(super::super::input::AttachInput::with_windows_console_key(
            b";".to_vec(),
            repeated_key,
        ))
        .await
        .expect("send maximum repeated Ctrl+; input");
    timeout(first_repeat_rx)
        .await?
        .map_err(|_| "attach stream ended before the first repeated key")?;

    let detach = encode_attach_message(&AttachMessage::DetachKill)?;
    timeout(server_writer.write_all(&detach)).await??;
    actions
        .wait_for_call("detach-kill", Duration::from_secs(1))
        .await?;

    scenario.join().await?;
    let emitted_repeats = timeout(drain).await??;
    assert!(emitted_repeats > 0, "repeat stream should have started");
    assert!(
        emitted_repeats < u32::from(u16::MAX),
        "server detach must interrupt the repeat stream before all repeats are emitted"
    );
    Ok(())
}

#[tokio::test]
async fn repeated_windows_console_keys_preserve_count_without_capability(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut scenario = AttachScenario::with_windows_console_key(RecordingActions::default(), false);
    let mut server = scenario.take_server();
    let input_tx = scenario.input_tx();
    let repeated_key = AttachedWindowsConsoleKey::new(0xba, 0x27, b';' as u16, 0x0008, 3);

    input_tx
        .send(super::super::input::AttachInput::with_windows_console_key(
            b";".to_vec(),
            repeated_key,
        ))
        .await
        .expect("send repeated Ctrl+; input");

    assert_eq!(
        read_client_messages(&mut server, 3).await?,
        vec![AttachMessage::Keystroke(AttachedKeystroke::new(b";".to_vec())); 3]
    );

    write_server_message(&mut server, AttachMessage::DetachKill).await?;
    scenario.join().await?;
    Ok(())
}

#[tokio::test]
async fn windows_console_key_metadata_is_not_sent_when_capability_is_disabled(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut scenario = AttachScenario::with_windows_console_key(RecordingActions::default(), false);
    let mut server = scenario.take_server();
    let input_tx = scenario.input_tx();
    let key = AttachedWindowsConsoleKey::new(0x44, 0x20, 0x04, 0x0008, 1);

    input_tx
        .send(super::super::input::AttachInput::with_windows_console_key(
            vec![0x04],
            key,
        ))
        .await
        .expect("send Ctrl-D input");

    assert_eq!(
        read_client_message(&mut server).await?,
        AttachMessage::Keystroke(AttachedKeystroke::new(vec![0x04]))
    );

    write_server_message(&mut server, AttachMessage::DetachKill).await?;
    scenario.join().await?;
    Ok(())
}

#[tokio::test]
async fn input_eof_keeps_attach_stream_until_server_detach(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut scenario = AttachScenario::new(RecordingActions::default());
    let mut server = scenario.take_server();
    scenario.close_input();

    write_server_message(&mut server, AttachMessage::Data(b"still-attached".to_vec())).await?;
    write_server_message(&mut server, AttachMessage::DetachKill).await?;

    let output = scenario.join().await?;
    assert_eq!(output, b"still-attached");
    Ok(())
}

#[tokio::test]
async fn render_frames_are_flushed_in_stream_order_before_strict_data(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut scenario = AttachScenario::new(RecordingActions::default());
    let mut server = scenario.take_server();

    write_server_message(&mut server, AttachMessage::Render(b"old".to_vec())).await?;
    write_server_message(&mut server, AttachMessage::Render(b"new".to_vec())).await?;
    write_server_message(&mut server, AttachMessage::Data(b"strict".to_vec())).await?;
    write_server_message(&mut server, AttachMessage::DetachKill).await?;

    let output = scenario.join().await?;
    assert_eq!(output, b"oldnewstrict");
    Ok(())
}

#[tokio::test]
async fn render_frames_flush_while_stream_stays_busy() -> Result<(), Box<dyn std::error::Error>> {
    let mut scenario = AttachScenario::new(RecordingActions::default());
    let output = scenario.output.clone();
    let mut server = scenario.take_server();

    for index in 0..8 {
        write_server_message(
            &mut server,
            AttachMessage::Render(format!("render-{index}\n").into_bytes()),
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while output.bytes().is_empty() {
        if tokio::time::Instant::now() >= deadline {
            return Err("render frame stayed pending while stream remained busy".into());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    write_server_message(&mut server, AttachMessage::DetachKill).await?;
    let final_output = scenario.join().await?;
    let final_output = String::from_utf8_lossy(&final_output);
    assert!(
        final_output.contains("render-"),
        "expected at least one flushed render frame, got {final_output:?}"
    );
    Ok(())
}

#[tokio::test]
async fn blocked_console_output_does_not_block_input_forwarding(
) -> Result<(), Box<dyn std::error::Error>> {
    let (client_stream, mut server) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(client_stream);
    let (input_tx, input_rx) = mpsc::channel(8);
    let (resize_tx, resize_rx) = mpsc::unbounded_channel();
    let locked = Arc::new(AttachLockState::default());
    let client_locked = Arc::clone(&locked);
    let actions = RecordingActions::default();
    let client_actions = actions.clone();
    let (action_tx, action_rx) = std::sync::mpsc::channel();
    let (completion_tx, completion_rx) = mpsc::unbounded_channel();
    let action_worker: std::thread::JoinHandle<std::result::Result<(), crate::ClientError>> =
        std::thread::spawn(move || {
            let mut actions = client_actions;
            while let Ok(action) = action_rx.recv() {
                if completion_tx
                    .send(run_attach_action(&mut actions, action))
                    .is_err()
                {
                    return Ok(());
                }
            }
            Ok(())
        });
    let (output, write_started_rx, release_tx) = BlockingOutput::new();
    let client = tokio::spawn(async move {
        drive_async_attach(
            reader,
            writer,
            Vec::new(),
            output,
            AttachScreenTracker::default(),
            AttachAsyncChannels::new(
                input_rx,
                resize_rx,
                action_tx,
                completion_rx,
                client_locked,
                true,
            ),
        )
        .await
    });

    write_server_message(&mut server, AttachMessage::Data(b"blocked".to_vec())).await?;
    wait_for_blocking_output_start(write_started_rx).await?;
    input_tx
        .send(super::super::input::AttachInput::bytes(
            b"still-live".to_vec(),
        ))
        .await
        .expect("send input while output is blocked");

    assert_eq!(
        read_client_message(&mut server).await?,
        AttachMessage::Keystroke(AttachedKeystroke::new(b"still-live".to_vec()))
    );

    resize_tx
        .send(TerminalSize::new(120, 40))
        .expect("send resize while output is blocked");
    assert_eq!(
        read_client_message(&mut server).await?,
        AttachMessage::Resize(TerminalSize::new(120, 40))
    );

    write_server_message(&mut server, AttachMessage::DetachKill).await?;
    wait_for_attach_lock(&locked).await?;
    input_tx
        .send(super::super::input::AttachInput::bytes(
            b"locked-before-fence".to_vec(),
        ))
        .await
        .expect("send input after exclusive action request");
    assert!(
        tokio::time::timeout(Duration::from_millis(25), read_client_message(&mut server))
            .await
            .is_err(),
        "exclusive action must lock input before waiting for output"
    );
    assert!(
        actions.calls().is_empty(),
        "detach-kill must wait for earlier console output"
    );

    release_tx.send(()).expect("release blocked output");
    actions
        .wait_for_call("detach-kill", Duration::from_secs(1))
        .await?;
    timeout(client).await???;
    action_worker
        .join()
        .map_err(|_| io::Error::other("action worker panicked"))??;
    assert_eq!(actions.calls(), vec!["detach-kill"]);
    Ok(())
}

#[tokio::test]
async fn output_backpressure_keeps_local_input_and_resize_live(
) -> Result<(), Box<dyn std::error::Error>> {
    let (client_stream, mut server) = tokio::io::duplex(8192);
    let (reader, writer) = tokio::io::split(client_stream);
    let (input_tx, input_rx) = mpsc::channel(8);
    let (resize_tx, resize_rx) = mpsc::unbounded_channel();
    let locked = Arc::new(AttachLockState::default());
    let client_locked = Arc::clone(&locked);
    let actions = RecordingActions::default();
    let client_actions = actions.clone();
    let (action_tx, action_rx) = std::sync::mpsc::channel();
    let (completion_tx, completion_rx) = mpsc::unbounded_channel();
    let action_worker: std::thread::JoinHandle<std::result::Result<(), crate::ClientError>> =
        std::thread::spawn(move || {
            let mut actions = client_actions;
            while let Ok(action) = action_rx.recv() {
                if completion_tx
                    .send(run_attach_action(&mut actions, action))
                    .is_err()
                {
                    return Ok(());
                }
            }
            Ok(())
        });
    let (output, write_started_rx, release_tx) = BlockingOutput::new();
    let captured = output.bytes.clone();
    let client = tokio::spawn(async move {
        drive_async_attach(
            reader,
            writer,
            Vec::new(),
            output,
            AttachScreenTracker::default(),
            AttachAsyncChannels::new(
                input_rx,
                resize_rx,
                action_tx,
                completion_rx,
                client_locked,
                true,
            ),
        )
        .await
    });

    write_server_message(&mut server, AttachMessage::Data(b"blocked".to_vec())).await?;
    wait_for_blocking_output_start(write_started_rx).await?;
    for index in 0..(ATTACH_OUTPUT_QUEUE_CAPACITY + 8) {
        write_server_message(
            &mut server,
            AttachMessage::Data(format!("queued-{index}\n").into_bytes()),
        )
        .await?;
    }

    input_tx
        .send(super::super::input::AttachInput::bytes(
            b"backpressure-input".to_vec(),
        ))
        .await
        .expect("send input while output queue is backpressured");
    assert_eq!(
        read_client_message(&mut server).await?,
        AttachMessage::Keystroke(AttachedKeystroke::new(b"backpressure-input".to_vec()))
    );

    resize_tx
        .send(TerminalSize::new(132, 43))
        .expect("send resize while output queue is backpressured");
    assert_eq!(
        read_client_message(&mut server).await?,
        AttachMessage::Resize(TerminalSize::new(132, 43))
    );

    write_server_message(&mut server, AttachMessage::DetachKill).await?;
    wait_for_attach_lock(&locked).await?;
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        actions.calls().is_empty(),
        "detach-kill must not overtake backpressured output"
    );

    release_tx.send(()).expect("release blocked output");
    actions
        .wait_for_call("detach-kill", Duration::from_secs(1))
        .await?;
    timeout(client).await???;
    action_worker
        .join()
        .map_err(|_| io::Error::other("action worker panicked"))??;
    assert!(actions.calls().contains(&"detach-kill".to_owned()));
    let mut expected = b"blocked".to_vec();
    for index in 0..(ATTACH_OUTPUT_QUEUE_CAPACITY + 8) {
        expected.extend_from_slice(format!("queued-{index}\n").as_bytes());
    }
    assert_eq!(
        *captured.lock().expect("output mutex poisoned"),
        expected,
        "detach must drain every strict frame already received"
    );
    Ok(())
}

#[tokio::test]
async fn backpressured_render_frames_replace_stale_pending_render(
) -> Result<(), Box<dyn std::error::Error>> {
    let (client_stream, mut server) = tokio::io::duplex(8192);
    let (reader, writer) = tokio::io::split(client_stream);
    let (_input_tx, input_rx) = mpsc::channel(8);
    let (_resize_tx, resize_rx) = mpsc::unbounded_channel();
    let locked = Arc::new(AttachLockState::default());
    let client_locked = Arc::clone(&locked);
    let actions = RecordingActions::default();
    let client_actions = actions.clone();
    let (action_tx, action_rx) = std::sync::mpsc::channel();
    let (completion_tx, completion_rx) = mpsc::unbounded_channel();
    let action_worker: std::thread::JoinHandle<std::result::Result<(), crate::ClientError>> =
        std::thread::spawn(move || {
            let mut actions = client_actions;
            while let Ok(action) = action_rx.recv() {
                if completion_tx
                    .send(run_attach_action(&mut actions, action))
                    .is_err()
                {
                    return Ok(());
                }
            }
            Ok(())
        });
    let (output, write_started_rx, release_tx) = BlockingOutput::new();
    let captured = output.bytes.clone();
    let client = tokio::spawn(async move {
        drive_async_attach(
            reader,
            writer,
            Vec::new(),
            output,
            AttachScreenTracker::default(),
            AttachAsyncChannels::new(
                input_rx,
                resize_rx,
                action_tx,
                completion_rx,
                client_locked,
                true,
            ),
        )
        .await
    });

    write_server_message(&mut server, AttachMessage::Data(b"blocked".to_vec())).await?;
    wait_for_blocking_output_start(write_started_rx).await?;
    for index in 0..24 {
        write_server_message(
            &mut server,
            AttachMessage::Render(format!("stale-{index}\n").into_bytes()),
        )
        .await?;
    }
    write_server_message(&mut server, AttachMessage::Render(b"latest\n".to_vec())).await?;
    write_server_message(&mut server, AttachMessage::DetachKill).await?;
    wait_for_attach_lock(&locked).await?;
    assert!(
        actions.calls().is_empty(),
        "detach-kill must wait for the latest coalesced render"
    );

    release_tx.send(()).expect("release blocked output");
    wait_for_output_contains(&captured, b"latest\n", Duration::from_secs(1)).await?;
    actions
        .wait_for_call("detach-kill", Duration::from_secs(1))
        .await?;
    let output = captured.lock().expect("output mutex poisoned").clone();
    assert!(
        !String::from_utf8_lossy(&output).contains("stale-"),
        "stale render frames should be replaced under backpressure, got {output:?}"
    );
    timeout(client).await???;
    action_worker
        .join()
        .map_err(|_| io::Error::other("action worker panicked"))??;
    Ok(())
}

#[tokio::test]
async fn pending_render_after_strict_frame_rearms_coalescing_deadline(
) -> Result<(), Box<dyn std::error::Error>> {
    let output = SharedOutput::default();
    let captured = output.bytes.clone();
    let mut queue = AttachOutputQueue::spawn(output);

    queue.push_pending(AttachOutputFrame::render(b"first-render\n".to_vec()))?;
    queue.push_pending(AttachOutputFrame::strict(b"strict-frame\n".to_vec()))?;
    queue.push_pending(AttachOutputFrame::render(b"final-render\n".to_vec()))?;
    queue.flush_pending()?;

    wait_for_output_contains(&captured, b"strict-frame\n", Duration::from_secs(1)).await?;
    wait_for_output_queue_idle(&mut queue, Duration::from_secs(1)).await?;
    tokio::time::sleep(ATTACH_RENDER_MAX_PENDING + Duration::from_millis(5)).await;
    queue.flush_pending()?;

    wait_for_output_contains(&captured, b"final-render\n", Duration::from_secs(1)).await?;
    Ok(())
}

#[test]
fn expired_render_backpressure_uses_retry_delay_instead_of_zero_spin() {
    let mut queue = AttachOutputQueue::spawn(SharedOutput::default());
    queue
        .pending
        .push_back(AttachOutputFrame::render(b"render".to_vec()));
    queue.pending_bytes = b"render".len();
    queue.pending_render_started_at = Some(Instant::now() - ATTACH_RENDER_MAX_PENDING);
    queue.queued_frames = 1;
    queue.painted_frame = true;

    assert_eq!(
        queue.backpressure_retry_delay(),
        Some(ATTACH_OUTPUT_BACKPRESSURE_RETRY)
    );
}

#[tokio::test]
async fn strict_overflow_spools_without_pausing_server_reads_or_losing_order(
) -> Result<(), Box<dyn std::error::Error>> {
    let output = SharedOutput::default();
    let captured = output.bytes.clone();
    let mut queue = AttachOutputQueue::spawn(output);
    let base = vec![b'x'; ATTACH_OUTPUT_PENDING_MAX_BYTES - 1];

    queue.push_pending(AttachOutputFrame::strict(base.clone()))?;
    queue.push_pending(AttachOutputFrame::strict(b"latest-delta".to_vec()))?;

    assert_eq!(
        queue.pending_bytes,
        ATTACH_OUTPUT_PENDING_MAX_BYTES - 1,
        "spooled output must not consume in-memory pending budget"
    );
    assert!(
        !queue.should_pause_server_reads(),
        "overflow must not block control messages behind server output"
    );
    assert_eq!(queue.spool.frames.len(), 1);

    queue.flush_pending()?;
    wait_for_output_contains(&captured, b"latest-delta", Duration::from_secs(1)).await?;
    let output = captured.lock().expect("output mutex poisoned");
    assert!(output.starts_with(&base));
    assert!(output.ends_with(b"latest-delta"));
    Ok(())
}

#[tokio::test]
async fn incoming_render_overflow_spools_after_strict_instead_of_discarding_base(
) -> Result<(), Box<dyn std::error::Error>> {
    let output = SharedOutput::default();
    let captured = output.bytes.clone();
    let mut queue = AttachOutputQueue::spawn(output);
    let base = vec![b'x'; ATTACH_OUTPUT_PENDING_MAX_BYTES - 1];

    queue.push_pending(AttachOutputFrame::strict(base.clone()))?;
    queue.push_pending(AttachOutputFrame::render(b"fresh-render".to_vec()))?;

    assert_eq!(queue.pending.len(), 1);
    let frame = queue.pending.front().expect("strict base remains queued");
    assert_eq!(frame.kind, AttachOutputFrameKind::Strict);
    assert_eq!(queue.spool.frames.len(), 1);
    assert_eq!(
        queue.spool.frames.front().map(|frame| frame.kind),
        Some(AttachOutputFrameKind::Render)
    );

    queue.flush_pending()?;
    wait_for_output_queue_idle(&mut queue, Duration::from_secs(1)).await?;
    tokio::time::sleep(ATTACH_RENDER_MAX_PENDING + Duration::from_millis(5)).await;
    queue.flush_pending()?;
    wait_for_output_contains(&captured, b"fresh-render", Duration::from_secs(1)).await?;
    let output = captured.lock().expect("output mutex poisoned");
    assert!(output.starts_with(&base));
    assert!(output.ends_with(b"fresh-render"));
    Ok(())
}

#[tokio::test]
async fn later_frames_stay_behind_existing_spool_even_when_memory_has_room(
) -> Result<(), Box<dyn std::error::Error>> {
    let output = SharedOutput::default();
    let captured = output.bytes.clone();
    let mut queue = AttachOutputQueue::spawn(output);
    let base = vec![b'x'; ATTACH_OUTPUT_PENDING_MAX_BYTES - 1];

    queue.push_pending(AttachOutputFrame::strict(base.clone()))?;
    queue.push_pending(AttachOutputFrame::render(b"render".to_vec()))?;
    queue.push_pending(AttachOutputFrame::strict(b"!".to_vec()))?;

    assert_eq!(queue.pending.len(), 1);
    assert_eq!(queue.spool.frames.len(), 2);
    assert_eq!(
        queue.spool.frames.front().map(|frame| frame.kind),
        Some(AttachOutputFrameKind::Render)
    );
    assert_eq!(
        queue.spool.frames.back().map(|frame| frame.kind),
        Some(AttachOutputFrameKind::Strict)
    );

    queue.flush_pending()?;
    wait_for_output_contains(&captured, b"render!", Duration::from_secs(1)).await?;
    let output = captured.lock().expect("output mutex poisoned");
    assert!(output.starts_with(&base));
    assert!(output.ends_with(b"render!"));
    Ok(())
}

#[test]
fn spool_cap_rejects_frames_that_exceed_outstanding_backlog_limit() {
    let mut spool = AttachOutputSpool::default();
    spool.outstanding_bytes = ATTACH_OUTPUT_SPOOL_MAX_BYTES;

    let error = spool
        .push(AttachOutputFrame::strict(vec![b'x']))
        .expect_err("backlog beyond the cap must fail before mutation");

    assert!(
        error.to_string().contains("attach output spool exceeded"),
        "unexpected error: {error}"
    );
    assert!(spool.frames.is_empty());
    assert_eq!(spool.outstanding_bytes, ATTACH_OUTPUT_SPOOL_MAX_BYTES);
    assert!(spool.file.is_none());
    assert!(spool.path.is_none());
}

#[test]
fn spool_compacts_when_physical_offset_reaches_cap_but_backlog_fits() {
    let mut spool = AttachOutputSpool::default();

    spool
        .push_with_limit(AttachOutputFrame::strict(b"abcd".to_vec()), 8)
        .expect("first frame fits");
    spool
        .push_with_limit(AttachOutputFrame::strict(b"efgh".to_vec()), 8)
        .expect("second frame reaches cap");
    assert_eq!(
        spool
            .pop()
            .expect("pop succeeds")
            .expect("first frame exists")
            .bytes,
        b"abcd"
    );
    assert_eq!(spool.end_offset, 8);
    assert_eq!(spool.outstanding_bytes, 4);

    spool
        .push_with_limit(AttachOutputFrame::strict(b"ij".to_vec()), 8)
        .expect("small backlog compacts instead of tripping cumulative cap");

    assert_eq!(spool.end_offset, 6);
    assert_eq!(spool.outstanding_bytes, 6);
    assert_eq!(
        spool
            .pop()
            .expect("pop succeeds")
            .expect("second frame exists")
            .bytes,
        b"efgh"
    );
    assert_eq!(
        spool
            .pop()
            .expect("pop succeeds")
            .expect("third frame exists")
            .bytes,
        b"ij"
    );
    assert!(spool.pop().expect("empty pop succeeds").is_none());
}

#[test]
fn spool_defers_compaction_for_small_reclaimable_prefix_near_cap() {
    let mut spool = AttachOutputSpool::default();

    spool
        .push_with_limit(AttachOutputFrame::strict(b"a".to_vec()), 16)
        .expect("first frame fits");
    spool
        .push_with_limit(AttachOutputFrame::strict(vec![b'b'; 15]), 16)
        .expect("second frame reaches logical cap");
    assert_eq!(
        spool
            .pop()
            .expect("pop succeeds")
            .expect("first frame exists")
            .bytes,
        b"a"
    );

    spool
        .push_with_limit(AttachOutputFrame::strict(b"c".to_vec()), 16)
        .expect("one-byte refill fits in physical slack without compaction");

    assert_eq!(spool.end_offset, 17);
    assert_eq!(spool.outstanding_bytes, 16);
    assert_eq!(spool.frames.front().map(|frame| frame.offset), Some(1));
    assert_eq!(
        spool
            .pop()
            .expect("pop succeeds")
            .expect("second frame exists")
            .bytes,
        vec![b'b'; 15]
    );
    assert_eq!(
        spool
            .pop()
            .expect("pop succeeds")
            .expect("third frame exists")
            .bytes,
        b"c"
    );
    assert!(spool.pop().expect("empty pop succeeds").is_none());
}

#[test]
fn pending_render_replacement_survives_spool_cap_failure() {
    let mut queue = AttachOutputQueue::spawn(SharedOutput::default());
    queue
        .pending
        .push_back(AttachOutputFrame::render(b"old-render".to_vec()));
    queue.pending_bytes = ATTACH_OUTPUT_PENDING_MAX_BYTES + b"old-render".len() - 1;
    queue.spool.outstanding_bytes = ATTACH_OUTPUT_SPOOL_MAX_BYTES;

    let error = queue
        .push_pending(AttachOutputFrame::render(vec![b'x'; 2]))
        .expect_err("spool cap failure must leave pending render intact");

    assert!(
        error.to_string().contains("attach output spool exceeded"),
        "unexpected error: {error}"
    );
    assert_eq!(queue.pending.len(), 1);
    let frame = queue.pending.back().expect("old render remains queued");
    assert_eq!(frame.kind, AttachOutputFrameKind::Render);
    assert_eq!(frame.bytes, b"old-render");
    assert_eq!(
        queue.pending_bytes,
        ATTACH_OUTPUT_PENDING_MAX_BYTES + b"old-render".len() - 1
    );
    assert!(queue.spool.frames.is_empty());
    assert!(queue.spool.file.is_none());
}

#[test]
fn orphan_spool_cleanup_removes_dead_pid_files_and_parses_current_shape() {
    let file_name = attach_output_spool_file_name(1234, 5678);
    assert_eq!(attach_output_spool_pid(&file_name), Some(1234));
    assert_eq!(
        attach_output_spool_pid("rmux-attach-output-1234-x.spool"),
        None
    );

    let orphan = std::env::temp_dir().join(attach_output_spool_file_name(0, 42));
    std::fs::write(&orphan, b"stale spool").expect("write orphan spool fixture");
    cleanup_orphaned_attach_output_spools_now();
    assert!(
        !orphan.exists(),
        "orphaned attach output spool should be removed"
    );
}

#[test]
fn render_stream_strict_overflow_spools_and_keeps_server_reads_enabled(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut queue = AttachOutputQueue::spawn(SharedOutput::default());
    queue.push_pending(AttachOutputFrame::strict(vec![
        b'x';
        ATTACH_OUTPUT_PENDING_MAX_BYTES
            - 1
    ]))?;

    queue.push_pending(AttachOutputFrame::strict(b"latest-delta".to_vec()))?;

    assert_eq!(queue.pending.len(), 1);
    assert!(
        queue
            .pending
            .iter()
            .all(|frame| frame.kind == AttachOutputFrameKind::Strict),
        "Strict output is delivery-guaranteed and must not be shed"
    );
    assert_eq!(
        queue.pending_bytes,
        ATTACH_OUTPUT_PENDING_MAX_BYTES - 1,
        "overflow Strict should leave the memory budget bounded"
    );
    assert_eq!(queue.spool.frames.len(), 1);
    assert!(!queue.should_pause_server_reads());
    Ok(())
}

#[tokio::test]
async fn output_writer_failure_wakes_attach_loop_while_server_is_idle(
) -> Result<(), Box<dyn std::error::Error>> {
    let (client_stream, mut server) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(client_stream);
    let (_input_tx, input_rx) = mpsc::channel(8);
    let (_resize_tx, resize_rx) = mpsc::unbounded_channel();
    let locked = Arc::new(AttachLockState::default());
    let (action_tx, _action_rx) = std::sync::mpsc::channel();
    let (_completion_tx, completion_rx) = mpsc::unbounded_channel();
    let client = tokio::spawn(async move {
        drive_async_attach(
            reader,
            writer,
            Vec::new(),
            FailingOutput,
            AttachScreenTracker::default(),
            AttachAsyncChannels::new(input_rx, resize_rx, action_tx, completion_rx, locked, true),
        )
        .await
    });

    write_server_message(&mut server, AttachMessage::Data(b"broken".to_vec())).await?;
    let error = timeout(client)
        .await??
        .expect_err("output write failure must fail attach");
    match error {
        ClientError::Io(error) => assert_eq!(error.kind(), io::ErrorKind::BrokenPipe),
        other => panic!("expected BrokenPipe output error, got {other:?}"),
    }

    Ok(())
}

#[tokio::test]
async fn output_fence_timeout_cancels_and_joins_blocked_writer(
) -> Result<(), Box<dyn std::error::Error>> {
    let (client_stream, mut server) = tokio::io::duplex(2 * 1024 * 1024);
    let (reader, writer) = tokio::io::split(client_stream);
    let (_input_tx, input_rx) = mpsc::channel(8);
    let (_resize_tx, resize_rx) = mpsc::unbounded_channel();
    let locked = Arc::new(AttachLockState::default());
    let (action_tx, action_rx) = std::sync::mpsc::channel();
    let (_completion_tx, completion_rx) = mpsc::unbounded_channel();
    let (output, blocked_output_reader, write_started_rx) = SignaledPipeOutput::new()?;
    let client = tokio::spawn(async move {
        drive_async_attach(
            reader,
            writer,
            Vec::new(),
            output,
            AttachScreenTracker::default(),
            AttachAsyncChannels::new(input_rx, resize_rx, action_tx, completion_rx, locked, true)
                .with_output_fence_timeout(Duration::from_millis(50)),
        )
        .await
    });

    write_server_message(&mut server, AttachMessage::Data(vec![b'x'; 1024 * 1024])).await?;
    wait_for_blocking_output_start(write_started_rx).await?;
    write_server_message(&mut server, AttachMessage::Lock("must-not-run".to_owned())).await?;

    let error = timeout(client)
        .await??
        .expect_err("a blocked output fence must time out");
    assert!(
        matches!(
            error,
            ClientError::Io(ref error)
                if error.kind() == io::ErrorKind::TimedOut
                    && error.to_string().contains("exclusive terminal action")
        ),
        "unexpected output fence timeout: {error}"
    );
    assert!(
        action_rx.try_recv().is_err(),
        "the terminal action must not run without an acknowledged fence"
    );

    let drained = tokio::task::spawn_blocking(move || {
        let mut reader = blocked_output_reader;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        Ok::<_, io::Error>(bytes)
    });
    let _bytes = timeout(drained).await???;
    // `read_to_end` can only return after the output worker has dropped the
    // pipe's sole write handle. The write-start signal above establishes that
    // cancellation interrupted an entered WriteFile call rather than a worker
    // that had not started yet.
    Ok(())
}

#[test]
fn cancellation_remains_active_while_output_drop_blocks() -> Result<(), Box<dyn std::error::Error>>
{
    let (output, mut blocked_output_reader, drop_started_rx) = DropBlockingPipeOutput::new()?;
    let mut worker = AttachOutputWorker::spawn(output);
    let (joined_tx, joined_rx) = std::sync::mpsc::channel();
    let cancel_thread = std::thread::spawn(move || {
        let _ = joined_tx.send(worker.cancel_and_join());
    });

    drop_started_rx
        .recv_timeout(Duration::from_secs(1))
        .map_err(|_| io::Error::other("output Drop did not start"))?;
    let joined = match joined_rx.recv_timeout(Duration::from_secs(2)) {
        Ok(joined) => joined,
        Err(_) => {
            drop(blocked_output_reader);
            cancel_thread
                .join()
                .map_err(|_| io::Error::other("output cancellation thread panicked"))?;
            return Err(io::Error::other("output cancellation did not join a blocked Drop").into());
        }
    };
    joined?;
    let mut bytes = Vec::new();
    blocked_output_reader.read_to_end(&mut bytes)?;
    cancel_thread
        .join()
        .map_err(|_| io::Error::other("output cancellation thread panicked"))?;
    Ok(())
}

#[tokio::test]
async fn input_worker_error_wakes_idle_attach_and_runs_cleanup_once(
) -> Result<(), Box<dyn std::error::Error>> {
    let (client_stream, _server) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(client_stream);
    let (_input_tx, input_rx) = mpsc::channel(8);
    let (_resize_tx, resize_rx) = mpsc::unbounded_channel();
    let locked = Arc::new(AttachLockState::default());
    let (action_tx, _action_rx) = std::sync::mpsc::channel();
    let (_action_completion_tx, action_completion_rx) = mpsc::unbounded_channel();
    let written = Arc::new(Mutex::new(Vec::new()));
    let cleanup = fallback_attach_stop_sequence("xterm-256color");
    let client_output = SharedOutput {
        bytes: Arc::clone(&written),
    };
    let (input_completion_tx, input_completion_rx) = oneshot::channel();
    let client = tokio::spawn(async move {
        drive_async_attach(
            reader,
            writer,
            Vec::new(),
            client_output,
            AttachScreenTracker::default(),
            AttachAsyncChannels::new(
                input_rx,
                resize_rx,
                action_tx,
                action_completion_rx,
                locked,
                true,
            )
            .with_input_completion(input_completion_rx)
            .with_error_cleanup(Some(cleanup)),
        )
        .await
    });

    input_completion_tx
        .send(Err(ClientError::Io(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "injected attach input failure",
        ))))
        .map_err(|_| "input completion receiver dropped before failure")?;
    let error = timeout(client)
        .await??
        .expect_err("input failure must stop an otherwise idle attach");
    assert!(
        matches!(
            error,
            ClientError::Io(ref error)
                if error.kind() == io::ErrorKind::BrokenPipe
                    && error.to_string() == "injected attach input failure"
        ),
        "unexpected attach input error: {error}"
    );
    assert_eq!(
        *written.lock().expect("output mutex poisoned"),
        fallback_attach_stop_sequence("xterm-256color"),
        "input failure must enqueue exactly one terminal cleanup sequence"
    );
    Ok(())
}

#[tokio::test]
async fn ready_input_error_preempts_already_queued_keystrokes(
) -> Result<(), Box<dyn std::error::Error>> {
    let (client_stream, mut server) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(client_stream);
    let (input_tx, input_rx) = mpsc::channel(8);
    input_tx
        .send(super::super::input::AttachInput::bytes(
            b"must-not-be-forwarded".to_vec(),
        ))
        .await?;
    let (input_completion_tx, input_completion_rx) = oneshot::channel();
    input_completion_tx
        .send(Err(ClientError::Io(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "input failed after queuing bytes",
        ))))
        .map_err(|_| "input completion receiver dropped before attach start")?;
    let (_resize_tx, resize_rx) = mpsc::unbounded_channel();
    let locked = Arc::new(AttachLockState::default());
    let (action_tx, _action_rx) = std::sync::mpsc::channel();
    let (_action_completion_tx, action_completion_rx) = mpsc::unbounded_channel();

    let error = drive_async_attach(
        reader,
        writer,
        Vec::new(),
        SharedOutput::default(),
        AttachScreenTracker::default(),
        AttachAsyncChannels::new(
            input_rx,
            resize_rx,
            action_tx,
            action_completion_rx,
            locked,
            true,
        )
        .with_input_completion(input_completion_rx),
    )
    .await
    .expect_err("a published input error must abort queued keystrokes");
    assert!(
        matches!(error, ClientError::Io(ref error) if error.kind() == io::ErrorKind::BrokenPipe),
        "unexpected queued input error: {error}"
    );

    let mut forwarded = [0_u8; 64];
    assert_eq!(
        server.read(&mut forwarded).await?,
        0,
        "once the worker publishes an error, queued input must be abandoned rather than forwarded"
    );
    Ok(())
}

#[tokio::test]
async fn input_worker_exit_without_result_fails_instead_of_becoming_read_only(
) -> Result<(), Box<dyn std::error::Error>> {
    let (client_stream, _server) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(client_stream);
    let (_input_tx, input_rx) = mpsc::channel(8);
    let (input_completion_tx, input_completion_rx) = oneshot::channel();
    let (_resize_tx, resize_rx) = mpsc::unbounded_channel();
    let locked = Arc::new(AttachLockState::default());
    let (action_tx, _action_rx) = std::sync::mpsc::channel();
    let (_action_completion_tx, action_completion_rx) = mpsc::unbounded_channel();
    drop(input_completion_tx);

    let error = drive_async_attach(
        reader,
        writer,
        Vec::new(),
        SharedOutput::default(),
        AttachScreenTracker::default(),
        AttachAsyncChannels::new(
            input_rx,
            resize_rx,
            action_tx,
            action_completion_rx,
            locked,
            true,
        )
        .with_input_completion(input_completion_rx),
    )
    .await
    .expect_err("a panicked input worker must stop attach");
    assert!(
        matches!(
            error,
            ClientError::Io(ref error)
                if error.kind() == io::ErrorKind::Other
                    && error.to_string()
                        == "attach input worker stopped before reporting completion"
        ),
        "unexpected missing input completion error: {error}"
    );
    Ok(())
}

#[tokio::test]
async fn normal_input_completion_drains_final_bytes_and_keeps_output_open(
) -> Result<(), Box<dyn std::error::Error>> {
    let (client_stream, mut server) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(client_stream);
    let (input_tx, input_rx) = mpsc::channel(8);
    input_tx
        .send(super::super::input::AttachInput::bytes(
            b"final-input".to_vec(),
        ))
        .await?;
    drop(input_tx);
    let (input_completion_tx, input_completion_rx) = oneshot::channel();
    input_completion_tx
        .send(Ok(()))
        .map_err(|_| "input completion receiver dropped before attach start")?;
    let (_resize_tx, resize_rx) = mpsc::unbounded_channel();
    let locked = Arc::new(AttachLockState::default());
    let (action_tx, _action_rx) = std::sync::mpsc::channel();
    let (_action_completion_tx, action_completion_rx) = mpsc::unbounded_channel();
    let client = tokio::spawn(async move {
        drive_async_attach(
            reader,
            writer,
            Vec::new(),
            SharedOutput::default(),
            AttachScreenTracker::default(),
            AttachAsyncChannels::new(
                input_rx,
                resize_rx,
                action_tx,
                action_completion_rx,
                locked,
                true,
            )
            .with_input_completion(input_completion_rx),
        )
        .await
    });

    assert_eq!(
        read_client_message(&mut server).await?,
        AttachMessage::Keystroke(AttachedKeystroke::new(b"final-input".to_vec())),
        "normal input completion must not discard bytes queued before EOF"
    );
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        !client.is_finished(),
        "normal stdin EOF must preserve the existing output-only attach behavior"
    );

    write_server_message(
        &mut server,
        AttachMessage::Data(ALT_SCREEN_EXIT_FALLBACK.to_vec()),
    )
    .await?;
    drop(server);
    timeout(client).await???;
    Ok(())
}

#[tokio::test]
async fn abrupt_eof_queues_cleanup_after_output_and_drops_the_only_writer(
) -> Result<(), Box<dyn std::error::Error>> {
    let (client_stream, mut server) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(client_stream);
    let (_input_tx, input_rx) = mpsc::channel(8);
    let (_resize_tx, resize_rx) = mpsc::unbounded_channel();
    let locked = Arc::new(AttachLockState::default());
    let (action_tx, _action_rx) = std::sync::mpsc::channel();
    let (_completion_tx, completion_rx) = mpsc::unbounded_channel();
    let dropped = Arc::new(AtomicBool::new(false));
    let written = Arc::new(Mutex::new(Vec::new()));
    let output = DropTrackingOutput {
        dropped: Arc::clone(&dropped),
        written: Arc::clone(&written),
    };

    write_server_message(
        &mut server,
        AttachMessage::Data(b"render-before-eof".to_vec()),
    )
    .await?;
    drop(server);
    let error = drive_async_attach(
        reader,
        writer,
        Vec::new(),
        output,
        AttachScreenTracker::default(),
        AttachAsyncChannels::new(input_rx, resize_rx, action_tx, completion_rx, locked, true)
            .with_error_cleanup(Some(fallback_attach_stop_sequence("xterm-256color"))),
    )
    .await
    .expect_err("abrupt daemon EOF must fail an unstopped attach");

    assert!(
        matches!(
            error,
            ClientError::Io(ref error) if error.kind() == io::ErrorKind::UnexpectedEof
        ),
        "unexpected attach error: {error}"
    );
    assert!(dropped.load(Ordering::SeqCst), "output writer must stop");
    let mut expected = b"render-before-eof".to_vec();
    expected.extend_from_slice(&fallback_attach_stop_sequence("xterm-256color"));
    assert_eq!(
        *written.lock().expect("output mutex poisoned"),
        expected,
        "cleanup must be the final frame drained by the existing writer"
    );
    Ok(())
}

#[tokio::test]
async fn successful_detach_does_not_enqueue_error_cleanup() -> Result<(), Box<dyn std::error::Error>>
{
    let mut scenario = AttachScenario::with_error_cleanup(RecordingActions::default());
    let mut server = scenario.take_server();

    write_server_message(&mut server, AttachMessage::DetachKill).await?;

    assert!(scenario.join().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn observed_attach_stop_does_not_enqueue_error_cleanup(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut scenario = AttachScenario::with_error_cleanup(RecordingActions::default());
    let mut server = scenario.take_server();

    write_server_message(
        &mut server,
        AttachMessage::Data(ALT_SCREEN_EXIT_FALLBACK.to_vec()),
    )
    .await?;
    drop(server);

    assert_eq!(scenario.join().await?, ALT_SCREEN_EXIT_FALLBACK);
    Ok(())
}

#[tokio::test]
async fn mouse_sequences_toggle_windows_console_mouse_actions(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut scenario = AttachScenario::new(RecordingActions::default());
    let actions = scenario.actions.clone();
    let mut server = scenario.take_server();

    write_server_message(
        &mut server,
        AttachMessage::Data(b"\x1b[?1006h\x1b[?1000h\x1b[?1002h".to_vec()),
    )
    .await?;
    actions
        .wait_for_call("mouse:true", Duration::from_secs(1))
        .await?;

    write_server_message(
        &mut server,
        AttachMessage::Data(b"\x1b[?1002l\x1b[?1000l\x1b[?1006l".to_vec()),
    )
    .await?;
    actions
        .wait_for_call("mouse:false", Duration::from_secs(1))
        .await?;

    write_server_message(&mut server, AttachMessage::DetachKill).await?;
    scenario.join().await?;

    assert_eq!(
        actions.calls(),
        vec!["mouse:true", "mouse:false", "detach-kill"]
    );
    Ok(())
}

#[test]
fn mouse_tracker_keeps_console_mouse_enabled_when_only_sgr_is_disabled() {
    let mut tracker = WindowsConsoleMouseTracker::default();

    assert_eq!(tracker.observe(b"\x1b[?1000h\x1b[?1006h"), Some(true));
    assert_eq!(tracker.observe(b"\x1b[?1006l"), None);
    assert_eq!(tracker.observe(b"\x1b[?1000l"), Some(false));
}

#[test]
fn split_sgr_mouse_sequence_does_not_enable_windows_console_mouse() {
    let mut tracker = WindowsConsoleMouseTracker::default();

    assert_eq!(tracker.observe(b"\x1b[?10"), None);
    assert_eq!(tracker.observe(b"06h"), None);
}

#[tokio::test]
async fn split_mouse_sequence_toggles_windows_console_mouse(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut scenario = AttachScenario::new(RecordingActions::default());
    let actions = scenario.actions.clone();
    let mut server = scenario.take_server();

    write_server_message(&mut server, AttachMessage::Data(b"\x1b[?10".to_vec())).await?;
    write_server_message(&mut server, AttachMessage::Data(b"00h".to_vec())).await?;
    actions
        .wait_for_call("mouse:true", Duration::from_secs(1))
        .await?;

    write_server_message(&mut server, AttachMessage::DetachKill).await?;
    scenario.join().await?;

    assert_eq!(actions.calls(), vec!["mouse:true", "detach-kill"]);
    Ok(())
}

#[tokio::test]
async fn mouse_all_sequences_toggle_windows_console_mouse_actions(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut scenario = AttachScenario::new(RecordingActions::default());
    let actions = scenario.actions.clone();
    let mut server = scenario.take_server();

    write_server_message(&mut server, AttachMessage::Data(b"\x1b[?1003h".to_vec())).await?;
    actions
        .wait_for_call("mouse:true", Duration::from_secs(1))
        .await?;

    write_server_message(&mut server, AttachMessage::Data(b"\x1b[?1003l".to_vec())).await?;
    actions
        .wait_for_call("mouse:false", Duration::from_secs(1))
        .await?;

    write_server_message(&mut server, AttachMessage::DetachKill).await?;
    scenario.join().await?;

    assert_eq!(
        actions.calls(),
        vec!["mouse:true", "mouse:false", "detach-kill"]
    );
    Ok(())
}

#[tokio::test]
async fn literal_exit_banners_do_not_end_the_attach_stream(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut scenario = AttachScenario::new(RecordingActions::default());
    let mut server = scenario.take_server();
    let split = 12;

    write_server_message(
        &mut server,
        AttachMessage::Data(DETACHED_BANNER_PREFIX[..split].to_vec()),
    )
    .await?;
    write_server_message(
        &mut server,
        AttachMessage::Data(DETACHED_BANNER_PREFIX[split..].to_vec()),
    )
    .await?;
    write_server_message(&mut server, AttachMessage::Data(EXITED_BANNER.to_vec())).await?;
    write_server_message(&mut server, AttachMessage::Data(b"still-attached".to_vec())).await?;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !scenario.client.is_finished(),
        "pane bytes that resemble RMUX banners must not terminate attach"
    );

    write_server_message(
        &mut server,
        AttachMessage::Data(ALT_SCREEN_EXIT_FALLBACK.to_vec()),
    )
    .await?;
    drop(server);

    let output = scenario.join().await?;
    let mut expected = DETACHED_BANNER_PREFIX.to_vec();
    expected.extend_from_slice(EXITED_BANNER);
    expected.extend_from_slice(b"still-attached");
    expected.extend_from_slice(ALT_SCREEN_EXIT_FALLBACK);
    assert_eq!(output, expected);
    Ok(())
}

#[derive(Debug)]
struct AttachScenario {
    client: tokio::task::JoinHandle<std::result::Result<(), crate::ClientError>>,
    action_worker: std::thread::JoinHandle<std::result::Result<(), crate::ClientError>>,
    actions: RecordingActions,
    output: SharedOutput,
    server: Option<tokio::io::DuplexStream>,
    input_tx: Option<mpsc::Sender<super::super::input::AttachInput>>,
}

impl AttachScenario {
    fn new(actions: RecordingActions) -> Self {
        Self::with_windows_console_key(actions, true)
    }

    fn with_windows_console_key(
        actions: RecordingActions,
        windows_console_key_enabled: bool,
    ) -> Self {
        Self::with_screen_tracker(
            actions,
            windows_console_key_enabled,
            AttachScreenTracker::default(),
        )
    }

    fn with_screen_tracker(
        actions: RecordingActions,
        windows_console_key_enabled: bool,
        screen_tracker: AttachScreenTracker,
    ) -> Self {
        Self::with_options(actions, windows_console_key_enabled, screen_tracker, None)
    }

    fn with_error_cleanup(actions: RecordingActions) -> Self {
        Self::with_options(
            actions,
            true,
            AttachScreenTracker::default(),
            Some(fallback_attach_stop_sequence("xterm-256color")),
        )
    }

    fn with_options(
        actions: RecordingActions,
        windows_console_key_enabled: bool,
        screen_tracker: AttachScreenTracker,
        error_cleanup: Option<Vec<u8>>,
    ) -> Self {
        let (client_stream, server) = tokio::io::duplex(4096);
        let (reader, writer) = tokio::io::split(client_stream);
        let (input_tx, input_rx) = mpsc::channel(8);
        let (_resize_tx, resize_rx) = mpsc::unbounded_channel();
        let locked = Arc::new(AttachLockState::default());
        let client_actions = actions.clone();
        let (action_tx, action_rx) = std::sync::mpsc::channel();
        let (completion_tx, completion_rx) = mpsc::unbounded_channel();
        let action_worker = std::thread::spawn(move || {
            let mut actions = client_actions;
            while let Ok(action) = action_rx.recv() {
                if completion_tx
                    .send(run_attach_action(&mut actions, action))
                    .is_err()
                {
                    return Ok(());
                }
            }
            Ok(())
        });
        let output = SharedOutput::default();
        let client_output = output.clone();
        let client = tokio::spawn(async move {
            drive_async_attach(
                reader,
                writer,
                Vec::new(),
                client_output,
                screen_tracker,
                AttachAsyncChannels::new(
                    input_rx,
                    resize_rx,
                    action_tx,
                    completion_rx,
                    locked,
                    windows_console_key_enabled,
                )
                .with_error_cleanup(error_cleanup),
            )
            .await
        });

        Self {
            client,
            action_worker,
            actions,
            output,
            server: Some(server),
            input_tx: Some(input_tx),
        }
    }

    fn take_server(&mut self) -> tokio::io::DuplexStream {
        self.server.take().expect("server stream should be present")
    }

    fn input_tx(&self) -> mpsc::Sender<super::super::input::AttachInput> {
        self.input_tx
            .as_ref()
            .expect("input sender should be present")
            .clone()
    }

    fn close_input(&mut self) {
        drop(self.input_tx.take());
    }

    async fn join(self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let client_result = timeout(self.client).await?;
        let attach_result = client_result?;
        attach_result?;
        self.action_worker
            .join()
            .map_err(|_| io::Error::other("action worker panicked"))??;
        Ok(self.output.bytes())
    }
}

#[derive(Clone, Debug, Default)]
struct SharedOutput {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl SharedOutput {
    fn bytes(&self) -> Vec<u8> {
        self.bytes.lock().expect("output mutex poisoned").clone()
    }
}

impl Write for SharedOutput {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes
            .lock()
            .map_err(|_| io::Error::other("output mutex poisoned"))?
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct BlockingOutput {
    bytes: Arc<Mutex<Vec<u8>>>,
    write_started_tx: Option<std::sync::mpsc::Sender<()>>,
    release_rx: Option<std::sync::mpsc::Receiver<()>>,
}

impl BlockingOutput {
    fn new() -> (
        Self,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (write_started_tx, write_started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        (
            Self {
                bytes: Arc::new(Mutex::new(Vec::new())),
                write_started_tx: Some(write_started_tx),
                release_rx: Some(release_rx),
            },
            write_started_rx,
            release_tx,
        )
    }
}

impl Write for BlockingOutput {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if let Some(write_started_tx) = self.write_started_tx.take() {
            let _ = write_started_tx.send(());
        }
        if let Some(release_rx) = self.release_rx.take() {
            release_rx
                .recv()
                .map_err(|_| io::Error::other("blocking output release sender dropped"))?;
        }
        self.bytes
            .lock()
            .map_err(|_| io::Error::other("output mutex poisoned"))?
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct SignaledPipeOutput {
    writer: File,
    write_started_tx: Option<std::sync::mpsc::Sender<()>>,
}

impl SignaledPipeOutput {
    fn new() -> io::Result<(Self, File, std::sync::mpsc::Receiver<()>)> {
        let mut read_handle: HANDLE = std::ptr::null_mut();
        let mut write_handle: HANDLE = std::ptr::null_mut();
        let created = unsafe {
            // SAFETY: both handle pointers refer to writable local slots. The
            // default security descriptor is sufficient for this process-local
            // cancellation test, and ownership transfers to File below.
            CreatePipe(
                &mut read_handle,
                &mut write_handle,
                std::ptr::null_mut(),
                4096,
            )
        };
        if created == 0 {
            return Err(io::Error::last_os_error());
        }
        let reader = unsafe {
            // SAFETY: a successful CreatePipe call returned a new owned read
            // handle, transferred exactly once into File.
            File::from_raw_handle(read_handle)
        };
        let writer = unsafe {
            // SAFETY: a successful CreatePipe call returned a new owned write
            // handle, transferred exactly once into File.
            File::from_raw_handle(write_handle)
        };
        let (write_started_tx, write_started_rx) = std::sync::mpsc::channel();
        Ok((
            Self {
                writer,
                write_started_tx: Some(write_started_tx),
            },
            reader,
            write_started_rx,
        ))
    }
}

impl Write for SignaledPipeOutput {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if let Some(write_started_tx) = self.write_started_tx.take() {
            let _ = write_started_tx.send(());
        }
        self.writer.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

#[derive(Debug)]
struct DropBlockingPipeOutput {
    writer: File,
    drop_started_tx: Option<std::sync::mpsc::Sender<()>>,
}

impl DropBlockingPipeOutput {
    fn new() -> io::Result<(Self, File, std::sync::mpsc::Receiver<()>)> {
        let (writer, reader, drop_started_rx) = SignaledPipeOutput::new()?;
        Ok((
            Self {
                writer: writer.writer,
                drop_started_tx: writer.write_started_tx,
            },
            reader,
            drop_started_rx,
        ))
    }
}

impl Write for DropBlockingPipeOutput {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for DropBlockingPipeOutput {
    fn drop(&mut self) {
        if let Some(drop_started_tx) = self.drop_started_tx.take() {
            let _ = drop_started_tx.send(());
        }
        let _ = self.writer.write_all(&vec![b'd'; 1024 * 1024]);
    }
}

#[derive(Debug)]
struct BufferedVtTailOutput {
    captured: Arc<Mutex<Vec<u8>>>,
    pending_tail: Vec<u8>,
    fence_flushed: Arc<AtomicBool>,
}

impl BufferedVtTailOutput {
    fn new() -> (Self, Arc<Mutex<Vec<u8>>>, Arc<AtomicBool>) {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let fence_flushed = Arc::new(AtomicBool::new(false));
        (
            Self {
                captured: Arc::clone(&captured),
                pending_tail: Vec::new(),
                fence_flushed: Arc::clone(&fence_flushed),
            },
            captured,
            fence_flushed,
        )
    }

    fn flush_output_fence(&mut self) -> io::Result<()> {
        self.captured
            .lock()
            .map_err(|_| io::Error::other("captured output mutex poisoned"))?
            .append(&mut self.pending_tail);
        self.fence_flushed.store(true, Ordering::SeqCst);
        Ok(())
    }
}

impl Write for BufferedVtTailOutput {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let split_at = buffer.iter().position(|byte| *byte == b'\x1b');
        let (visible, pending) = split_at
            .map(|split_at| buffer.split_at(split_at))
            .unwrap_or((buffer, &[]));
        self.captured
            .lock()
            .map_err(|_| io::Error::other("captured output mutex poisoned"))?
            .extend_from_slice(visible);
        self.pending_tail.extend_from_slice(pending);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct FailingOutput;

impl Write for FailingOutput {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "injected output failure",
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct DropTrackingOutput {
    dropped: Arc<AtomicBool>,
    written: Arc<Mutex<Vec<u8>>>,
}

impl Write for DropTrackingOutput {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.written
            .lock()
            .map_err(|_| io::Error::other("output mutex poisoned"))?
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for DropTrackingOutput {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

#[derive(Clone, Debug, Default)]
struct RecordingActions {
    calls: Arc<Mutex<Vec<String>>>,
    lock_blocks_for: Duration,
}

impl RecordingActions {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls mutex poisoned").clone()
    }

    fn push(&self, call: impl Into<String>) {
        self.calls
            .lock()
            .expect("calls mutex poisoned")
            .push(call.into());
    }

    async fn wait_for_call(
        &self,
        expected: &str,
        timeout: Duration,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            if self.calls().iter().any(|call| call == expected) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        Err(format!("timed out waiting for attach action call {expected:?}").into())
    }
}

impl AttachActionExecutor for RecordingActions {
    fn handle_lock(
        &mut self,
        command: &AttachShellCommand,
    ) -> std::result::Result<(), crate::ClientError> {
        self.push(format!("lock:{}", command.command()));
        if !self.lock_blocks_for.is_zero() {
            std::thread::sleep(self.lock_blocks_for);
        }
        Ok(())
    }

    fn handle_legacy_lock(&mut self, command: &str) -> std::result::Result<(), crate::ClientError> {
        self.push(format!("lock:{command}"));
        if !self.lock_blocks_for.is_zero() {
            std::thread::sleep(self.lock_blocks_for);
        }
        Ok(())
    }

    fn handle_mouse_input_enabled(
        &mut self,
        enabled: bool,
    ) -> std::result::Result<(), crate::ClientError> {
        self.push(format!("mouse:{enabled}"));
        Ok(())
    }

    fn handle_suspend(&mut self) -> std::result::Result<(), crate::ClientError> {
        self.push("suspend");
        Ok(())
    }

    fn handle_detach_kill(&mut self) -> std::result::Result<(), crate::ClientError> {
        self.push("detach-kill");
        Ok(())
    }

    fn handle_detach_exec(
        &mut self,
        command: &AttachShellCommand,
    ) -> std::result::Result<(), crate::ClientError> {
        self.push(format!("detach-exec:{}", command.command()));
        Ok(())
    }

    fn handle_legacy_detach_exec(
        &mut self,
        command: &str,
    ) -> std::result::Result<(), crate::ClientError> {
        self.push(format!("detach-exec:{command}"));
        Ok(())
    }
}

async fn write_server_message(
    stream: &mut tokio::io::DuplexStream,
    message: AttachMessage,
) -> Result<(), Box<dyn std::error::Error>> {
    let frame = encode_attach_message(&message)?;
    timeout(stream.write_all(&frame)).await??;
    Ok(())
}

async fn receive_attach_action(
    receiver: std::sync::mpsc::Receiver<AttachAction>,
) -> Result<(std::sync::mpsc::Receiver<AttachAction>, AttachAction), Box<dyn std::error::Error>> {
    let (receiver, action) = tokio::task::spawn_blocking(move || {
        let action = receiver.recv_timeout(Duration::from_secs(1));
        (receiver, action)
    })
    .await?;
    Ok((receiver, action?))
}

async fn wait_for_stop_generation(
    tracker: &AttachScreenTracker,
    previous: Option<AttachStopGeneration>,
) -> Result<AttachStopGeneration, Box<dyn std::error::Error>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        if let Some(generation) = tracker.current_stop_generation() {
            if Some(generation) != previous {
                return Ok(generation);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("timed out waiting for attach stop generation".into());
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

async fn read_client_message(
    stream: &mut tokio::io::DuplexStream,
) -> Result<AttachMessage, Box<dyn std::error::Error>> {
    let mut decoder = AttachFrameDecoder::new();
    let mut buffer = [0_u8; 128];
    loop {
        let bytes_read = timeout(stream.read(&mut buffer)).await??;
        if bytes_read == 0 {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "client stream closed before response",
            )));
        }
        decoder.push_bytes(&buffer[..bytes_read]);
        if let Some(message) = decoder.next_message()? {
            return Ok(message);
        }
    }
}

async fn read_client_messages(
    stream: &mut tokio::io::DuplexStream,
    expected_count: usize,
) -> Result<Vec<AttachMessage>, Box<dyn std::error::Error>> {
    let mut decoder = AttachFrameDecoder::new();
    let mut messages = Vec::with_capacity(expected_count);
    let mut buffer = [0_u8; 128];
    while messages.len() < expected_count {
        let bytes_read = timeout(stream.read(&mut buffer)).await??;
        if bytes_read == 0 {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "client stream closed before all responses",
            )));
        }
        decoder.push_bytes(&buffer[..bytes_read]);
        while messages.len() < expected_count {
            let Some(message) = decoder.next_message()? else {
                break;
            };
            messages.push(message);
        }
    }
    Ok(messages)
}

async fn wait_for_blocking_output_start(
    rx: std::sync::mpsc::Receiver<()>,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        match rx.try_recv() {
            Ok(()) => return Ok(()),
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err("blocking output ended before first write".into());
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("timed out waiting for blocking output write".into());
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

async fn wait_for_attach_lock(locked: &AttachLockState) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while !locked.is_locked() {
        if tokio::time::Instant::now() >= deadline {
            return Err("timed out waiting for exclusive attach input lock".into());
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    Ok(())
}

async fn wait_for_output_contains(
    output: &Arc<Mutex<Vec<u8>>>,
    needle: &[u8],
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        {
            let bytes = output.lock().expect("output mutex poisoned");
            if bytes.windows(needle.len()).any(|window| window == needle) {
                return Ok(());
            }
        }
        if tokio::time::Instant::now() >= deadline {
            let bytes = output.lock().expect("output mutex poisoned").clone();
            return Err(format!("timed out waiting for {needle:?} in output {bytes:?}").into());
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

async fn wait_for_output_queue_idle(
    queue: &mut AttachOutputQueue,
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        queue.drain_completed_writes();
        queue.check_failure()?;
        if queue.queued_frames == 0 {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for attach output queue to drain; {} frame(s) still queued",
                queue.queued_frames
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

async fn timeout<F, T>(future: F) -> Result<T, tokio::time::error::Elapsed>
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(Duration::from_secs(2), future).await
}
