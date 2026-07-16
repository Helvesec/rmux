//! Golden parser-trace fixtures.
//!
//! These tests freeze the current behavior of [`rmux_core::input::InputParser`]
//! plus [`rmux_core::Screen`] as deterministic textual traces under
//! `tests/parser_traces/<name>.trace`. They exist so that any future parser
//! migration (in particular a private `vt100 0.16` adapter behind the
//! crate-private `TerminalParser` boundary) must reproduce the same screen
//! semantics before it can replace the existing parser.
//!
//! Inputs are encoded inline in this file rather than as raw byte fixtures
//! because Rust string literals encode the relevant escape sequences clearly
//! and reviewably. The golden files are the persisted artifact in
//! `tests/parser_traces/`. To regenerate (after intentional behavior
//! changes), set `RMUX_REGEN_PARSER_TRACES=1` and re-run this test.
//!
//! Coverage targets:
//! - ASCII wrapping, reflow, and previous-line wrap break behavior
//! - SGR colours and attributes (palette, 256-colour, RGB, reset)
//! - Wide CJK glyph padding and combining marks (e + U+0301, emoji + VS16)
//! - Wide-glyph wrap-at-last-column and narrow-overwrites-wide collapse
//! - OSC 0/2 title, OSC 7 path, and OSC 8 hyperlink behavior
//! - Scrollback overflow and history limit truncation
//! - Cursor motion (CUP/CUF/CUB), DECSC/DECRC save/restore, scroll regions
//! - Alternate-screen on/off behavior including saved-grid capture
//! - Capture-grid and transcript output, including SGR pass-through
//! - Edge cases: empty feed, incomplete CSI buffering, DA1 reply round-trip,
//!   DECALN alignment, DECTCEM cursor-visibility mode bits, and C0 control
//!   handling (HT/BS/CR)

use std::collections::BTreeSet;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use rmux_core::input::{
    Colour, GridAttr, InputParser, COLOUR_DEFAULT, COLOUR_FLAG_256, COLOUR_FLAG_RGB, COLOUR_NONE,
    COLOUR_TERMINAL,
};
use rmux_core::{
    GridRenderOptions, Screen, ScreenCaptureRange, ScreenCellView, ScreenLineView, Utf8Config,
};
use rmux_proto::TerminalSize;

/// One scenario, run end-to-end against a fresh parser+screen.
struct Fixture {
    /// Stable file-system slug.
    name: &'static str,
    /// Initial geometry.
    cols: u16,
    rows: u16,
    /// Scrollback limit. Use a small value to exercise rotation.
    history: usize,
    /// Optional resize applied after `feeds`. Useful for reflow fixtures.
    resize_after: Option<(u16, u16)>,
    /// Sequential byte feeds so that fixtures can describe multi-step input
    /// (for example, write some content, enter alternate screen, write more,
    /// exit alternate screen).
    feeds: &'static [&'static [u8]],
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "ascii_wrap",
        cols: 3,
        rows: 2,
        history: 10,
        resize_after: None,
        feeds: &[b"abcdef"],
    },
    Fixture {
        name: "ascii_wrap_reflow_widen",
        cols: 5,
        rows: 16,
        history: 10,
        // Reflow narrow→wide should re-join the wrapped logical line.
        resize_after: Some((1, 16)),
        feeds: &[b"PANE1-ABCDE"],
    },
    Fixture {
        name: "ascii_break_previous_wrap_then_widen",
        cols: 3,
        rows: 4,
        history: 10,
        resize_after: Some((6, 4)),
        feeds: &[b"abcdef\x1b[2;1HXYZ"],
    },
    Fixture {
        name: "ascii_reflow_cursor_history_anchor_product_divergence",
        cols: 10,
        rows: 4,
        history: 10,
        resize_after: Some((5, 4)),
        feeds: &[b"abcdef\x1b[1;3H"],
    },
    Fixture {
        name: "wide_reflow_cursor_gap",
        cols: 10,
        rows: 4,
        history: 10,
        resize_after: Some((4, 4)),
        feeds: &["abc表d".as_bytes()],
    },
    Fixture {
        name: "sgr_basic_fg_bg_attrs",
        cols: 30,
        rows: 2,
        history: 10,
        resize_after: None,
        feeds: &[b"\x1b[1;31mBOLD-RED\x1b[0m \x1b[44mblue-bg\x1b[0m"],
    },
    Fixture {
        name: "sgr_256_and_rgb",
        cols: 30,
        rows: 2,
        history: 10,
        resize_after: None,
        feeds: &[b"\x1b[38;5;208morange\x1b[0m \x1b[38;2;10;20;30mrgb\x1b[0m"],
    },
    Fixture {
        name: "sgr_underline_styles",
        cols: 20,
        rows: 1,
        history: 5,
        resize_after: None,
        feeds: &[b"\x1b[4;58;2;200;30;40munder\x1b[0m \x1b[4:3mcurly\x1b[0m"],
    },
    Fixture {
        name: "unicode_wide_cjk",
        cols: 10,
        rows: 2,
        history: 10,
        resize_after: None,
        // Three Japanese wide glyphs followed by ASCII; verify padding cells.
        feeds: &["日本語!".as_bytes()],
    },
    Fixture {
        name: "unicode_combining_acute",
        cols: 10,
        rows: 1,
        history: 5,
        resize_after: None,
        feeds: &["e\u{0301}f".as_bytes()],
    },
    Fixture {
        name: "unicode_emoji_vs16",
        cols: 10,
        rows: 1,
        history: 5,
        resize_after: None,
        // Heart + VS16 should render wide under default config.
        feeds: &["\u{2764}\u{FE0F}!".as_bytes()],
    },
    Fixture {
        name: "osc_title_window_path",
        cols: 12,
        rows: 1,
        history: 5,
        resize_after: None,
        feeds: &[
            b"\x1b]0;outer-title\x07hi",
            b"\x1b]2;just-window\x1b\\",
            b"\x1b]7;file:///tmp/x\x1b\\",
        ],
    },
    Fixture {
        name: "osc_hyperlink_run",
        cols: 12,
        rows: 1,
        history: 5,
        resize_after: None,
        feeds: &[b"\x1b]8;id=42;https://example.invalid/\x1b\\link\x1b]8;;\x1b\\."],
    },
    Fixture {
        name: "scrollback_overflow",
        cols: 4,
        rows: 2,
        history: 3,
        resize_after: None,
        // 6 logical lines, history holds 3; the oldest must be dropped.
        feeds: &[b"L1\nL2\nL3\nL4\nL5\nL6\n"],
    },
    Fixture {
        name: "cursor_motion_save_restore",
        cols: 10,
        rows: 4,
        history: 5,
        resize_after: None,
        // CUP, CUF, CUB, DECSC, DECRC.
        feeds: &[b"abc\x1b[3;5HXY\x1b7\x1b[1;1HQ\x1b8Z"],
    },
    Fixture {
        name: "scroll_region_linefeed",
        cols: 6,
        rows: 5,
        history: 4,
        resize_after: None,
        // Set a 2..=4 scroll region (rows 2-4, 1-based) and overflow it.
        feeds: &[b"\x1b[2;4r\x1b[2;1HA\nB\nC\nD\nE\n"],
    },
    Fixture {
        name: "alternate_screen_roundtrip",
        cols: 8,
        rows: 3,
        history: 5,
        resize_after: None,
        // Write primary, enter alt, write alt, exit alt; cursor is restored.
        feeds: &[
            b"primary-line-1\nprimary-line-2",
            b"\x1b[?1049h",
            b"ALT-A\nALT-B",
            b"\x1b[?1049l",
        ],
    },
    Fixture {
        name: "alternate_screen_active_capture",
        cols: 8,
        rows: 3,
        history: 5,
        resize_after: None,
        feeds: &[b"primary", b"\x1b[?1049h", b"alt-content-only"],
    },
    Fixture {
        name: "capture_with_sequences_sgr",
        cols: 12,
        rows: 1,
        history: 4,
        resize_after: None,
        feeds: &[b"\x1b[1mA\x1b[31mB\x1b[0mC"],
    },
    Fixture {
        name: "bell_count",
        cols: 6,
        rows: 1,
        history: 4,
        resize_after: None,
        feeds: &[b"\x07hi\x07"],
    },
    Fixture {
        name: "decset_origin_mode",
        cols: 10,
        rows: 4,
        history: 4,
        resize_after: None,
        // Set scroll region 2..=3, enable origin mode, CUP origin-relative.
        feeds: &[b"\x1b[2;3r\x1b[?6h\x1b[1;1HX\x1b[2;1HY"],
    },
    // --- Edge case fixtures (hardening) ----------------------------------
    Fixture {
        name: "empty_feed",
        cols: 4,
        rows: 2,
        history: 2,
        resize_after: None,
        // Zero-byte feed: parser must stay in Ground with no replies and the
        // screen must remain entirely blank.
        feeds: &[b""],
    },
    Fixture {
        name: "incomplete_csi_pending",
        cols: 8,
        rows: 1,
        history: 2,
        resize_after: None,
        // Partial CSI: parser must buffer the bytes and report a non-Ground
        // state with non-empty `pending_bytes`. The screen should not have
        // received any printable output yet.
        feeds: &[b"\x1b[1;31"],
    },
    Fixture {
        name: "incomplete_osc_pending",
        cols: 8,
        rows: 1,
        history: 2,
        resize_after: None,
        // Partial OSC: parser must remain in `OscString` with the bytes
        // pending. This complements `incomplete_csi_pending` since the OSC
        // string state has its own buffering path distinct from the CSI
        // parameter buffer.
        feeds: &[b"\x1b]0;hello"],
    },
    Fixture {
        name: "device_attributes_reply",
        cols: 4,
        rows: 1,
        history: 2,
        resize_after: None,
        // CSI c (DA1) must produce a reply that round-trips through
        // `take_replies`. We only feed the request so the trace captures the
        // full response without any subsequent visible characters.
        feeds: &[b"\x1b[c"],
    },
    Fixture {
        name: "decaln_alignment_e",
        cols: 4,
        rows: 3,
        history: 2,
        resize_after: None,
        // DECALN (`ESC # 8`) fills the entire visible region with capital E.
        feeds: &[b"\x1b#8"],
    },
    Fixture {
        name: "wide_at_last_column",
        cols: 3,
        rows: 2,
        history: 2,
        resize_after: None,
        // After printing one ASCII char the cursor sits at column 1 of a
        // 3-column screen. A 2-wide CJK glyph then a third ASCII char must
        // either wrap or land somewhere deterministic — the trace freezes
        // whatever the current `Screen` implementation decides.
        feeds: &["a日X".as_bytes()],
    },
    Fixture {
        name: "wide_overwritten_by_narrow",
        cols: 4,
        rows: 1,
        history: 2,
        resize_after: None,
        // Place a wide glyph at column 0 (occupies 0 + padding at 1), then
        // move back to column 0 and overwrite with a narrow ASCII char. The
        // padding cell at column 1 must be cleared rather than left dangling.
        feeds: &["日\x1b[1;1HX".as_bytes()],
    },
    Fixture {
        name: "c0_controls_tab_bs_cr",
        cols: 12,
        rows: 1,
        history: 2,
        resize_after: None,
        // HT to next tab stop, then BS, then CR back to column 0, then more
        // text. Verifies C0 handling does not corrupt cells.
        feeds: &[b"a\tb\x08c\rZZ"],
    },
    Fixture {
        name: "cursor_visibility_dectcem",
        cols: 6,
        rows: 1,
        history: 2,
        resize_after: None,
        // DECTCEM hide: end on `hide` so the resulting `mode_bits` clears
        // the cursor-visible bit. Compared against the default `0x00000011`
        // mode mask seen in other fixtures, this trace freezes how the
        // current screen represents a hidden cursor.
        feeds: &[b"\x1b[?25l"],
    },
    Fixture {
        name: "erase_in_display_all",
        cols: 6,
        rows: 3,
        history: 2,
        resize_after: None,
        // Fill three rows then ED 2 (erase entire display); only cursor
        // position should remain.
        feeds: &[b"AAA\nBBB\nCCC\x1b[2J"],
    },
    Fixture {
        name: "erase_in_line_to_end",
        cols: 6,
        rows: 1,
        history: 2,
        resize_after: None,
        // Fill row, move cursor mid-row, then EL 0 erases from cursor to end.
        feeds: &[b"ABCDEF\x1b[1;3HX\x1b[K"],
    },
];

#[test]
fn parser_traces_match_goldens() {
    let traces_dir = manifest_dir().join("tests").join("parser_traces");
    fs::create_dir_all(&traces_dir).expect("ensure parser_traces directory exists");

    let regen = env::var("RMUX_REGEN_PARSER_TRACES").is_ok_and(|value| value != "0");
    let mut mismatches: Vec<String> = Vec::new();
    let mut seen: BTreeSet<&'static str> = BTreeSet::new();

    for fixture in FIXTURES {
        assert!(
            seen.insert(fixture.name),
            "duplicate fixture slug: {}",
            fixture.name
        );
        assert!(
            is_valid_slug(fixture.name),
            "invalid fixture slug `{}`: must be lowercase ascii letters, \
             digits, or underscores",
            fixture.name
        );
        assert!(
            fixture.cols >= 1 && fixture.rows >= 1,
            "fixture `{}` has degenerate geometry {}x{}",
            fixture.name,
            fixture.cols,
            fixture.rows
        );

        let actual = render_trace(fixture);
        // Determinism guard: rendering the same fixture twice must produce
        // byte-identical output. This catches subtle non-determinism in the
        // parser, screen, or formatting helpers (e.g. iteration over hash
        // sets) that would otherwise only surface as flaky golden diffs.
        let actual_again = render_trace(fixture);
        assert_eq!(
            actual, actual_again,
            "fixture `{}` is not deterministic across runs",
            fixture.name
        );
        let golden_path = traces_dir.join(format!("{}.trace", fixture.name));

        if regen {
            fs::write(&golden_path, &actual).unwrap_or_else(|err| {
                panic!("write golden {}: {err}", golden_path.display());
            });
            continue;
        }

        let expected = fs::read_to_string(&golden_path).unwrap_or_else(|err| {
            panic!(
                "missing golden {}: {err}\nrun with RMUX_REGEN_PARSER_TRACES=1 to create",
                golden_path.display()
            )
        });

        if expected != actual {
            mismatches.push(format!(
                "fixture `{}` diverged\n--- expected ({}) ---\n{}\n--- actual ---\n{}\n",
                fixture.name,
                golden_path.display(),
                expected,
                actual
            ));
        }
    }

    if !mismatches.is_empty() {
        let body = mismatches.join("\n");
        panic!(
            "{} parser-trace fixture(s) diverged from golden traces.\n\
             Re-run with RMUX_REGEN_PARSER_TRACES=1 if the change is intentional.\n\n{}",
            mismatches.len(),
            body
        );
    }
}

#[test]
fn no_orphan_golden_files() {
    let traces_dir = manifest_dir().join("tests").join("parser_traces");
    if !traces_dir.exists() {
        return;
    }
    let known: BTreeSet<&'static str> = FIXTURES.iter().map(|fixture| fixture.name).collect();
    let mut orphans = Vec::new();
    for entry in fs::read_dir(&traces_dir).expect("read parser_traces directory") {
        let entry = entry.expect("read parser_traces entry");
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("trace") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_owned();
        if !known.contains(stem.as_str()) {
            orphans.push(stem);
        }
    }
    assert!(
        orphans.is_empty(),
        "orphan golden trace files (no matching fixture): {orphans:?}"
    );
}

#[test]
fn golden_trace_files_have_no_trailing_whitespace() {
    let traces_dir = manifest_dir().join("tests").join("parser_traces");
    if !traces_dir.exists() {
        return;
    }

    let mut offenders = Vec::new();
    for entry in fs::read_dir(&traces_dir).expect("read parser_traces directory") {
        let entry = entry.expect("read parser_traces entry");
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("trace") {
            continue;
        }
        let contents = fs::read_to_string(&path).unwrap_or_else(|err| {
            panic!("read golden trace {}: {err}", path.display());
        });
        for (line_idx, line) in contents.lines().enumerate() {
            if line.ends_with(' ') || line.ends_with('\t') {
                offenders.push(format!("{}:{}", path.display(), line_idx + 1));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "golden trace files contain trailing whitespace: {offenders:?}"
    );
}

#[test]
fn terminal_parser_boundary_stays_private() {
    let lib_rs = fs::read_to_string(manifest_dir().join("src").join("lib.rs"))
        .expect("read rmux-core lib.rs");
    assert!(
        lib_rs.lines().any(|line| line.trim() == "mod terminal;"),
        "terminal parser module must remain a private crate-root module"
    );
    for forbidden in [
        "pub mod terminal;",
        "pub use crate::terminal::",
        "pub use self::terminal::",
        "pub use terminal::",
        "TerminalParser",
        "vt100",
    ] {
        assert!(
            !lib_rs.contains(forbidden),
            "rmux-core crate root must not expose terminal parser internals: {forbidden}"
        );
    }

    let terminal_rs =
        fs::read_to_string(manifest_dir().join("src").join("terminal").join("mod.rs"))
            .expect("read terminal module");
    assert!(
        terminal_rs.contains("pub(crate) struct TerminalParser"),
        "TerminalParser should be visible only inside rmux-core"
    );
    assert!(
        !terminal_rs.contains("pub struct TerminalParser"),
        "TerminalParser must not become a public type"
    );
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn is_valid_slug(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_'))
}

fn render_trace(fixture: &Fixture) -> String {
    let mut screen = Screen::new(
        TerminalSize {
            cols: fixture.cols,
            rows: fixture.rows,
        },
        fixture.history,
    );
    screen.set_utf8_config(Utf8Config::default());

    let mut parser = InputParser::new();
    for feed in fixture.feeds {
        parser.parse(feed, &mut screen);
    }

    if let Some((cols, rows)) = fixture.resize_after {
        screen.resize(TerminalSize { cols, rows });
    }

    let combined: Vec<u8> = fixture
        .feeds
        .iter()
        .flat_map(|chunk| chunk.iter().copied())
        .collect();

    write_trace(fixture, &combined, &mut parser, &screen)
}

fn write_trace(
    fixture: &Fixture,
    input: &[u8],
    parser: &mut InputParser,
    screen: &Screen,
) -> String {
    let mut out = String::new();
    writeln!(out, "# rmux-core parser trace").unwrap();
    writeln!(
        out,
        "# regenerate with: RMUX_REGEN_PARSER_TRACES=1 cargo test \
         --package rmux-core --test parser_traces -- --nocapture"
    )
    .unwrap();
    writeln!(out, "fixture: {}", fixture.name).unwrap();
    writeln!(out, "feeds: {}", fixture.feeds.len()).unwrap();
    writeln!(
        out,
        "size: cols={} rows={} history={}",
        fixture.cols, fixture.rows, fixture.history
    )
    .unwrap();
    if let Some((cols, rows)) = fixture.resize_after {
        writeln!(out, "resize_after: cols={cols} rows={rows}").unwrap();
    } else {
        writeln!(out, "resize_after: (none)").unwrap();
    }

    writeln!(out, "input_length: {}", input.len()).unwrap();
    writeln!(out, "input_hex: {}", hex_field(input)).unwrap();

    writeln!(out, "parser_state: {:?}", parser.state()).unwrap();
    let pending_bytes = parser.pending_bytes();
    writeln!(out, "parser_pending_hex: {}", hex_field(&pending_bytes)).unwrap();
    let replies = parser.take_replies();
    writeln!(out, "parser_replies_hex: {}", hex_field(&replies)).unwrap();
    writeln!(
        out,
        "parser_ground_timer_active: {}",
        parser.ground_timer_active()
    )
    .unwrap();

    writeln!(out, "mode_bits: {:#010x}", screen.mode()).unwrap();
    writeln!(out, "cursor_style: {}", screen.cursor_style()).unwrap();
    let (cx, cy) = screen.cursor_position();
    writeln!(out, "cursor: x={cx} y={cy}").unwrap();
    writeln!(out, "cursor_absolute_y: {}", screen.cursor_absolute_y()).unwrap();
    writeln!(out, "alternate: {}", screen.is_alternate()).unwrap();
    writeln!(out, "history_size: {}", screen.history_size()).unwrap();
    writeln!(out, "history_bytes: {}", screen.history_bytes()).unwrap();
    let mut bell_screen = (*screen).clone();
    let bell_count = bell_screen.take_bell_count();
    writeln!(out, "bell_count: {bell_count}").unwrap();
    writeln!(out, "title: {:?}", screen.title()).unwrap();
    writeln!(out, "path: {:?}", screen.path()).unwrap();
    writeln!(
        out,
        "size_runtime: cols={} rows={}",
        screen.size().cols,
        screen.size().rows
    )
    .unwrap();
    writeln!(out, "absolute_line_count: {}", screen.absolute_line_count()).unwrap();

    writeln!(out, "--- visible_lines ---").unwrap();
    let total = screen.absolute_line_count();
    let history = screen.history_size();
    for visible_y in 0..u32::from(fixture.rows) {
        let absolute_y = history + usize::try_from(visible_y).unwrap_or(0);
        if absolute_y >= total {
            writeln!(out, "[v={visible_y}] (missing absolute line)").unwrap();
            continue;
        }
        let Some(line) = screen.absolute_line_view(absolute_y) else {
            writeln!(out, "[v={visible_y}] (no line view)").unwrap();
            continue;
        };
        write_line_view(&mut out, "v", visible_y as usize, &line);
    }

    writeln!(out, "--- history_lines ---").unwrap();
    if history == 0 {
        writeln!(out, "(empty)").unwrap();
    } else {
        for absolute_y in 0..history {
            let Some(line) = screen.absolute_line_view(absolute_y) else {
                writeln!(out, "[h={absolute_y}] (no line view)").unwrap();
                continue;
            };
            write_line_view(&mut out, "h", absolute_y, &line);
        }
    }

    writeln!(out, "--- transcript ---").unwrap();
    let transcript = screen.capture_transcript(full_range(), GridRenderOptions::default());
    write_transcript_block(&mut out, &transcript);

    writeln!(out, "--- transcript_joined ---").unwrap();
    let joined_options = GridRenderOptions {
        join_wrapped: true,
        ..GridRenderOptions::default()
    };
    let joined = screen.capture_transcript(full_range(), joined_options);
    write_transcript_block(&mut out, &joined);

    writeln!(out, "--- transcript_with_sequences ---").unwrap();
    let seq_options = GridRenderOptions {
        with_sequences: true,
        escape_sequences: true,
        ..GridRenderOptions::default()
    };
    let with_seq = screen.capture_transcript(full_range(), seq_options);
    write_transcript_block(&mut out, &with_seq);

    writeln!(out, "--- saved_transcript ---").unwrap();
    if let Some(saved) = screen.capture_saved_transcript(full_range(), GridRenderOptions::default())
    {
        write_transcript_block(&mut out, &saved);
    } else {
        writeln!(out, "(none)").unwrap();
    }

    out
}

fn write_line_view(out: &mut String, marker: &str, index: usize, line: &ScreenLineView) {
    let mut flags = String::new();
    if line.wrapped() {
        flags.push_str("WRAPPED");
    }
    if line.start_prompt() {
        if !flags.is_empty() {
            flags.push('|');
        }
        flags.push_str("START_PROMPT");
    }
    if line.start_output() {
        if !flags.is_empty() {
            flags.push('|');
        }
        flags.push_str("START_OUTPUT");
    }
    if flags.is_empty() {
        flags.push('-');
    }
    writeln!(
        out,
        "[{marker}={index}] flags={flags} cells={}",
        line.cells().len()
    )
    .unwrap();
    let last_significant = last_significant_cell(line);
    if last_significant == 0 && cell_is_blank(&line.cells()[0]) {
        writeln!(out, "  (blank)").unwrap();
        return;
    }
    for (col, cell) in line.cells().iter().enumerate().take(last_significant + 1) {
        write_cell(out, col, cell);
    }
}

fn last_significant_cell(line: &ScreenLineView) -> usize {
    line.cells()
        .iter()
        .enumerate()
        .rev()
        .find_map(|(idx, cell)| if cell_is_blank(cell) { None } else { Some(idx) })
        .unwrap_or(0)
}

fn cell_is_blank(cell: &ScreenCellView) -> bool {
    if cell.is_padding() {
        return false;
    }
    if cell.attr() != 0 {
        return false;
    }
    if cell.fg() != COLOUR_DEFAULT {
        return false;
    }
    if cell.bg() != COLOUR_DEFAULT {
        return false;
    }
    if cell.us() != COLOUR_DEFAULT {
        return false;
    }
    if cell.link() != 0 {
        return false;
    }
    matches!(cell.text(), "" | " ")
}

fn write_cell(out: &mut String, col: usize, cell: &ScreenCellView) {
    let padding_marker = if cell.is_padding() { "P" } else { "." };
    writeln!(
        out,
        "  c{col:>3} {padding_marker} text={text:?} w={w} attr={attr} fg={fg} bg={bg} us={us} link={link}",
        text = cell.text(),
        w = cell.width(),
        attr = format_attr(cell.attr()),
        fg = format_colour(cell.fg()),
        bg = format_colour(cell.bg()),
        us = format_colour(cell.us()),
        link = cell.link(),
    )
    .unwrap();
}

fn full_range() -> ScreenCaptureRange {
    ScreenCaptureRange {
        start_is_absolute: true,
        end_is_absolute: true,
        ..ScreenCaptureRange::default()
    }
}

fn write_transcript_block(out: &mut String, bytes: &[u8]) {
    if bytes.is_empty() {
        writeln!(out, "(empty)").unwrap();
        return;
    }
    let text = String::from_utf8_lossy(bytes);
    let trailing_newline = bytes.last() == Some(&b'\n');
    let body = if trailing_newline {
        &text[..text.len() - 1]
    } else {
        text.as_ref()
    };
    if body.is_empty() {
        write_transcript_line(out, "");
    } else {
        for line in body.split('\n') {
            write_transcript_line(out, line);
        }
    }
    if !trailing_newline {
        writeln!(out, "(no trailing newline)").unwrap();
    }
}

fn write_transcript_line(out: &mut String, line: &str) {
    if line.is_empty() {
        writeln!(out, "|").unwrap();
    } else {
        writeln!(out, "| {}", line).unwrap();
    }
}

fn hex_field(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        "(empty)".to_owned()
    } else {
        to_hex(bytes)
    }
}

fn to_hex(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(bytes.len() * 3);
    for (idx, byte) in bytes.iter().enumerate() {
        if idx > 0 {
            out.push(' ');
        }
        write!(out, "{:02x}", byte).unwrap();
    }
    out
}

fn format_colour(colour: Colour) -> String {
    if colour == COLOUR_NONE {
        return "none".to_owned();
    }
    if colour == COLOUR_DEFAULT {
        return "default".to_owned();
    }
    if colour == COLOUR_TERMINAL {
        return "terminal".to_owned();
    }
    if (0..=7).contains(&colour) {
        return format!("ansi={colour}");
    }
    if colour & COLOUR_FLAG_RGB != 0 {
        let v = colour & 0x00FF_FFFF;
        let r = (v >> 16) & 0xFF;
        let g = (v >> 8) & 0xFF;
        let b = v & 0xFF;
        return format!("rgb={r},{g},{b}");
    }
    if colour & COLOUR_FLAG_256 != 0 {
        let idx = colour & 0xFF;
        return format!("x256={idx}");
    }
    format!("raw={colour:#x}")
}

fn format_attr(attr: u16) -> String {
    if attr == 0 {
        return "0x0000".to_owned();
    }
    let mut parts = Vec::new();
    let mappings = [
        (GridAttr::BRIGHT, "BOLD"),
        (GridAttr::DIM, "DIM"),
        (GridAttr::UNDERSCORE, "UNDER"),
        (GridAttr::BLINK, "BLINK"),
        (GridAttr::REVERSE, "REVERSE"),
        (GridAttr::HIDDEN, "HIDDEN"),
        (GridAttr::ITALICS, "ITALIC"),
        (GridAttr::CHARSET, "CHARSET"),
        (GridAttr::STRIKETHROUGH, "STRIKE"),
        (GridAttr::UNDERSCORE_2, "UNDER2"),
        (GridAttr::UNDERSCORE_3, "UNDER3"),
        (GridAttr::UNDERSCORE_4, "UNDER4"),
        (GridAttr::UNDERSCORE_5, "UNDER5"),
        (GridAttr::OVERLINE, "OVERLINE"),
        (GridAttr::NOATTR, "NOATTR"),
    ];
    for (mask, label) in mappings {
        if attr & mask != 0 {
            parts.push(label);
        }
    }
    format!("{:#06x}({})", attr, parts.join("|"))
}
