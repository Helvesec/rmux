use super::{
    decode_attach_data_frame, encode_attach_data_into_slice, encode_attach_message,
    AttachFrameDecoder, AttachMessage, AttachShellCommand, AttachedKeystroke, KeyDispatched,
};
use crate::{RmuxError, TerminalGeometry, TerminalPixels, TerminalSize};

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
