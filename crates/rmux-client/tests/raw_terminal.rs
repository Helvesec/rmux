use std::error::Error;
use std::os::unix::net::UnixStream;
use std::panic::{catch_unwind, AssertUnwindSafe};

use rmux_client::{AttachError, RawTerminal};
use rmux_pty::PtyPair;
use rustix::termios::{
    tcgetattr, tcsetattr, LocalModes, OptionalActions, SpecialCodeIndex, Termios,
};

#[test]
fn explicit_restore_restores_original_termios() -> Result<(), Box<dyn Error>> {
    let pair = PtyPair::open()?;
    let original = prepare_canonical_termios(pair.slave())?;

    let raw_terminal = RawTerminal::from_fd(pair.slave())?;
    let raw_termios = tcgetattr(pair.slave())?;

    assert_raw_mode_applied(&original, &raw_termios);

    raw_terminal.restore()?;
    raw_terminal.restore()?;

    let restored = tcgetattr(pair.slave())?;
    assert_termios_eq(&original, &restored);

    drop(raw_terminal);

    let restored_after_drop = tcgetattr(pair.slave())?;
    assert_termios_eq(&original, &restored_after_drop);
    Ok(())
}

#[test]
fn restore_survives_original_descriptor_drop() -> Result<(), Box<dyn Error>> {
    let (_master, slave) = PtyPair::open()?.into_split();
    let observer = slave.try_clone()?;
    let original = prepare_canonical_termios(&observer)?;

    let raw_terminal = RawTerminal::from_fd(&slave)?;
    let raw_termios = tcgetattr(&observer)?;
    assert_raw_mode_applied(&original, &raw_termios);

    drop(slave);

    raw_terminal.restore()?;

    let restored = tcgetattr(&observer)?;
    assert_termios_eq(&original, &restored);

    drop(raw_terminal);

    let restored_after_drop = tcgetattr(&observer)?;
    assert_termios_eq(&original, &restored_after_drop);
    Ok(())
}

#[test]
fn drop_restores_original_termios() -> Result<(), Box<dyn Error>> {
    let pair = PtyPair::open()?;
    let original = prepare_canonical_termios(pair.slave())?;

    {
        let _raw_terminal = RawTerminal::from_fd(pair.slave())?;
        let raw_termios = tcgetattr(pair.slave())?;
        assert_raw_mode_applied(&original, &raw_termios);
    }

    let restored = tcgetattr(pair.slave())?;
    assert_termios_eq(&original, &restored);
    Ok(())
}

#[test]
fn drop_restores_original_termios_after_panic() -> Result<(), Box<dyn Error>> {
    let pair = PtyPair::open()?;
    let original = prepare_canonical_termios(pair.slave())?;

    let panic_result = catch_unwind(AssertUnwindSafe(|| {
        let _raw_terminal = RawTerminal::from_fd(pair.slave()).expect("enter raw terminal mode");
        panic!("intentional panic after entering raw mode");
    }));

    assert!(
        panic_result.is_err(),
        "catch_unwind should capture the panic"
    );

    let restored = tcgetattr(pair.slave())?;
    assert_termios_eq(&original, &restored);
    Ok(())
}

#[test]
fn from_fd_rejects_non_terminal_descriptors() -> Result<(), Box<dyn Error>> {
    let (socket, _peer) = UnixStream::pair()?;
    let error = RawTerminal::from_fd(&socket).expect_err("sockets are not terminal devices");

    assert!(
        matches!(error, AttachError::Termios(_)),
        "expected termios error for non-terminal fd, got {error:?}"
    );

    Ok(())
}

fn prepare_canonical_termios<Fd>(fd: &Fd) -> Result<Termios, Box<dyn Error>>
where
    Fd: std::os::fd::AsFd,
{
    let mut termios = tcgetattr(fd)?;
    termios.local_modes |= LocalModes::ICANON | LocalModes::ECHO;
    termios.special_codes[SpecialCodeIndex::VMIN] = 4;
    termios.special_codes[SpecialCodeIndex::VTIME] = 9;
    tcsetattr(fd, OptionalActions::Now, &termios)?;
    Ok(tcgetattr(fd)?)
}

fn assert_raw_mode_applied(original: &Termios, raw: &Termios) {
    assert!(
        original.local_modes.contains(LocalModes::ICANON),
        "baseline terminal must start in canonical mode"
    );
    assert!(
        original.local_modes.contains(LocalModes::ECHO),
        "baseline terminal must start with echo enabled"
    );
    assert!(
        !raw.local_modes.contains(LocalModes::ICANON),
        "raw mode must clear ICANON"
    );
    assert!(
        !raw.local_modes.contains(LocalModes::ECHO),
        "raw mode must clear ECHO"
    );
    assert_eq!(raw.special_codes[SpecialCodeIndex::VMIN], 1);
    assert_eq!(raw.special_codes[SpecialCodeIndex::VTIME], 0);
}

fn assert_termios_eq(expected: &Termios, actual: &Termios) {
    assert_eq!(actual.input_modes, expected.input_modes);
    assert_eq!(actual.output_modes, expected.output_modes);
    assert_eq!(actual.control_modes, expected.control_modes);
    assert_eq!(actual.local_modes, expected.local_modes);
    #[cfg(target_os = "linux")]
    assert_eq!(actual.line_discipline, expected.line_discipline);
    assert_eq!(
        format!("{:?}", actual.special_codes),
        format!("{:?}", expected.special_codes)
    );
    assert_eq!(actual.input_speed(), expected.input_speed());
    assert_eq!(actual.output_speed(), expected.output_speed());
}
