use super::*;

impl CopyViewport {
    fn observe(&mut self, bytes: &[u8]) {
        self.feed(bytes).unwrap();
        settle(self).unwrap();
    }

    fn reset(&mut self) {
        self.oracle.reset();
        self.bytes.clear();
        self.parser = InputParser::new();
        self.screen = Screen::new(TerminalSize { cols: 80, rows: 24 }, 0);
    }
}

fn settle(viewport: &mut CopyViewport) -> io::Result<()> {
    let end = Instant::now() + Duration::from_secs(3);
    while !viewport.oracle.current(viewport.bytes.len()) {
        let remaining = end
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "test oracle deadline"))?;
        viewport.oracle.service(&viewport.bytes)?;
        if viewport.oracle.needs_service(viewport.bytes.len()) {
            continue;
        }
        if viewport.oracle.current(viewport.bytes.len()) {
            break;
        }
        poll(
            &mut viewport.oracle.poll_fds(),
            Some(&Timespec::try_from(remaining).unwrap()),
        )?;
    }
    Ok(())
}

pub(super) fn retained_r1() -> Vec<u8> {
    let hex = include_str!("fixtures/r1.hex").trim();
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

pub(super) fn pipe() -> (File, std::os::unix::net::UnixStream) {
    use std::os::{fd::OwnedFd, unix::net::UnixStream};
    let (reader, writer) = UnixStream::pair().unwrap();
    reader.set_nonblocking(true).unwrap();
    (File::from(OwnedFd::from(reader)), writer)
}

#[test]
fn r1_rejects_external_right_margin_x_after_consuming_every_queued_byte() {
    use std::io::Write;
    let baseline = retained_r1();
    assert_eq!(baseline.len(), 2398);
    let suffix = b"\x1b[2;80HX\x1b[K";
    let mut corrupted = baseline.clone();
    corrupted.extend_from_slice(suffix);
    let (mut left, mut wl) = pipe();
    let (mut right, mut wr) = pipe();
    wl.write_all(&corrupted).unwrap();
    wr.write_all(&baseline).unwrap();
    let mut pair = [CopyViewport::new().unwrap(), CopyViewport::new().unwrap()];
    // Settle startup before the short rejection window, without consuming PTYs.
    for v in &mut pair {
        settle(v).unwrap();
    }
    let error = wait_for_viewports(
        &mut left,
        &mut right,
        &mut pair,
        ViewportPhase::CopyTop,
        Duration::from_millis(100),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(pair[0].bytes, corrupted);
    assert_eq!(pair[1].bytes, baseline);
    assert_eq!(
        left.read(&mut [0]).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(
        right.read(&mut [0]).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(pair[0].rows()[1].ends_with(" X"));
    assert!(!pair[1].rows()[1].ends_with('X'));
    assert_ne!(comparable_rows(&pair[0]), comparable_rows(&pair[1]));
    // RMUX's own screen model now keeps X as well: tmux 3.7b leaves its
    // logical cursor at sx after the last column, so EL 0 finds no cell to
    // erase. Both models reject the corruption and agree on the edited row.
    let internal = |v: &CopyViewport| {
        (0..24)
            .map(|row| {
                let mut text = String::new();
                v.screen.visit_visible_line_cells(row, 80, |c| {
                    if !c.is_padding() {
                        text.push_str(c.text());
                    }
                });
                text.trim_end_matches(' ').to_owned()
            })
            .collect::<Vec<_>>()
    };
    for v in &pair {
        assert_eq!(internal(v)[1], v.rows()[1]);
    }
    assert_ne!(internal(&pair[0]), internal(&pair[1]));
    assert!(pair.iter().all(
        |v| v.parser.pending_bytes().is_empty() && ViewportPhase::CopyTop.matches(&internal(v))
    ));
}

#[test]
fn pinned_xterm_right_margin_erase_control_and_chunk_neighborhood() {
    let baseline = retained_r1();
    let mut viewport = CopyViewport::new().unwrap();
    viewport.observe(&baseline);
    let original = viewport.rows();
    for (suffix, row2) in [
        (
            b"\x1b[2;80HX\x1b[K".as_slice(),
            format!("{} X", original[1]),
        ),
        (b"\x1b[2;80HX\x1b[0K", format!("{} X", original[1])),
        (b"\x1b[2;80HX\x1b[1K", String::new()),
        (b"\x1b[2;80HX\x1b[2K", String::new()),
        (b"\x1b[2;79HX\x1b[K", format!("{}X", original[1])),
        (b"\x1b[2;80HX\r\x1b[K", String::new()),
        (b"\x1b[2;80HX\x08\x1b[K", original[1].clone()),
        (b"\x1b[2;80HX\x1b[0m\x1b[K", format!("{} X", original[1])),
    ] {
        for cut in 0..=suffix.len() {
            viewport.reset();
            viewport.observe(&baseline);
            viewport.observe(&suffix[..cut]);
            viewport.observe(&suffix[cut..]);
            assert_eq!(viewport.rows()[1], row2, "suffix {suffix:?}, cut {cut}");
            for row in [0, 2, 22, 23] {
                assert_eq!(viewport.rows()[row], original[row]);
            }
        }
    }
}

#[test]
fn comparison_preserves_all_rows_except_fixture_clocks() {
    let mut rmux = CopyViewport::new().unwrap();
    let mut tmux = CopyViewport::new().unwrap();
    for viewport in [&mut rmux, &mut tmux] {
        viewport.observe(&live());
        viewport.observe(&copy_top());
    }
    rmux.observe(b"\x1b[1;68H12:01");
    assert_eq!(comparable_rows(&rmux), comparable_rows(&tmux));
    rmux.observe(&paint(1, "wrong prelude"));
    assert!(rmux.ready(ViewportPhase::CopyTop));
    assert_ne!(comparable_rows(&rmux), comparable_rows(&tmux));
}

fn paint(row: usize, text: &str) -> Vec<u8> {
    format!("\x1b[{};1H\x1b[K{text}", row + 1).into_bytes()
}

fn live() -> Vec<u8> {
    let mut bytes = b"\x1b[?1049h\x1b[2J".to_vec();
    for row in 0..22 {
        bytes.extend(paint(row, &format!("P0-LINE-{:02}", row + 9)));
    }
    bytes.extend(paint(22, "tester@RMUXHOST:~$"));
    bytes.extend(paint(23, "[alpha] 0:bash*"));
    bytes
}

fn copy_top() -> Vec<u8> {
    let mut bytes = paint(0, "");
    bytes.extend(paint(1, "fixture command"));
    for row in 2..23 {
        bytes.extend(paint(row, &format!("P0-LINE-{:02}", row - 1)));
    }
    bytes.extend(paint(
        0,
        "                                                                   12:00 [10/10]",
    ));
    bytes
}

#[test]
fn stale_live_marker_is_not_a_copy_transition() {
    let bytes = live();
    // The old assertion accepts this complete, but stale, live viewport.
    assert!(crate::support::render_cells(&bytes, 80, 24)
        .join("\n")
        .contains("P0-LINE-12"));
    let mut viewport = CopyViewport::new().unwrap();
    viewport.observe(&bytes);
    assert!(viewport.ready(ViewportPhase::Live));
    assert!(!viewport.ready(ViewportPhase::CopyTop));
    viewport.observe(&[]); // No output is not a transition acknowledgement.
    assert!(!viewport.ready(ViewportPhase::CopyTop));
    viewport.observe(&copy_top());
    assert!(viewport.ready(ViewportPhase::CopyTop));
}

#[test]
fn partial_repaint_requires_the_unchanged_tail() {
    let bytes = copy_top();
    let cut = bytes
        .windows(b"P0-LINE-10".len())
        .position(|part| part == b"P0-LINE-10")
        .unwrap()
        + 4;
    let mut viewport = CopyViewport::new().unwrap();
    viewport.observe(&live());
    viewport.observe(&bytes[..cut]);
    assert!(!crate::support::render_cells(&bytes[..cut], 80, 24)
        .join("\n")
        .contains("P0-LINE-12"));
    assert!(!viewport.ready(ViewportPhase::CopyTop));
    viewport.observe(&[]);
    assert!(!viewport.ready(ViewportPhase::CopyTop));
    viewport.observe(&bytes[cut..]);
    assert!(viewport.ready(ViewportPhase::CopyTop));
    assert_eq!(viewport.rows()[13], "P0-LINE-12");
}

#[test]
fn marker_and_header_do_not_hide_an_incomplete_body() {
    let mut viewport = CopyViewport::new().unwrap();
    viewport.observe(&live());
    viewport.observe(&copy_top());
    viewport.observe(&paint(20, "P0-LINE-29"));
    assert!(viewport.rows()[13].contains("P0-LINE-12"));
    assert!(!viewport.ready(ViewportPhase::CopyTop));
}

#[test]
fn escape_requires_header_removal_and_retains_other_rows() {
    let mut viewport = CopyViewport::new().unwrap();
    viewport.observe(&live());
    viewport.observe(&paint(
        0,
        "P0-LINE-09                                                                [0/10]",
    ));
    assert!(viewport.ready(ViewportPhase::CopyBottom));
    assert!(!viewport.ready(ViewportPhase::Live));
    viewport.observe(b"\x1b[1;11H\x1b[");
    assert!(!viewport.ready(ViewportPhase::Live));
    viewport.observe(b"K");
    assert!(viewport.ready(ViewportPhase::Live));
    assert_eq!(viewport.rows()[21], "P0-LINE-30");
}

#[test]
fn every_chunk_split_preserves_csi_utf8_and_grid() {
    let mut bytes = live();
    bytes.extend(copy_top());
    bytes.extend(paint(1, "fixture café"));
    let mut complete = CopyViewport::new().unwrap();
    complete.observe(&bytes);
    assert!(complete.ready(ViewportPhase::CopyTop));
    let mut viewport = CopyViewport::new().unwrap();
    for cut in 0..=bytes.len() {
        viewport.reset();
        viewport.observe(&bytes[..cut]);
        viewport.observe(&bytes[cut..]);
        assert_eq!(viewport.rows(), complete.rows(), "cut {cut}");
        assert!(viewport.ready(ViewportPhase::CopyTop), "cut {cut}");
    }
    complete.observe(b"\x1b[2;1H\xc3");
    assert!(!complete.ready(ViewportPhase::CopyTop));
    complete.observe(b"\xa9");
    assert!(complete.ready(ViewportPhase::CopyTop));
    complete.observe(b"\x1b[");
    assert!(!complete.ready(ViewportPhase::CopyTop));
    complete.observe(b"0m");
    assert!(complete.ready(ViewportPhase::CopyTop));
}

#[test]
fn native_macos_bash_copy_top_accepts_complete_oracle_view() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/macos-bash-top.json")).unwrap();
    let rows: Vec<String> = serde_json::from_value(fixture["rows"].clone()).unwrap();
    assert_eq!(rows.len(), 24);
    assert_eq!(rows.iter().position(|row| row == "P0-LINE-01"), Some(5));
    assert!(ViewportPhase::CopyTop.matches(&rows));
}

// Variants of the measured 30-line + prompt fixture. The empty banner case
// still has the echoed command, under the copy header at row zero.
fn top_with_prelude(first: usize) -> Vec<String> {
    let mut rows = vec![String::new(); 24];
    for (row, text) in rows.iter_mut().enumerate().take(first).skip(1) {
        *text = format!("shell prelude {row}: café");
    }
    for (row, text) in rows.iter_mut().enumerate().take(23).skip(first) {
        *text = format!("P0-LINE-{:02}", row - first + 1);
    }
    rows[0] = format!("12:00 [{0}/{0}]", first + 8);
    rows[23] = "[alpha] 0:bash*".into();
    rows
}

#[test]
fn copy_top_prelude_height_tracks_fixture_history_with_visible_body() {
    for first in 1..23 {
        let rows = top_with_prelude(first);
        assert!(ViewportPhase::CopyTop.matches(&rows), "first {first}");
        assert!(!ViewportPhase::CopyBottom.matches(&rows));
        assert!(!ViewportPhase::Live.matches(&rows));
    }
    // No numbered body, or a first line overwritten by the header.
    for first in [0, 23, 24] {
        assert!(!ViewportPhase::CopyTop.matches(&top_with_prelude(first)));
    }
}

#[test]
fn copy_top_rejects_wrong_history_bounds_shape_and_every_corrupt_body_row() {
    for first in [1, 2, 3, 5, 10, 22] {
        let rows = top_with_prelude(first);
        for header in [
            "[0/0]".to_owned(),
            format!("[0/{}]", first + 8),
            format!("[{0}/{0}]", first + 7),
            format!("[{0}/{0}]", first + 9),
            format!("[{0}/{0}]", usize::MAX),
            "[13/no]".to_owned(),
            "[13/13] residue".to_owned(),
        ] {
            let mut bad = rows.clone();
            bad[0] = header;
            assert!(
                !ViewportPhase::CopyTop.matches(&bad),
                "first {first}: {bad:?}"
            );
        }
        for row in first..23 {
            for text in ["", "P0-LINE-", "P0-LINE-30", "P0-LINE-01 X"] {
                let mut bad = rows.clone();
                bad[row] = text.into();
                assert!(
                    !ViewportPhase::CopyTop.matches(&bad),
                    "first {first}, row {row}"
                );
            }
        }
        let mut bad = rows.clone();
        bad[23] = "wrong status".into();
        assert!(!ViewportPhase::CopyTop.matches(&bad));
        assert!(!ViewportPhase::CopyTop.matches(&rows[..23]));
        let mut bad = rows;
        bad.push(String::new());
        assert!(!ViewportPhase::CopyTop.matches(&bad));
    }
}

#[test]
fn native_banner_view_requires_xterm_ack_and_complete_input() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/macos-bash-top.json")).unwrap();
    let rows: Vec<String> = serde_json::from_value(fixture["rows"].clone()).unwrap();
    let mut viewport = CopyViewport::new().unwrap();
    let mut bytes = b"\x1b[?1049h\x1b[2J".to_vec();
    for (row, text) in rows.iter().enumerate() {
        bytes.extend(paint(row, text));
    }
    viewport.feed(&bytes).unwrap();
    assert!(!viewport.ready(ViewportPhase::CopyTop)); // No xterm ACK yet.
    settle(&mut viewport).unwrap();
    assert_eq!(viewport.rows(), rows);
    assert!(viewport.ready(ViewportPhase::CopyTop));
    for (partial, completion) in [
        (b"\x1b[".as_slice(), b"0m".as_slice()),
        (b"\x1b]0;title", b"\x07"),
        (b"\x1b[2;1H\xc3", b"\xa9"),
    ] {
        viewport.observe(partial);
        assert!(!viewport.ready(ViewportPhase::CopyTop));
        viewport.observe(completion);
        assert!(viewport.ready(ViewportPhase::CopyTop));
    }
    // Even a prelude-only change survives the full-row comparison.
    assert_ne!(viewport.rows(), rows);
}
