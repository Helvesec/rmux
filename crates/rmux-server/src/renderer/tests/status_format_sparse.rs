use super::{render, screen_with, session_name, visible_line_text};
use crate::status_ranges::{StatusRange, StatusRangeType};
use rmux_core::OptionStore;
use rmux_proto::{
    types::OptionScopeSelector, OptionName, ScopeSelector, SetOptionMode, TerminalSize,
};

fn check_sparse_status_format(entries: &[(u32, &str)], expected: [&str; 3], ranges: bool) {
    for position in ["top", "bottom"] {
        for global in [false, true] {
            let size = TerminalSize { cols: 80, rows: 24 };
            let session = crate::session_fixture::SessionFixture::new(session_name("sparse"), size);
            let mut options = OptionStore::new();
            for (name, value) in [
                (OptionName::Status, "3"),
                (OptionName::StatusPosition, position),
            ] {
                options
                    .set(
                        ScopeSelector::Global,
                        name,
                        value.to_owned(),
                        SetOptionMode::Replace,
                    )
                    .expect("status option set");
            }
            let scope = if global {
                OptionScopeSelector::SessionGlobal
            } else {
                OptionScopeSelector::Session(session.name().clone())
            };
            // Remove the inherited dense defaults explicitly, including index 1.
            for index in 0..3 {
                options
                    .set_by_name(
                        scope.clone(),
                        &format!("status-format[{index}]"),
                        None,
                        SetOptionMode::Replace,
                        false,
                        true,
                        false,
                    )
                    .expect("index unset");
            }
            for &(index, value) in entries {
                options
                    .set_by_name(
                        scope.clone(),
                        &format!("status-format[{index}]"),
                        Some(value.to_owned()),
                        SetOptionMode::Replace,
                        false,
                        false,
                        false,
                    )
                    .expect("indexed format set");
            }
            // Writes to an emptied global array may materialize defaults again.
            // Remove holes after the writes so the effective table is truly sparse.
            for index in 0..3 {
                if !entries.iter().any(|&(key, _)| key == index) {
                    options
                        .set_by_name(
                            scope.clone(),
                            &format!("status-format[{index}]"),
                            None,
                            SetOptionMode::Replace,
                            false,
                            true,
                            false,
                        )
                        .expect("hole explicitly absent after writes");
                }
            }
            let effective =
                options.resolve_array_entries(Some(session.name()), OptionName::StatusFormat);
            assert_eq!(
                effective,
                entries
                    .iter()
                    .map(|&(i, s)| (i, s.to_owned()))
                    .collect::<Vec<_>>()
            );
            let screen = screen_with(&render(session.view(), &options), size);
            let first = if position == "top" { 0 } else { 21 };
            for (index, text) in expected.iter().enumerate() {
                assert_eq!(
                    visible_line_text(&screen, first + index, 80),
                    format!("{text:<80}"),
                    "position={position}, global={global}, status line={index}"
                );
            }
            if ranges {
                let layout = crate::renderer::overlay::status_line_layout(
                    session.view(),
                    &options,
                    0,
                    None,
                    None,
                )
                .expect("status layout");
                let expected_ranges: Vec<_> = entries
                    .iter()
                    .map(|&(line, _)| StatusRange {
                        line: u16::try_from(line).unwrap(),
                        x: 0..=2,
                        kind: StatusRangeType::Control(if line == 0 { 7 } else { 9 }),
                    })
                    .collect();
                assert_eq!(
                    layout.ranges, expected_ranges,
                    "position={position}, global={global}"
                );
            }
        }
    }
}

#[test]
fn sparse_status_format_preserves_missing_middle_line_cells() {
    check_sparse_status_format(&[(0, "ZERO"), (2, "TWO")], ["ZERO", "", "TWO"], false);
}

#[test]
fn sparse_status_format_preserves_leading_holes_cells() {
    check_sparse_status_format(&[(2, "TWO")], ["", "", "TWO"], false);
}

#[test]
fn sparse_status_format_preserves_overlay_range_line_indices() {
    check_sparse_status_format(
        &[(0, "#[range=control|7]ZER"), (2, "#[range=control|9]TWO")],
        ["ZER", "", "TWO"],
        true,
    );
}

#[test]
fn sparse_status_format_keeps_dense_cells_unchanged() {
    check_sparse_status_format(
        &[(0, "ZERO"), (1, "ONE"), (2, "TWO")],
        ["ZERO", "ONE", "TWO"],
        false,
    );
}
