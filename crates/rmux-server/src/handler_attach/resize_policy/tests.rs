use super::*;

#[test]
fn terminal_candidate_keeps_its_raw_height_when_status_consumes_every_row() {
    let size = TerminalSize { cols: 80, rows: 1 };
    let selected = selected_client_size(
        AttachedWindowSizePolicy::Latest,
        vec![AttachedSizeCandidate {
            size,
            stored_rows: size.rows,
            sequence: 1,
            basis: WindowSizeBasis::Terminal,
        }],
        &[],
        Some("3"),
    );

    assert_eq!(
        selected,
        Some(SelectedWindowSize {
            stored_size: size,
            basis: WindowSizeBasis::Terminal,
        })
    );
}
