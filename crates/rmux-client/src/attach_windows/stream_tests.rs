use std::io::{self, Write};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmux_proto::{encode_attach_message, AttachFrameDecoder, AttachMessage};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use super::super::action::{run_attach_action, AttachActionExecutor};
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

#[derive(Debug)]
struct AttachScenario {
    client: tokio::task::JoinHandle<std::result::Result<(), crate::ClientError>>,
    action_worker: std::thread::JoinHandle<std::result::Result<(), crate::ClientError>>,
    actions: RecordingActions,
    output: SharedOutput,
    server: Option<tokio::io::DuplexStream>,
}

impl AttachScenario {
    fn new(actions: RecordingActions) -> Self {
        let (client_stream, server) = tokio::io::duplex(4096);
        let (reader, writer) = tokio::io::split(client_stream);
        let (_input_tx, input_rx) = mpsc::unbounded_channel();
        let (_resize_tx, resize_rx) = mpsc::unbounded_channel();
        let locked = Arc::new(AtomicBool::new(false));
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
                AttachAsyncChannels::new(input_rx, resize_rx, action_tx, completion_rx, locked),
            )
            .await
        });

        Self {
            client,
            action_worker,
            actions,
            output,
            server: Some(server),
        }
    }

    fn take_server(&mut self) -> tokio::io::DuplexStream {
        self.server.take().expect("server stream should be present")
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
}

impl AttachActionExecutor for RecordingActions {
    fn handle_lock(&mut self, command: &str) -> std::result::Result<(), crate::ClientError> {
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

    fn handle_detach_exec(&mut self, command: &str) -> std::result::Result<(), crate::ClientError> {
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
