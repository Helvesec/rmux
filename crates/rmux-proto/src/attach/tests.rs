use super::{
    decode_attach_data_frame, encode_attach_data_into_slice, encode_attach_message,
    encode_data_like_message, AttachFrameDecoder, AttachMessage, AttachShellCommand,
    AttachedKeystroke, KeyDispatched, KEYSTROKE_TAG, WINDOWS_CONSOLE_KEYSTROKE_TAG,
};
use crate::{
    AttachedWindowsConsoleKey, RmuxError, TerminalGeometry, TerminalPixels, TerminalSize,
    DEFAULT_MAX_FRAME_LENGTH,
};

#[test]
fn data_messages_round_trip() {
    let message = AttachMessage::Data(b"hello".to_vec());
    let encoded = encode_attach_message(&message).expect("encode attach message");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);

    assert_eq!(
        decoder.next_message().expect("decode attach message"),
        Some(message)
    );
    assert_eq!(
        decoder.next_message().expect("buffer should be empty"),
        None
    );
}

#[test]
fn borrowed_data_frame_decode_returns_payload_without_consuming_buffer() {
    let encoded =
        encode_attach_message(&AttachMessage::Data(b"hello".to_vec())).expect("encode data");
    let frame = decode_attach_data_frame(&encoded)
        .expect("decode borrowed data")
        .expect("complete data frame");

    assert_eq!(frame.payload(), b"hello");
    assert_eq!(frame.frame_len(), encoded.len());
    assert!(decode_attach_data_frame(&encoded[..encoded.len() - 1])
        .expect("partial data is not an error")
        .is_none());
    assert!(decode_attach_data_frame(
        &encode_attach_message(&AttachMessage::Unlock).expect("unlock")
    )
    .expect("non-data frame is not an error")
    .is_none());
}

#[test]
fn attach_data_slice_encoder_matches_allocating_encoder() {
    let encoded =
        encode_attach_message(&AttachMessage::Data(b"hello".to_vec())).expect("encode data");
    let mut frame = [0_u8; 32];

    let len = encode_attach_data_into_slice(b"hello", &mut frame).expect("encode into slice");

    assert_eq!(&frame[..len], encoded.as_slice());
}

#[test]
fn attach_data_encoder_accepts_exact_payload_ceiling_and_rejects_next_byte() {
    let at_limit = vec![b'x'; DEFAULT_MAX_FRAME_LENGTH];
    let encoded =
        encode_attach_message(&AttachMessage::Data(at_limit)).expect("payload ceiling encodes");
    assert_eq!(
        encoded.len(),
        DEFAULT_MAX_FRAME_LENGTH + super::ATTACH_DATA_HEADER_LEN
    );

    assert_eq!(
        encode_attach_message(&AttachMessage::Data(vec![
            b'x';
            DEFAULT_MAX_FRAME_LENGTH + 1
        ])),
        Err(RmuxError::FrameTooLarge {
            length: DEFAULT_MAX_FRAME_LENGTH + 1,
            maximum: DEFAULT_MAX_FRAME_LENGTH,
        })
    );
}

#[test]
fn decoder_copies_small_data_payload_into_caller_scratch() {
    let data = encode_attach_message(&AttachMessage::Data(b"abc".to_vec())).expect("encode data");
    let unlock = encode_attach_message(&AttachMessage::Unlock).expect("encode unlock");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&data);
    decoder.push_bytes(&unlock);
    let mut scratch = [0_u8; 8];

    let payload = decoder
        .next_data_payload_into(&mut scratch)
        .expect("decode data payload")
        .expect("data frame should fit scratch");

    assert_eq!(payload, b"abc");
    assert_eq!(
        decoder.next_message().expect("decode next frame"),
        Some(AttachMessage::Unlock)
    );
}

#[test]
fn render_messages_round_trip() {
    let message = AttachMessage::Render(b"frame".to_vec());
    let encoded = encode_attach_message(&message).expect("encode attach render message");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);

    assert_eq!(
        decoder
            .next_message()
            .expect("decode attach render message"),
        Some(message)
    );
    assert_eq!(
        decoder.next_message().expect("buffer should be empty"),
        None
    );
}

#[test]
fn resize_messages_round_trip() {
    let message = AttachMessage::Resize(TerminalSize {
        cols: 120,
        rows: 40,
    });
    let encoded = encode_attach_message(&message).expect("encode attach resize");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);

    assert_eq!(
        decoder.next_message().expect("decode attach resize"),
        Some(message)
    );
}

#[test]
fn resize_geometry_messages_round_trip() {
    let message = AttachMessage::ResizeGeometry(
        TerminalGeometry::new(120, 40).with_pixels(TerminalPixels::new(1920, 1080)),
    );
    let encoded = encode_attach_message(&message).expect("encode attach geometry resize");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);

    assert_eq!(
        decoder
            .next_message()
            .expect("decode attach geometry resize"),
        Some(message)
    );
}

#[test]
fn keystroke_messages_round_trip() {
    let message = AttachMessage::Keystroke(AttachedKeystroke::new(b"\x1b[A".to_vec()));
    let encoded = encode_attach_message(&message).expect("encode attach keystroke");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);

    assert_eq!(encoded[0], KEYSTROKE_TAG);
    assert_eq!(
        decoder.next_message().expect("decode attach keystroke"),
        Some(message)
    );
}

#[test]
fn plain_keystroke_uses_legacy_vec_payload() {
    let bytes = b"\x1b[A".to_vec();
    let encoded = encode_attach_message(&AttachMessage::Keystroke(AttachedKeystroke::new(
        bytes.clone(),
    )))
    .expect("encode attach keystroke");
    let payload_len = u32::from_le_bytes(
        encoded[1..5]
            .try_into()
            .expect("legacy keystroke payload length"),
    ) as usize;
    let payload = &encoded[5..5 + payload_len];

    assert_eq!(encoded[0], KEYSTROKE_TAG);
    assert_eq!(
        bincode::deserialize::<Vec<u8>>(payload).expect("legacy vector payload decodes"),
        bytes
    );
}

#[test]
fn legacy_keystroke_payload_decodes_under_current_codec() {
    let bytes = vec![0x10];
    let payload = bincode::serialize(&bytes).expect("legacy keystroke payload encodes");
    let encoded =
        encode_data_like_message(KEYSTROKE_TAG, &payload).expect("encode legacy keystroke frame");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);

    assert_eq!(
        decoder
            .next_message()
            .expect("decode legacy attach keystroke"),
        Some(AttachMessage::Keystroke(AttachedKeystroke::new(bytes)))
    );
}

#[test]
fn keystroke_messages_preserve_windows_console_key() {
    let key = AttachedWindowsConsoleKey::new(0x44, 0x20, 0x04, 0x0008, 1);
    let message =
        AttachMessage::Keystroke(AttachedKeystroke::new(vec![0x04]).with_windows_console_key(key));
    let encoded = encode_attach_message(&message).expect("encode attach keystroke");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);

    assert_eq!(encoded[0], WINDOWS_CONSOLE_KEYSTROKE_TAG);
    assert_eq!(
        decoder.next_message().expect("decode attach keystroke"),
        Some(message)
    );
}

#[test]
fn key_dispatched_messages_round_trip() {
    let message = AttachMessage::KeyDispatched(KeyDispatched::new(3));
    let encoded = encode_attach_message(&message).expect("encode attach key dispatch ack");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);

    assert_eq!(
        decoder
            .next_message()
            .expect("decode attach key dispatch ack"),
        Some(message)
    );
}

#[test]
fn lock_messages_round_trip() {
    let message = AttachMessage::Lock("lock-command".to_owned());
    let encoded = encode_attach_message(&message).expect("encode attach lock");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);

    assert_eq!(
        decoder.next_message().expect("decode attach lock"),
        Some(message)
    );
}

#[test]
fn lock_shell_command_messages_round_trip() {
    let message = AttachMessage::LockShellCommand(AttachShellCommand::new(
        "lock-command".to_owned(),
        "pwsh.exe".to_owned(),
        "C:\\work".to_owned(),
    ));
    let encoded = encode_attach_message(&message).expect("encode attach lock shell command");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);

    assert_eq!(
        decoder.next_message().expect("decode attach lock"),
        Some(message)
    );
}

#[test]
fn unlock_messages_round_trip() {
    let message = AttachMessage::Unlock;
    let encoded = encode_attach_message(&message).expect("encode attach unlock");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);

    assert_eq!(
        decoder.next_message().expect("decode attach unlock"),
        Some(message)
    );
}

#[test]
fn decoder_handles_fragmented_messages() {
    let message = AttachMessage::Data(b"fragmented".to_vec());
    let encoded = encode_attach_message(&message).expect("encode attach message");
    let mut decoder = AttachFrameDecoder::new();

    decoder.push_bytes(&encoded[..3]);
    assert_eq!(
        decoder
            .next_message()
            .expect("partial message should not fail"),
        None
    );

    decoder.push_bytes(&encoded[3..]);
    assert_eq!(
        decoder.next_message().expect("fragment should decode"),
        Some(message)
    );
}

#[test]
fn attach_fragment_current_read_excludes_stale_payload() {
    let first = encode_attach_message(&AttachMessage::Data(vec![b'A'; 64])).unwrap();
    let second = AttachMessage::Data(b"y\x1b[?1049l".to_vec());
    let encoded = encode_attach_message(&second).unwrap();
    let mut scratch = [0_u8; 8192];
    scratch[..first.len()].copy_from_slice(&first);
    let mut decoder = AttachFrameDecoder::new();
    scratch[..5].copy_from_slice(&encoded[..5]);
    assert!(decode_attach_data_frame(&scratch[..5]).unwrap().is_none());
    decoder.push_bytes(&scratch[..5]);
    assert_eq!(decoder.next_message().unwrap(), None);
    scratch[..encoded.len() - 5].copy_from_slice(&encoded[5..]);
    decoder.push_bytes(&scratch[..encoded.len() - 5]);
    assert_eq!(decoder.next_message().unwrap(), Some(second));
    assert!(decoder.is_empty());
}

#[test]
fn attach_fragment_every_split_preserves_payload_and_coalesced_successor() {
    // CR is also a valid attach tag: stale completion can silently consume a
    // following frame instead of producing the unknown-tag error from `y`.
    for payload in [b"y\x1b[?1049l".as_slice(), b"\r\n".as_slice()] {
        let message = AttachMessage::Data(payload.to_vec());
        let frame = encode_attach_message(&message).unwrap();
        let successor = AttachMessage::Data(b"next\x1b[?1049l".to_vec());
        let next_frame = encode_attach_message(&successor).unwrap();
        for split in 1..frame.len() {
            let mut scratch = [b'A'; 8192];
            scratch[..split].copy_from_slice(&frame[..split]);
            assert!(decode_attach_data_frame(&scratch[..split])
                .unwrap()
                .is_none());
            let mut decoder = AttachFrameDecoder::new();
            decoder.push_bytes(&scratch[..split]);
            assert_eq!(decoder.next_message().unwrap(), None);

            let tail_len = frame.len() - split;
            scratch[..tail_len].copy_from_slice(&frame[split..]);
            scratch[tail_len..tail_len + next_frame.len()].copy_from_slice(&next_frame);
            decoder.push_bytes(&scratch[..tail_len + next_frame.len()]);
            assert_eq!(decoder.next_message().unwrap(), Some(message.clone()));
            assert_eq!(decoder.next_message().unwrap(), Some(successor.clone()));
            assert!(
                decoder.is_empty(),
                "split {split} must consume exactly two frames"
            );
        }
    }
}

#[test]
fn suspend_messages_round_trip() {
    let message = AttachMessage::Suspend;
    let encoded = encode_attach_message(&message).expect("encode attach suspend");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);

    assert_eq!(
        decoder.next_message().expect("decode attach suspend"),
        Some(message)
    );
}

#[test]
fn detach_kill_messages_round_trip() {
    let message = AttachMessage::DetachKill;
    let encoded = encode_attach_message(&message).expect("encode attach detach-kill");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);

    assert_eq!(
        decoder.next_message().expect("decode attach detach-kill"),
        Some(message)
    );
}

#[test]
fn server_exited_messages_round_trip_as_a_single_tag() {
    let message = AttachMessage::ServerExited;
    let encoded = encode_attach_message(&message).expect("encode attach server-exited");
    assert_eq!(
        encoded,
        [15],
        "server-exited is a published single-tag frame"
    );
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);

    assert_eq!(
        decoder.next_message().expect("decode attach server-exited"),
        Some(message)
    );
    assert!(decoder.is_empty());
}

#[test]
fn detach_exec_messages_round_trip() {
    let message = AttachMessage::DetachExec("exec /bin/bash".to_owned());
    let encoded = encode_attach_message(&message).expect("encode attach detach-exec");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);

    assert_eq!(
        decoder.next_message().expect("decode attach detach-exec"),
        Some(message)
    );
}

#[test]
fn detach_exec_shell_command_messages_round_trip() {
    let message = AttachMessage::DetachExecShellCommand(AttachShellCommand::new(
        "echo detached".to_owned(),
        "C:\\Program Files\\PowerShell\\7\\pwsh.exe".to_owned(),
        "C:\\repo".to_owned(),
    ));
    let encoded = encode_attach_message(&message).expect("encode attach detach-exec shell command");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);

    assert_eq!(
        decoder.next_message().expect("decode attach detach-exec"),
        Some(message)
    );
}

#[test]
fn decoder_rejects_unknown_tags() {
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&[250, 0, 0, 0, 0]);

    assert_eq!(
        decoder.next_message(),
        Err(RmuxError::Decode(
            "unknown attach-stream message tag 250".to_owned()
        ))
    );
}
