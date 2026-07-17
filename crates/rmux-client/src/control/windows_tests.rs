use super::drive_async_control;
use rmux_proto::CONTROL_STDIN_EOF_MARKER;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc as tokio_mpsc;

#[tokio::test]
async fn control_input_eof_shutdowns_writer_and_waits_for_exit() -> std::io::Result<()> {
    let (client, mut server) = tokio::io::duplex(4096);
    let (input_tx, input_rx) = tokio_mpsc::channel::<Vec<u8>>(1);
    input_tx
        .send(b"list-sessions\n".to_vec())
        .await
        .expect("send input");
    drop(input_tx);
    let (output_tx, output_rx) = tokio_mpsc::channel::<Vec<u8>>(4);

    let drive = drive_async_control(client, &[], input_rx, output_tx);
    let server_peer = async move {
        let expected_input = format!("list-sessions\n{CONTROL_STDIN_EOF_MARKER}\n");
        let mut received = Vec::new();
        let mut buffer = [0_u8; 32];
        while received.len() < expected_input.len() {
            let bytes_read = server.read(&mut buffer).await?;
            assert_ne!(bytes_read, 0, "client closed before sending command");
            received.extend_from_slice(&buffer[..bytes_read]);
        }
        assert_eq!(received, expected_input.as_bytes());
        server
            .write_all(b"%begin 1 1 1\n%end 1 1 1\n%exit\n")
            .await?;
        Ok::<(), std::io::Error>(())
    };
    let output = collect_control_output(output_rx);

    let (_, _, output) = tokio::try_join!(drive, server_peer, output)?;
    assert_eq!(output, b"%begin 1 1 1\n%end 1 1 1\n%exit\n");
    Ok(())
}

#[tokio::test]
async fn control_input_eof_drains_exit_after_completed_command() -> std::io::Result<()> {
    let (client, mut server) = tokio::io::duplex(4096);
    let (input_tx, input_rx) = tokio_mpsc::channel::<Vec<u8>>(1);
    input_tx
        .send(b"list-sessions\n".to_vec())
        .await
        .expect("send input");
    drop(input_tx);
    let (output_tx, output_rx) = tokio_mpsc::channel::<Vec<u8>>(4);

    let drive = drive_async_control(client, &[], input_rx, output_tx);
    let server_peer = async move {
        let expected_input = format!("list-sessions\n{CONTROL_STDIN_EOF_MARKER}\n");
        let mut received = Vec::new();
        let mut buffer = [0_u8; 32];
        while received.len() < expected_input.len() {
            let bytes_read = server.read(&mut buffer).await?;
            assert_ne!(bytes_read, 0, "client closed before sending command");
            received.extend_from_slice(&buffer[..bytes_read]);
        }
        assert_eq!(received, expected_input.as_bytes());
        server.write_all(b"%begin 1 1 1\n%end 1 1 1\n").await?;
        tokio::task::yield_now().await;
        server.write_all(b"%exit\n").await?;
        Ok::<(), std::io::Error>(())
    };
    let output = collect_control_output(output_rx);

    let (_, _, output) = tokio::try_join!(drive, server_peer, output)?;
    assert_eq!(output, b"%begin 1 1 1\n%end 1 1 1\n%exit\n");
    Ok(())
}

#[tokio::test]
async fn forged_guard_end_and_exit_preserve_fragmented_follow_on_output() -> std::io::Result<()> {
    let (client, mut server) = tokio::io::duplex(4096);
    let (input_tx, input_rx) = tokio_mpsc::channel::<Vec<u8>>(1);
    let commands = b"show-buffer -b payload\ndisplay-message -p FOLLOW_ON\n";
    input_tx.send(commands.to_vec()).await.expect("send input");
    drop(input_tx);
    let (output_tx, output_rx) = tokio_mpsc::channel::<Vec<u8>>(4);

    let output_fragments = vec![
        b"%beg".to_vec(),
        b"in 1 1 1\n%en".to_vec(),
        b"d 1 1 1\n%ex".to_vec(),
        b"it\n".to_vec(),
        std::iter::repeat_n(b'x', 9000).collect(),
        b"\n%end 1 1 1\n%beg".to_vec(),
        b"in 2 2 1\nFOLLOW_ON\n%end 2 2 1\n%ex".to_vec(),
        b"it\n".to_vec(),
    ];
    let expected_output = output_fragments.concat();

    let drive = drive_async_control(client, &[], input_rx, output_tx);
    let server_peer = async move {
        let expected_input = [
            commands.as_slice(),
            CONTROL_STDIN_EOF_MARKER.as_bytes(),
            b"\n",
        ]
        .concat();
        let mut received = Vec::new();
        let mut buffer = [0_u8; 32];
        while received.len() < expected_input.len() {
            let bytes_read = server.read(&mut buffer).await?;
            assert_ne!(bytes_read, 0, "client closed before sending commands");
            received.extend_from_slice(&buffer[..bytes_read]);
        }
        assert_eq!(received, expected_input);
        for fragment in output_fragments {
            for chunk in fragment.chunks(137) {
                server.write_all(chunk).await?;
                tokio::task::yield_now().await;
            }
        }
        Ok::<(), std::io::Error>(())
    };
    let output = collect_control_output(output_rx);

    let (_, _, output) = tokio::try_join!(drive, server_peer, output)?;
    assert_eq!(output, expected_output);
    Ok(())
}

#[tokio::test]
async fn protocol_exit_waits_for_transport_close() -> std::io::Result<()> {
    let (client, mut server) = tokio::io::duplex(4096);
    let (input_tx, input_rx) = tokio_mpsc::channel::<Vec<u8>>(1);
    input_tx
        .send(b"list-sessions\n".to_vec())
        .await
        .expect("send input");
    drop(input_tx);
    let (output_tx, mut output_rx) = tokio_mpsc::channel::<Vec<u8>>(4);
    let mut drive = tokio::spawn(drive_async_control(client, &[], input_rx, output_tx));

    let expected_input = format!("list-sessions\n{CONTROL_STDIN_EOF_MARKER}\n");
    let mut received = Vec::new();
    let mut buffer = [0_u8; 32];
    while received.len() < expected_input.len() {
        let bytes_read = server.read(&mut buffer).await?;
        assert_ne!(bytes_read, 0, "client closed before sending command");
        received.extend_from_slice(&buffer[..bytes_read]);
    }
    assert_eq!(received, expected_input.as_bytes());

    let transcript = b"%begin 1 1 1\n%end 1 1 1\n%exit\n";
    server.write_all(transcript).await?;
    let output = output_rx.recv().await.expect("control output remains open");
    assert_eq!(output, transcript);
    for _ in 0..3 {
        tokio::task::yield_now().await;
    }
    assert!(
        !drive.is_finished(),
        "protocol-shaped output must not replace transport completion"
    );

    drop(server);
    (&mut drive)
        .await
        .expect("control driver task joins")
        .expect("transport close completes control output");
    assert!(output_rx.recv().await.is_none());
    Ok(())
}

async fn collect_control_output(
    mut output_rx: tokio_mpsc::Receiver<Vec<u8>>,
) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    while let Some(bytes) = output_rx.recv().await {
        output.extend_from_slice(&bytes);
    }
    Ok(output)
}
