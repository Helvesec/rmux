use rmux_proto::WebTerminalPalette;

#[cfg(unix)]
mod imp {
    use std::fs::OpenOptions;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    use super::WebTerminalPalette;

    const QUERY_TIMEOUT: Duration = Duration::from_millis(220);
    const READ_BUF_SIZE: usize = 4096;

    pub(super) fn capture() -> Option<WebTerminalPalette> {
        let mut tty = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .ok()?;
        let fd = tty.as_raw_fd();
        let original = TermiosGuard::new(fd)?;

        tty.write_all(query_bytes().as_bytes()).ok()?;
        tty.flush().ok()?;

        let mut bytes = Vec::new();
        let deadline = Instant::now() + QUERY_TIMEOUT;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !poll_readable(fd, remaining) {
                break;
            }
            let mut buf = [0; READ_BUF_SIZE];
            match tty.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => bytes.extend_from_slice(&buf[..n]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(_) => break,
            }
        }

        drop(original);
        parse_theme(&String::from_utf8_lossy(&bytes))
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

        #[test]
        fn parses_vte_palette_replies() {
            let mut input = "\x1b]10;rgb:eeee/eeee/eeee\x1b\\\
                         \x1b]11;rgb:3333/4444/5555\x1b\\\
                         \x1b]12;rgb:ffff/0000/0000\x1b\\"
                .to_owned();
            for index in 0..16 {
                input.push_str(&format!("\x1b]4;{index};rgb:{index:04x}/0000/ffff\x1b\\"));
            }

            let theme = parse_theme(&input).expect("valid theme");

            assert_eq!(theme.foreground, "#eeeeee");
            assert_eq!(theme.background, "#334455");
            assert_eq!(theme.cursor, "#ff0000");
            assert_eq!(theme.ansi[0], "#0000ff");
            assert_eq!(theme.ansi[15], "#0000ff");
        }
    }
}

/// Best-effort capture of the local terminal palette for `web-share`.
pub(crate) fn capture_terminal_palette() -> Option<WebTerminalPalette> {
    imp::capture()
}

#[cfg(not(unix))]
mod imp {
    use super::WebTerminalPalette;

    pub(super) fn capture() -> Option<WebTerminalPalette> {
        None
    }
}
