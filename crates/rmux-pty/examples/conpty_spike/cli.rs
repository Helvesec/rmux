//! Command-line arguments of the spike, and the payload child mode that
//! `--payload-bytes` spawns.

use std::io;
use std::io::Write;

#[derive(Debug)]
pub(crate) struct Args {
    pub(crate) mode: Mode,
    pub(crate) lines: u32,
    pub(crate) payload_bytes: Option<usize>,
    pub(crate) cols: i16,
    pub(crate) rows: i16,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Mode {
    TokioNamedPipe,
    BlockingAnonymousPipe,
    PlainCreateProcess,
    PlainStartupInfoEx,
}

impl Args {
    pub(crate) fn parse() -> Self {
        let mut mode = Mode::TokioNamedPipe;
        let mut lines = 10_000;
        let mut payload_bytes = None;
        let mut cols = 120;
        let mut rows = 40;
        let mut args = std::env::args().skip(1);

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--mode" => mode = parse_mode(&mut args),
                "--lines" => lines = parse_next(&mut args, "--lines"),
                "--payload-bytes" => payload_bytes = Some(parse_next(&mut args, "--payload-bytes")),
                "--cols" => cols = parse_next(&mut args, "--cols"),
                "--rows" => rows = parse_next(&mut args, "--rows"),
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                _ => {
                    eprintln!("unknown argument: {arg}");
                    print_usage();
                    std::process::exit(2);
                }
            }
        }

        Self {
            mode,
            lines,
            payload_bytes,
            cols,
            rows,
        }
    }
}

fn parse_next<T>(args: &mut impl Iterator<Item = String>, name: &str) -> T
where
    T: std::str::FromStr,
{
    args.next()
        .unwrap_or_else(|| {
            eprintln!("missing value for {name}");
            std::process::exit(2);
        })
        .parse()
        .unwrap_or_else(|_| {
            eprintln!("invalid value for {name}");
            std::process::exit(2);
        })
}

pub(crate) fn emit_payload_arg() -> Option<usize> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--emit-payload" {
            return Some(parse_next(&mut args, "--emit-payload"));
        }
    }
    None
}

pub(crate) fn emit_payload(bytes: usize) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(b"RMUX_CONPTY_READY\r\n")?;

    let chunk = vec![b'x'; 64 * 1024];
    let mut remaining = bytes;
    while remaining > 0 {
        let len = remaining.min(chunk.len());
        stdout.write_all(&chunk[..len])?;
        remaining -= len;
    }
    stdout.write_all(b"\r\n")?;
    stdout.flush()
}

fn parse_mode(args: &mut impl Iterator<Item = String>) -> Mode {
    let value = args.next().unwrap_or_else(|| {
        eprintln!("missing value for --mode");
        std::process::exit(2);
    });

    match value.as_str() {
        "tokio-named-pipe-overlapped" => Mode::TokioNamedPipe,
        "blocking-anonymous-pipe" => Mode::BlockingAnonymousPipe,
        "plain-create-process" => Mode::PlainCreateProcess,
        "plain-startupinfoex" => Mode::PlainStartupInfoEx,
        _ => {
            eprintln!("invalid mode: {value}");
            print_usage();
            std::process::exit(2);
        }
    }
}

fn print_usage() {
    eprintln!(
        "usage: cargo run -p rmux-pty --example conpty_spike -- [--mode MODE] [--lines N] [--payload-bytes N] [--cols N] [--rows N]\n\nmodes:\n  tokio-named-pipe-overlapped\n  blocking-anonymous-pipe\n  plain-create-process\n  plain-startupinfoex"
    );
}
