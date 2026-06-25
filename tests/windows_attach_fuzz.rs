#![cfg(windows)]

use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rmux_pty::{
    write_windows_console_key, ChildCommand, SpawnedPty, TerminalSize, WindowsConsoleKeyEvent,
};
use windows_sys::Win32::System::Console::{
    LEFT_ALT_PRESSED, LEFT_CTRL_PRESSED, RIGHT_ALT_PRESSED, RIGHT_CTRL_PRESSED, SHIFT_PRESSED,
};

const DONE_MARKER: &[u8] = b"RMUX_FUZZ_DONE";
const TIMEOUT_COMMAND: &[u8] = b"timeout /T 10000 & echo RMUX_FUZZ_DONE\r\n";
const CASE_TIMEOUT: Duration = Duration::from_millis(900);
const SETUP_TIMEOUT: Duration = Duration::from_secs(6);

#[test]
#[ignore = "slow Windows ConPTY comparison fuzz test"]
fn windows_timeout_key_behavior_matches_direct_console() -> Result<(), Box<dyn Error>> {
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
    let seed = std::env::var("RMUX_WINDOWS_ATTACH_FUZZ_SEED")
        .ok()
        .and_then(|value| u64::from_str_radix(value.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0x5eed_0d65_2026_0619);
    let random_count = std::env::var("RMUX_WINDOWS_ATTACH_FUZZ_RANDOM")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(24);
    let mut cases = corpus_cases();
    cases.extend(random_cases(seed, random_count));

    let mut mismatches = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let direct = run_direct_timeout(case)?;
        let through_rmux = run_rmux_timeout(&binary, case, index)?;
        println!(
            "{index:03} {:<18} direct={} rmux={}",
            case.name, direct, through_rmux
        );
        if direct != through_rmux {
            mismatches.push(format!(
                "#{index} {}: direct={direct}, rmux={through_rmux}",
                case.name
            ));
        }
    }

    assert!(
        mismatches.is_empty(),
        "Windows key behavior mismatches:\n{}",
        mismatches.join("\n")
    );
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct KeyCase {
    name: &'static str,
    event: WindowsConsoleKeyEvent,
}

fn run_direct_timeout(case: &KeyCase) -> Result<bool, Box<dyn Error>> {
    let mut spawned = ChildCommand::new("C:\\Windows\\System32\\cmd.exe")
        .args(["/D", "/K"])
        .size(TerminalSize::new(100, 30))
        .spawn()?;
    let io = spawned.master().try_clone_io()?;
    wait_for_needle_or_error(&mut spawned, b">", SETUP_TIMEOUT)?;
    io.write_all(TIMEOUT_COMMAND)?;
    thread::sleep(Duration::from_millis(250));

    write_windows_console_key(spawned.child().pid(), case.event)?;
    let returned = wait_for_needle_or_terminate(&mut spawned, DONE_MARKER, CASE_TIMEOUT)?;
    if returned {
        terminate_spawned(&mut spawned);
    }
    Ok(returned)
}

fn run_rmux_timeout(binary: &Path, case: &KeyCase, index: usize) -> Result<bool, Box<dyn Error>> {
    let label = format!("win-fuzz-{}-{index}", std::process::id());
    let setup = (|| -> Result<bool, Box<dyn Error>> {
        run_rmux(
            binary,
            &label,
            ["new-session", "-d", "-s", "fuzz", "cmd.exe", "/D", "/K"],
        )?;
        run_rmux(binary, &label, ["set-option", "-g", "status", "off"])?;

        let mut attach = ChildCommand::new(binary)
            .args(["-L", &label, "attach-session", "-t", "fuzz"])
            .size(TerminalSize::new(100, 30))
            .spawn()?;
        let io = attach.master().try_clone_io()?;
        wait_for_needle_or_error(&mut attach, b">", SETUP_TIMEOUT)?;
        io.write_all(TIMEOUT_COMMAND)?;
        thread::sleep(Duration::from_millis(250));

        write_windows_console_key(attach.child().pid(), case.event)?;
        let returned = wait_for_needle_or_terminate(&mut attach, DONE_MARKER, CASE_TIMEOUT)?;
        if returned {
            terminate_spawned(&mut attach);
        }
        Ok(returned)
    })();

    let _ = run_rmux(binary, &label, ["kill-server"]);
    setup
}

fn run_rmux<const N: usize>(
    binary: &Path,
    label: &str,
    args: [&str; N],
) -> Result<(), Box<dyn Error>> {
    let status = Command::new(binary)
        .arg("-L")
        .arg(label)
        .args(args)
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!("rmux command failed with {status}")).into());
    }
    Ok(())
}

fn wait_for_needle_or_error(
    spawned: &mut SpawnedPty,
    needle: &[u8],
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    if wait_for_needle_or_terminate(spawned, needle, timeout)? {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "timed out waiting for {:?}",
            String::from_utf8_lossy(needle)
        ),
    )
    .into())
}

fn wait_for_needle_or_terminate(
    spawned: &mut SpawnedPty,
    needle: &[u8],
    timeout: Duration,
) -> Result<bool, Box<dyn Error>> {
    let io = spawned.master().try_clone_io()?;
    let needle = needle.to_vec();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = read_until_io(&io, &needle).map_err(|error| error.to_string());
        let _ = tx.send(result);
    });

    match rx.recv_timeout(timeout) {
        Ok(Ok(found)) => Ok(found),
        Ok(Err(error)) => Err(io::Error::other(error).into()),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            terminate_spawned(spawned);
            let _ = rx.recv_timeout(Duration::from_secs(2));
            Ok(false)
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(io::Error::other("ConPTY reader thread disconnected").into())
        }
    }
}

fn read_until_io(io: &rmux_pty::PtyIo, needle: &[u8]) -> io::Result<bool> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let bytes_read = io.read(&mut buffer)?;
        if bytes_read == 0 {
            return Ok(false);
        }
        output.extend_from_slice(&buffer[..bytes_read]);
        if output.windows(needle.len()).any(|window| window == needle) {
            return Ok(true);
        }
    }
}

fn terminate_spawned(spawned: &mut SpawnedPty) {
    let _ = spawned.child().terminate_forcefully();
    let _ = spawned.child_mut().wait();
}

fn corpus_cases() -> Vec<KeyCase> {
    let mut cases = vec![
        plain("plain-x", b'X'),
        plain("plain-space", 0x20),
        key("enter", 0x0d, 0x1c, 0x0d, 0),
        key("tab", 0x09, 0x0f, 0x09, 0),
        key("escape", 0x1b, 0x01, 0x1b, 0),
        key("ctrl-space", 0x20, 0x39, 0x00, LEFT_CTRL_PRESSED),
    ];
    cases.extend((b'A'..=b'Z').map(ctrl_letter_case));
    cases
}

fn random_cases(seed: u64, count: usize) -> Vec<KeyCase> {
    let mut rng = XorShift64(seed);
    let mut cases = Vec::with_capacity(count);
    for index in 0..count {
        let letter = b'A' + (rng.next() % 26) as u8;
        let modifier = match rng.next() % 8 {
            0 => 0,
            1 => SHIFT_PRESSED,
            2 => LEFT_CTRL_PRESSED,
            3 => RIGHT_CTRL_PRESSED,
            4 => LEFT_ALT_PRESSED,
            5 => RIGHT_ALT_PRESSED,
            6 => LEFT_CTRL_PRESSED | SHIFT_PRESSED,
            _ => LEFT_CTRL_PRESSED | LEFT_ALT_PRESSED,
        };
        let unicode = unicode_for_letter(letter, modifier);
        cases.push(KeyCase {
            name: Box::leak(format!("rand-{index}-{letter:02x}-{modifier:x}").into_boxed_str()),
            event: WindowsConsoleKeyEvent::new(
                u16::from(letter),
                scan_code_for_letter(letter),
                unicode,
                modifier,
                1,
            ),
        });
    }
    cases
}

fn plain(name: &'static str, character: u8) -> KeyCase {
    key(
        name,
        character,
        scan_code_for_plain(character),
        character as u16,
        0,
    )
}

fn ctrl_letter_case(letter: u8) -> KeyCase {
    KeyCase {
        name: Box::leak(format!("ctrl-{}", char::from(letter)).into_boxed_str()),
        event: WindowsConsoleKeyEvent::new(
            u16::from(letter),
            scan_code_for_letter(letter),
            u16::from(letter - b'A' + 1),
            LEFT_CTRL_PRESSED,
            1,
        ),
    }
}

fn key(
    name: &'static str,
    virtual_key_code: u8,
    virtual_scan_code: u16,
    unicode_char: u16,
    control_key_state: u32,
) -> KeyCase {
    KeyCase {
        name,
        event: WindowsConsoleKeyEvent::new(
            u16::from(virtual_key_code),
            virtual_scan_code,
            unicode_char,
            control_key_state,
            1,
        ),
    }
}

fn unicode_for_letter(letter: u8, modifier: u32) -> u16 {
    if modifier & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0 {
        return u16::from(letter - b'A' + 1);
    }
    if modifier & SHIFT_PRESSED != 0 {
        return u16::from(letter);
    }
    u16::from(letter.to_ascii_lowercase())
}

fn scan_code_for_plain(character: u8) -> u16 {
    match character {
        b' ' => 0x39,
        b'A'..=b'Z' | b'a'..=b'z' => scan_code_for_letter(character.to_ascii_uppercase()),
        _ => 0,
    }
}

fn scan_code_for_letter(letter: u8) -> u16 {
    match letter {
        b'A' => 0x1e,
        b'B' => 0x30,
        b'C' => 0x2e,
        b'D' => 0x20,
        b'E' => 0x12,
        b'F' => 0x21,
        b'G' => 0x22,
        b'H' => 0x23,
        b'I' => 0x17,
        b'J' => 0x24,
        b'K' => 0x25,
        b'L' => 0x26,
        b'M' => 0x32,
        b'N' => 0x31,
        b'O' => 0x18,
        b'P' => 0x19,
        b'Q' => 0x10,
        b'R' => 0x13,
        b'S' => 0x1f,
        b'T' => 0x14,
        b'U' => 0x16,
        b'V' => 0x2f,
        b'W' => 0x11,
        b'X' => 0x2d,
        b'Y' => 0x15,
        b'Z' => 0x2c,
        _ => 0,
    }
}

struct XorShift64(u64);

impl XorShift64 {
    fn next(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        value
    }
}
