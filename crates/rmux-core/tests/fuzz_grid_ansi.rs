//! Adversarial fuzz of the InputParser + Screen pipeline.
//!
//! Goal: trigger panics / OOM / hangs / wide-char divergences in the ANSI
//! escape sequence parser. This is a deterministic xorshift-driven fuzz, so
//! every failure prints the seed and input that reproduce it.
//!
//! Categories exercised:
//! * CSI with huge params (`ESC[1000000A`, `ESC[1;9999H`).
//! * CSI with >PARAM_LIST_MAX (24) parameters.
//! * CSI `?` private-mode permutations with stress values.
//! * SGR `38;2;R;G;B` with missing components, `38:2:...` colon forms.
//! * OSC 8 (hyperlink) with garbage URIs and giant payloads.
//! * DCS with unbalanced ST (truncated, embedded ESC, BEL).
//! * APC (kitty) / sixel oversize / passthrough payload limits.
//! * UTF-8: emoji + ZWJ + skin tone interleaved at #{=N} boundaries.

use rmux_core::input::{InputParser, ScreenWriter};
use rmux_core::{
    text_width, truncate_to_width, GridRenderOptions, Screen, ScreenCaptureRange, Utf8Config,
};
use rmux_proto::TerminalSize;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

// ─── deterministic PRNG ─────────────────────────────────────────────

struct XorShift64(u64);
impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0xdead_beef_cafe_babe
        } else {
            seed
        })
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }
    fn pick<T: Copy>(&mut self, slice: &[T]) -> T {
        let i = (self.next_u32() as usize) % slice.len();
        slice[i]
    }
    fn range(&mut self, lo: usize, hi_exclusive: usize) -> usize {
        if hi_exclusive <= lo {
            return lo;
        }
        lo + (self.next_u32() as usize) % (hi_exclusive - lo)
    }
}

fn make_screen() -> Screen {
    Screen::new(TerminalSize { cols: 80, rows: 24 }, 1_000)
}

// ─── escape-sequence corpus generators ──────────────────────────────

fn push_csi_big_params(rng: &mut XorShift64, out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b[");
    let n_params = rng.range(1, 30); // intentionally include >PARAM_LIST_MAX
    for i in 0..n_params {
        if i > 0 {
            out.push(b';');
        }
        let v: i64 = match rng.next_u32() % 5 {
            0 => 0,
            1 => 1,
            2 => 9_999_999,
            3 => i32::MAX as i64,
            _ => (rng.next_u32() % 1_000_000) as i64,
        };
        out.extend_from_slice(v.to_string().as_bytes());
    }
    let final_byte = rng.pick(b"ABCDEFGHJKLMPSTXZ@bdfghlmnsu");
    out.push(final_byte);
}

fn push_csi_private(rng: &mut XorShift64, out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b[?");
    let v = match rng.next_u32() % 6 {
        0 => 1,
        1 => 25,
        2 => 1000,
        3 => 1049,
        4 => 9_999_999,
        _ => rng.next_u32(),
    };
    out.extend_from_slice(v.to_string().as_bytes());
    out.push(rng.pick(b"hl"));
}

fn push_sgr_truecolor(rng: &mut XorShift64, out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b[");
    // 38;2;R;G;B with possible missing tail components.
    let kind = rng.next_u32() % 6;
    match kind {
        0 => out.extend_from_slice(b"38;2;255;128;0"),
        1 => out.extend_from_slice(b"38;2;255;128"), // missing B
        2 => out.extend_from_slice(b"38;2;255"),     // missing G,B
        3 => out.extend_from_slice(b"38;2"),         // missing R,G,B
        4 => out.extend_from_slice(b"38:2::255:128:0"), // ISO colon form w/ empty cs
        _ => out.extend_from_slice(b"38;5;9999999"), // 256-color huge
    }
    out.push(b'm');
}

fn push_osc_hyperlink(rng: &mut XorShift64, out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b]8;");
    // Garbage parameters block.
    let plen = rng.range(0, 64);
    for _ in 0..plen {
        out.push((rng.next_u32() as u8).saturating_add(0x20));
    }
    out.push(b';');
    // Garbage URI.
    let ulen = rng.range(0, 256);
    for _ in 0..ulen {
        out.push(match rng.next_u32() % 4 {
            0 => b'\n', // raw newline inside URI
            1 => 0x07,  // BEL — terminates the OSC
            2 => (rng.next_u32() % 256) as u8,
            _ => (rng.next_u32() as u8) | 0x80, // high-bit / invalid utf8
        });
    }
    // Possibly leave it dangling (no ST). Otherwise close with BEL or ST.
    match rng.next_u32() % 3 {
        0 => out.extend_from_slice(b"\x1b\\"),
        1 => out.push(0x07),
        _ => {}
    }
}

fn push_dcs_unbalanced(rng: &mut XorShift64, out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1bP");
    // Some params.
    if rng.next_u32() & 1 == 0 {
        out.extend_from_slice(b"1;2");
    }
    out.push(b'q'); // sixel-final
                    // Garbage payload.
    let n = rng.range(0, 512);
    for _ in 0..n {
        out.push(rng.next_u32() as u8);
    }
    // Maybe an embedded ESC w/o ST.
    if rng.next_u32().is_multiple_of(4) {
        out.push(0x1b);
    }
    // Maybe no terminator at all.
    if !rng.next_u32().is_multiple_of(3) {
        out.extend_from_slice(b"\x1b\\");
    }
}

fn push_apc_kitty(rng: &mut XorShift64, out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b_");
    let n = rng.range(0, 4096);
    for _ in 0..n {
        out.push((rng.next_u32() as u8).saturating_add(1));
    }
    if rng.next_u32() & 1 == 0 {
        out.extend_from_slice(b"\x1b\\");
    }
}

fn push_random_utf8(rng: &mut XorShift64, out: &mut Vec<u8>) {
    // Emit a small grab-bag of zero-width / wide / combining sequences.
    let snippets: &[&[u8]] = &[
        "👨\u{200D}👩\u{200D}👧\u{200D}👦".as_bytes(), // family ZWJ
        "🏃\u{200D}♂\u{FE0F}".as_bytes(),              // ZWJ + VS16
        "👋\u{1F3FF}".as_bytes(),                      // skin tone
        "🇨🇭🇫🇷".as_bytes(),                             // RIS flags
        "한".as_bytes(),                               // composed Hangul
        "ᄒ\u{1175}ᆫ".as_bytes(),                       // jamo
        "표".as_bytes(),
        "\u{0301}".as_bytes(),  // lone combining
        "A\u{200D}".as_bytes(), // ZWJ after ASCII
        "\u{FEFF}".as_bytes(),  // BOM
    ];
    let pick = snippets[(rng.next_u32() as usize) % snippets.len()];
    out.extend_from_slice(pick);
}

fn build_fuzz_input(rng: &mut XorShift64, target_len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(target_len + 64);
    while out.len() < target_len {
        match rng.next_u32() % 12 {
            0 => push_csi_big_params(rng, &mut out),
            1 => push_csi_private(rng, &mut out),
            2 => push_sgr_truecolor(rng, &mut out),
            3 => push_osc_hyperlink(rng, &mut out),
            4 => push_dcs_unbalanced(rng, &mut out),
            5 => push_apc_kitty(rng, &mut out),
            6 => push_random_utf8(rng, &mut out),
            7 => out.push(rng.next_u32() as u8),
            8 => out.extend_from_slice(b"\x1b"), // bare ESC
            9 => out.extend_from_slice(b"\x1b[1;9999H"),
            10 => out.extend_from_slice(b"\x1b[1000A"),
            _ => out.extend_from_slice(b"hello "),
        }
    }
    out
}

// ─── parser/screen driver ───────────────────────────────────────────

fn drive(input: &[u8]) {
    let mut parser = InputParser::new();
    let mut screen = make_screen();
    parser.parse(input, &mut screen);
    // Drive replies + take_since_ground to make sure state APIs don't trip.
    let _ = parser.take_replies();
    let _ = parser.take_since_ground();
    let _ = parser.pending_bytes();
    let _ = screen.cursor_x();
    let _ = screen.cursor_y();
}

// ─── tests ──────────────────────────────────────────────────────────

const RANDOM_ESCAPE_WORKER: &str = "RMUX_RANDOM_ESCAPE_FUZZ_WORKER";
const RANDOM_ESCAPE_TIMEOUT: Duration = Duration::from_secs(60);
const RANDOM_ESCAPE_CASES_PER_SEED: u32 = 128;

// Per-case wall time measures host scheduling as well as parser work. Run a fixed
// corpus instead, with a process boundary so the aggregate deadline is enforceable.
fn run_random_escape_corpus() {
    let mut iterations: u64 = 0;
    for seed in 1u64..=64 {
        let mut rng = XorShift64::new(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        for _ in 0..RANDOM_ESCAPE_CASES_PER_SEED {
            let target = ((rng.next_u32() as usize) % 8192) + 16;
            let buf = build_fuzz_input(&mut rng, target);
            drive(&buf);
            iterations += 1;
        }
    }
    eprintln!("fuzz: ran {iterations} iterations without panic");
}

fn wait_for_fuzz_worker(mut child: Child) -> Output {
    let deadline = Instant::now() + RANDOM_ESCAPE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .expect("collect fuzz worker output")
            }
            Ok(None) if Instant::now() >= deadline => {
                child.kill().expect("kill timed-out fuzz worker");
                let output = child
                    .wait_with_output()
                    .expect("collect timed-out fuzz worker output");
                panic!(
                    "random escape corpus exceeded {RANDOM_ESCAPE_TIMEOUT:?}\nstdout:\n{}\nstderr:\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("failed to poll random escape fuzz worker: {error}");
            }
        }
    }
}

#[test]
fn fuzz_random_escape_sequences_does_not_panic() {
    if std::env::var_os(RANDOM_ESCAPE_WORKER).is_some() {
        run_random_escape_corpus();
        return;
    }

    let child = Command::new(std::env::current_exe().expect("resolve fuzz test executable"))
        .args([
            "--exact",
            "fuzz_random_escape_sequences_does_not_panic",
            "--nocapture",
        ])
        .env(RANDOM_ESCAPE_WORKER, "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn isolated random escape fuzz worker");
    let output = wait_for_fuzz_worker(child);
    assert!(
        output.status.success(),
        "random escape fuzz worker failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn fuzz_csi_huge_cursor_movements() {
    // The exact problem cases called out by the prompt.
    let cases: &[&[u8]] = &[
        b"\x1b[1000A",
        b"\x1b[1;9999H",
        b"\x1b[99999999;99999999H",
        b"\x1b[2147483647A",
        b"\x1b[2147483647;2147483647H",
        b"\x1b[2147483648A", // > i32::MAX → split fails (return false)
        b"\x1b[-1A",         // negative — split fails
        b"\x1b[A",           // missing param defaults to 1
        b"\x1b[;A",          // empty/missing
        b"\x1b[1;2;3;4;5;6;7;8;9;10;11;12;13;14;15;16;17;18;19;20;21;22;23;24;25H", // >PARAM_LIST_MAX
        b"\x1b[1r", // DECSTBM single param
        b"\x1b[0;0r",
        b"\x1b[1;1H\x1b[1J",
        b"\x1b[?25h\x1b[?25l\x1b[?1049h\x1b[?1049l",
    ];
    for input in cases {
        drive(input);
    }
}

#[test]
fn fuzz_dcs_unterminated_payload() {
    // Build a DCS with no ST and lots of bytes — must not OOM.
    let mut buf: Vec<u8> = Vec::with_capacity(2_000_000);
    buf.extend_from_slice(b"\x1bP1;2q");
    for i in 0..1_900_000u32 {
        buf.push((i % 200 + 33) as u8);
    }
    // intentionally no ST
    let start = Instant::now();
    drive(&buf);
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "elapsed: {:?}",
        start.elapsed()
    );
}

#[test]
fn fuzz_osc_hyperlink_pathological_uris() {
    let cases: &[&[u8]] = &[
        b"\x1b]8;;http://x\x1b\\",
        b"\x1b]8;;\x07",
        b"\x1b]8;id=\x00\x00\x00;\x1b\\", // embedded NULs
        b"\x1b]8;id=a;file:///\x07",
        b"\x1b]8;;javascript:alert(1)\x07", // we don't sanitize — just don't crash
        b"\x1b]8;params with no semi\x07",  // malformed: missing ';'
        b"\x1b]8;;\xff\xff\xfe\xfd\xfc\x07", // invalid utf-8 in URI
    ];
    for input in cases {
        drive(input);
    }
    // Giant URI.
    let mut big = b"\x1b]8;;".to_vec();
    big.extend(std::iter::repeat_n(b'A', 2_000_000));
    big.extend_from_slice(b"\x07");
    let start = Instant::now();
    drive(&big);
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[test]
fn fuzz_sgr_truecolor_missing_components() {
    let cases: &[&[u8]] = &[
        b"\x1b[38;2;255;128;0m",
        b"\x1b[38;2;255;128m", // missing B
        b"\x1b[38;2;255m",     // missing G,B
        b"\x1b[38;2m",         // missing R,G,B
        b"\x1b[48;2;255;128;0m",
        b"\x1b[38;5;255m",
        b"\x1b[38;5m",             // missing index
        b"\x1b[38:2::255:128:0m",  // ISO colon form, empty colourspace
        b"\x1b[38:2:1:2:3:4:5:6m", // way too many colon components
        b"\x1b[0;1;4;7;38;2;1;2;3;48;2;4;5;6;39;49;58:2:0:0:0m",
    ];
    for input in cases {
        drive(input);
    }
}

fn joined_screen_text(screen: &Screen) -> String {
    let range = ScreenCaptureRange {
        start_is_absolute: true,
        ..ScreenCaptureRange::default()
    };
    String::from_utf8(screen.capture_transcript(
        range,
        GridRenderOptions {
            join_wrapped: true,
            ..GridRenderOptions::default()
        },
    ))
    .expect("screen capture must be UTF-8")
}

#[test]
fn fuzz_resize_reflow_preserves_ascii_end_cursor_matrix() {
    let alphabet = b"abcdefghijklmnopqrstuvwxyz";
    for old_width in 2..=12_u16 {
        for new_width in 1..=12_u16 {
            if old_width == new_width {
                continue;
            }
            for length in 1..=30_usize {
                let text = (0..length)
                    .map(|index| alphabet[index % alphabet.len()])
                    .collect::<Vec<_>>();
                let mut parser = InputParser::new();
                let mut screen = Screen::new(
                    TerminalSize {
                        cols: old_width,
                        rows: 40,
                    },
                    200,
                );
                parser.parse(&text, &mut screen);
                screen.resize(TerminalSize {
                    cols: new_width,
                    rows: 40,
                });
                parser.parse(b"X", &mut screen);

                let expected = format!("{}X", String::from_utf8_lossy(&text));
                let captured = joined_screen_text(&screen);
                assert!(
                    captured.contains(&expected),
                    "old_width={old_width} new_width={new_width} length={length} \
                     cursor={:?} expected={expected:?} capture={captured:?}",
                    screen.cursor_position()
                );
            }
        }
    }
}

#[test]
fn fuzz_resize_reflow_preserves_wide_gap_end_cursor_matrix() {
    for old_width in 8..=12_u16 {
        for new_width in 2..=7_u16 {
            if old_width == new_width {
                continue;
            }
            let text = "abc表d";
            let mut parser = InputParser::new();
            let mut screen = Screen::new(
                TerminalSize {
                    cols: old_width,
                    rows: 12,
                },
                100,
            );
            parser.parse(text.as_bytes(), &mut screen);
            screen.resize(TerminalSize {
                cols: new_width,
                rows: 12,
            });
            parser.parse(b"X", &mut screen);

            let captured = joined_screen_text(&screen);
            assert!(
                captured.contains("abc表dX"),
                "old_width={old_width} new_width={new_width} cursor={:?} capture={captured:?}",
                screen.cursor_position()
            );
        }
    }
}

#[test]
fn fuzz_resize_reflow_wide_placement_gap_cycles_match_tmux_text() {
    let prefixes = ["", "a", "ab", "abc", "abcd", "abcde"];
    let middles = ["", "x", "xy", "xyz"];
    let suffixes = ["d", "def", "defgh"];

    for prefix in prefixes {
        for middle in middles {
            for suffix in suffixes {
                let text = format!("{prefix}表{middle}界{suffix}");
                for narrow_width in 2..=7_u16 {
                    for intermediate_width in 3..=11_u16 {
                        let mut parser = InputParser::new();
                        let mut screen = Screen::new(TerminalSize { cols: 20, rows: 40 }, 200);
                        parser.parse(text.as_bytes(), &mut screen);
                        screen.resize(TerminalSize {
                            cols: narrow_width,
                            rows: 40,
                        });
                        screen.resize(TerminalSize {
                            cols: intermediate_width,
                            rows: 40,
                        });
                        screen.resize(TerminalSize { cols: 20, rows: 40 });
                        parser.parse(b"X", &mut screen);

                        let expected = format!("{text}X");
                        let captured = joined_screen_text(&screen);
                        assert!(
                            captured.contains(&expected),
                            "narrow={narrow_width} intermediate={intermediate_width} \
                             cursor={:?} expected={expected:?} capture={captured:?}",
                            screen.cursor_position()
                        );
                    }
                }
            }
        }
    }
}

// ─── #{=N} truncation: ZWJ / skin tone / emoji boundaries ───────────

fn truncate_check(input: &str, width: usize) {
    let cfg = Utf8Config::default();
    let truncated = truncate_to_width(input, width, &cfg);
    let actual_width = text_width(&truncated, &cfg);
    assert!(
        actual_width <= width,
        "truncate_to_width({input:?}, {width}) -> {truncated:?} (width {actual_width} > {width})"
    );
    assert!(
        truncated.chars().count() <= input.chars().count(),
        "truncate produced more characters than input!"
    );
    // Every char in `truncated` must appear at least once in `input` — we
    // intentionally do not require strict substring because the truncator may
    // drop dangling combining/ZWJ codepoints that don't fold into a grapheme.
    for ch in truncated.chars() {
        assert!(
            input.contains(ch),
            "truncated produced char {ch:?} absent from input {input:?}"
        );
    }
}

#[test]
fn truncate_emoji_zwj_skin_tone_does_not_split_cluster() {
    // Family + skin tone + ZWJ between chars in the middle of a #{=N} window.
    let cases: &[(&str, usize)] = &[
        ("👨\u{200D}👩\u{200D}👧\u{200D}👦ABC", 2), // family is one cluster width 2
        ("👨\u{200D}👩\u{200D}👧\u{200D}👦ABC", 3),
        ("👨\u{200D}👩\u{200D}👧\u{200D}👦ABC", 4),
        ("👋\u{1F3FF}A", 1),
        ("👋\u{1F3FF}A", 2),
        ("👋\u{1F3FF}A", 3),
        ("🇨🇭🇫🇷abc", 2),
        ("🇨🇭🇫🇷abc", 3),
        ("🇨🇭🇫🇷abc", 4),
        ("A\u{0301}B\u{0301}C", 1),
        ("A\u{0301}B\u{0301}C", 2),
        ("ᄒ\u{1175}ᆫ", 2),
        ("표abc", 2),
        ("표abc", 3),
        ("\u{200D}\u{200D}A", 1),
        ("A\u{200D}", 1),
    ];
    for (input, width) in cases {
        truncate_check(input, *width);
    }
}

#[test]
fn truncate_fuzz_random_emoji_buffer() {
    let snippets: &[&str] = &[
        "👨\u{200D}👩\u{200D}👧",
        "🏃\u{200D}♂\u{FE0F}",
        "👋\u{1F3FF}",
        "🇨🇭",
        "한",
        "ᄒ\u{1175}ᆫ",
        "표",
        "A\u{0301}",
        "\u{200D}",
        "A",
        "B",
        "C",
        " ",
    ];
    let mut rng = XorShift64::new(0xfeed_face_dead_beef);
    for _ in 0..10_000 {
        let n = rng.range(0, 24);
        let mut s = String::new();
        for _ in 0..n {
            s.push_str(snippets[(rng.next_u32() as usize) % snippets.len()]);
        }
        for w in 0..16 {
            truncate_check(&s, w);
        }
    }
}
