use rmux_proto::WebTerminalPalette;

#[cfg(unix)]
mod imp {
    use std::fs::OpenOptions;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    use super::WebTerminalPalette;

    const QUERY_TIMEOUT: Duration = Duration::from_millis(800);
    const READ_POLL_SLICE: Duration = Duration::from_millis(25);
    const QUIET_DRAIN_TIMEOUT: Duration = Duration::from_millis(80);
    const READ_BUF_SIZE: usize = 4096;

    pub(super) fn capture() -> Option<WebTerminalPalette> {
        let mut tty = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .ok()?;
        capture_from_tty(&mut tty)
    }

    fn capture_from_tty(tty: &mut std::fs::File) -> Option<WebTerminalPalette> {
        let fd = tty.as_raw_fd();
        let original = TermiosGuard::new(fd)?;

        tty.write_all(query_bytes().as_bytes()).ok()?;
        tty.flush().ok()?;

        let mut bytes = Vec::new();
        let mut theme = None;
        let deadline = Instant::now() + QUERY_TIMEOUT;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !poll_readable(fd, remaining.min(READ_POLL_SLICE)) {
                continue;
            }
            if !read_available(tty, &mut bytes) {
                break;
            }
            theme = parse_theme(&String::from_utf8_lossy(&bytes));
            if theme.is_some() {
                break;
            }
        }

        drain_quiet_period(fd, tty, &mut bytes, QUIET_DRAIN_TIMEOUT);
        let _ = flush_input(fd);

        drop(original);
        theme.or_else(|| parse_theme(&String::from_utf8_lossy(&bytes)))
    }

    fn query_bytes() -> String {
        let mut query = String::from("\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b]12;?\x1b\\");
        for index in 0..16 {
            query.push_str(&format!("\x1b]4;{index};?\x1b\\"));
        }
        query
    }

    fn parse_theme(input: &str) -> Option<WebTerminalPalette> {
        let foreground = parse_osc_color(input, "10")?;
        let background = parse_osc_color(input, "11")?;
        let cursor = parse_osc_color(input, "12").unwrap_or_else(|| foreground.clone());
        let ansi: [Option<String>; 16] =
            std::array::from_fn(|index| parse_osc_color(input, &format!("4;{index}")));
        let ansi = ansi.into_iter().collect::<Option<Vec<_>>>()?;
        Some(WebTerminalPalette {
            foreground,
            background,
            cursor,
            ansi: ansi.try_into().ok()?,
        })
    }

    fn parse_osc_color(input: &str, code: &str) -> Option<String> {
        for terminator in ["\x1b\\", "\x07"] {
            let prefix = format!("\x1b]{code};");
            for segment in input.split(terminator) {
                let Some(value) = segment.strip_prefix(&prefix) else {
                    continue;
                };
                if let Some(hex) = parse_rgb(value) {
                    return Some(hex);
                }
            }
        }
        None
    }

    fn parse_rgb(value: &str) -> Option<String> {
        let rgb = value.strip_prefix("rgb:")?;
        let mut parts = rgb.split('/');
        let red = scale_channel(parts.next()?)?;
        let green = scale_channel(parts.next()?)?;
        let blue = scale_channel(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        Some(format!("#{red:02x}{green:02x}{blue:02x}"))
    }

    fn scale_channel(value: &str) -> Option<u8> {
        let digits = value.len();
        if digits == 0 || digits > 4 {
            return None;
        }
        let raw = u16::from_str_radix(value, 16).ok()?;
        let max = (1u32 << (digits * 4)) - 1;
        Some(((u32::from(raw) * 255 + (max / 2)) / max) as u8)
    }

    fn poll_readable(fd: libc::c_int, timeout: Duration) -> bool {
        let mut pollfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
        // SAFETY: `pollfd` points to a valid single-entry array for the duration of the call,
        // and `fd` is an open terminal descriptor owned by the caller.
        unsafe { libc::poll(&mut pollfd, 1, timeout_ms) > 0 && pollfd.revents & libc::POLLIN != 0 }
    }

    fn read_available(tty: &mut std::fs::File, bytes: &mut Vec<u8>) -> bool {
        loop {
            let mut buf = [0; READ_BUF_SIZE];
            match tty.read(&mut buf) {
                Ok(0) => return true,
                Ok(n) => {
                    bytes.extend_from_slice(&buf[..n]);
                    if n < READ_BUF_SIZE {
                        return true;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return true,
                Err(_) => return false,
            }
        }
    }

    fn drain_quiet_period(
        fd: libc::c_int,
        tty: &mut std::fs::File,
        bytes: &mut Vec<u8>,
        timeout: Duration,
    ) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !poll_readable(fd, remaining.min(READ_POLL_SLICE)) {
                continue;
            }
            if !read_available(tty, bytes) {
                break;
            }
        }
    }

    fn flush_input(fd: libc::c_int) -> std::io::Result<()> {
        // Best-effort cleanup for terminal emulators that answer OSC palette queries late.
        // Without this, unread replies can be consumed and echoed by the user's shell.
        // SAFETY: `fd` is borrowed for a `tcflush` call that only affects the terminal input
        // queue. Invalid descriptors are reported through the libc return value.
        if unsafe { libc::tcflush(fd, libc::TCIFLUSH) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    struct TermiosGuard {
        fd: libc::c_int,
        original: libc::termios,
    }

    impl TermiosGuard {
        fn new(fd: libc::c_int) -> Option<Self> {
            // SAFETY: `libc::termios` is a plain C struct whose all-zero value is only used as
            // an output buffer before being read after a successful `tcgetattr` call.
            let mut original = unsafe { std::mem::zeroed::<libc::termios>() };
            // SAFETY: `original` is a valid writable termios buffer and `fd` is expected to be
            // an open terminal descriptor.
            if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
                return None;
            }
            let mut raw = original;
            raw.c_lflag &= !(libc::ICANON | libc::ECHO);
            raw.c_cc[libc::VMIN] = 0;
            raw.c_cc[libc::VTIME] = 0;
            // SAFETY: `raw` was derived from a termios value returned by `tcgetattr` for this
            // descriptor, with only documented local-mode/control-byte fields adjusted.
            if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
                return None;
            }
            Some(Self { fd, original })
        }
    }

    impl Drop for TermiosGuard {
        fn drop(&mut self) {
            // SAFETY: `original` was captured from this descriptor by `tcgetattr`; restoring it
            // is best-effort and the return value is intentionally ignored during drop.
            let _ = unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.original) };
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::fs::File;
        use std::io::Write;
        use std::thread;

        fn palette_reply() -> String {
            let mut input = "\x1b]10;rgb:eeee/eeee/eeee\x1b\\\
                         \x1b]11;rgb:3333/4444/5555\x1b\\\
                         \x1b]12;rgb:ffff/0000/0000\x1b\\"
                .to_owned();
            for index in 0..16 {
                input.push_str(&format!("\x1b]4;{index};rgb:{index:04x}/0000/ffff\x1b\\"));
            }
            input
        }

        #[test]
        fn parses_vte_palette_replies() {
            let input = palette_reply();

            let theme = parse_theme(&input).expect("valid theme");

            assert_eq!(theme.foreground, "#eeeeee");
            assert_eq!(theme.background, "#334455");
            assert_eq!(theme.cursor, "#ff0000");
            assert_eq!(theme.ansi[0], "#0000ff");
            assert_eq!(theme.ansi[15], "#0000ff");
        }

        #[test]
        fn delayed_palette_replies_are_captured() {
            let (master, mut slave) = open_pty_pair();
            let mut responder_master = master.try_clone().expect("clone pty master");
            let reply = palette_reply();
            let responder = thread::spawn(move || {
                thread::sleep(Duration::from_millis(300));
                responder_master
                    .write_all(reply.as_bytes())
                    .expect("write reply");
            });

            let theme = capture_from_tty(&mut slave).expect("delayed theme reply");
            responder.join().expect("responder thread");

            assert_eq!(theme.foreground, "#eeeeee");
            assert_eq!(theme.background, "#334455");
            assert_eq!(theme.cursor, "#ff0000");
            assert_eq!(theme.ansi[15], "#0000ff");
        }

        /// Allocates through the production PTY entry point so the Darwin
        /// EREDRIVEOPEN lock and retry also cover this test.
        fn open_pty_pair() -> (File, File) {
            let (master, slave) = rmux_pty::PtyPair::open()
                .expect("allocate pty pair")
                .into_split();
            let master = master.into_owned_fd().expect("own pty master");
            // The responder relies on a blocking `write_all`, as with raw openpty.
            // SAFETY: `master` is an open descriptor owned here; F_GETFL/F_SETFL
            // only read and update its status flags.
            let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
            assert_ne!(flags, -1, "F_GETFL: {}", std::io::Error::last_os_error());
            // SAFETY: as above, clearing O_NONBLOCK on the same owned descriptor.
            let result = unsafe {
                libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK)
            };
            assert_ne!(result, -1, "F_SETFL: {}", std::io::Error::last_os_error());
            (File::from(master), File::from(slave.into_owned_fd()))
        }
    }
}

/// Best-effort capture of the local terminal palette for `web-share`.
pub(crate) fn capture_terminal_palette() -> Option<WebTerminalPalette> {
    imp::capture()
}

#[cfg(windows)]
mod imp {
    use super::WebTerminalPalette;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        GetConsoleScreenBufferInfoEx, GetStdHandle, BACKGROUND_BLUE, BACKGROUND_GREEN,
        BACKGROUND_INTENSITY, BACKGROUND_RED, CONSOLE_SCREEN_BUFFER_INFOEX, FOREGROUND_BLUE,
        FOREGROUND_GREEN, FOREGROUND_INTENSITY, FOREGROUND_RED, STD_OUTPUT_HANDLE,
    };

    const FOREGROUND_COLOR_BITS: u16 =
        FOREGROUND_BLUE | FOREGROUND_GREEN | FOREGROUND_RED | FOREGROUND_INTENSITY;
    const BACKGROUND_COLOR_BITS: u16 =
        BACKGROUND_BLUE | BACKGROUND_GREEN | BACKGROUND_RED | BACKGROUND_INTENSITY;

    pub(super) fn capture() -> Option<WebTerminalPalette> {
        // SAFETY: `GetStdHandle` does not dereference Rust memory and is safe to call with a
        // documented standard-handle constant. Invalid or redirected handles are handled below.
        let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return None;
        }

        let mut info = CONSOLE_SCREEN_BUFFER_INFOEX {
            cbSize: std::mem::size_of::<CONSOLE_SCREEN_BUFFER_INFOEX>() as u32,
            ..Default::default()
        };

        // SAFETY: `info` is a properly sized writable buffer for the Win32 call, and `handle`
        // came from `GetStdHandle`. Failure is reported as a zero return value.
        if unsafe { GetConsoleScreenBufferInfoEx(handle, &mut info) } == 0 {
            return None;
        }

        Some(capture_from_parts(info.wAttributes, info.ColorTable))
    }

    fn capture_from_parts(attributes: u16, color_table: [u32; 16]) -> WebTerminalPalette {
        let foreground = colorref_to_hex(color_table[foreground_index(attributes)]);
        let background = colorref_to_hex(color_table[background_index(attributes)]);
        let cursor = foreground.clone();
        let ansi = color_table.map(colorref_to_hex);

        WebTerminalPalette {
            foreground,
            background,
            cursor,
            ansi,
        }
    }

    fn foreground_index(attributes: u16) -> usize {
        usize::from(attributes & FOREGROUND_COLOR_BITS)
    }

    fn background_index(attributes: u16) -> usize {
        usize::from((attributes & BACKGROUND_COLOR_BITS) >> 4)
    }

    fn colorref_to_hex(color: u32) -> String {
        let red = (color & 0xff) as u8;
        let green = ((color >> 8) & 0xff) as u8;
        let blue = ((color >> 16) & 0xff) as u8;
        format!("#{red:02x}{green:02x}{blue:02x}")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn numbered_color_table() -> [u32; 16] {
            std::array::from_fn(|index| {
                let channel = index as u32;
                channel | (channel << 8) | (channel << 16)
            })
        }

        #[test]
        fn decodes_windows_colorref_bgr_order() {
            assert_eq!(colorref_to_hex(0x0033_2211), "#112233");
        }

        #[test]
        fn derives_foreground_and_background_from_console_attributes() {
            let palette = capture_from_parts(0x00e9, numbered_color_table());

            assert_eq!(palette.foreground, "#090909");
            assert_eq!(palette.background, "#0e0e0e");
            assert_eq!(palette.cursor, palette.foreground);
            assert_eq!(palette.ansi[1], "#010101");
            assert_eq!(palette.ansi[15], "#0f0f0f");
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::WebTerminalPalette;

    pub(super) fn capture() -> Option<WebTerminalPalette> {
        None
    }
}
