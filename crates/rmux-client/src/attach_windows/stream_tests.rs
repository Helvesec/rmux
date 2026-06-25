use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmux_proto::{
    encode_attach_message, AttachFrameDecoder, AttachMessage, AttachShellCommand,
    AttachedKeystroke, AttachedWindowsConsoleKey,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use super::super::action::{run_attach_action, AttachActionExecutor};
use super::super::lock_state::AttachLockState;
use super::*;

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
async fn split_detached_banner_marks_stream_stopped_before_eof(
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
    drop(server);

    let output = scenario.join().await?;
    assert_eq!(output, DETACHED_BANNER_PREFIX);
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
                AttachScreenTracker::default(),
                AttachAsyncChannels::new(
                    input_rx,
                    resize_rx,
                    action_tx,
                    completion_rx,
                    locked,
                    windows_console_key_enabled,
                ),
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

async fn timeout<F, T>(future: F) -> Result<T, tokio::time::error::Elapsed>
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(Duration::from_secs(2), future).await
}
