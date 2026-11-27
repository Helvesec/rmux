use super::*;

/// Ranks the candidates, each of which already lost the status rows of its own
/// session when it was built.
pub(super) fn selected_client_size(
    policy: AttachedWindowSizePolicy,
    mut attached_candidates: Vec<AttachedSizeCandidate>,
    control_candidates: &[ControlSizeCandidate],
    manual_size: rmux_core::ManualWindowSize,
) -> Option<SelectedWindowSize> {
    attached_candidates.extend(
        control_candidates
            .iter()
            .map(ControlSizeCandidate::size_candidate),
    );
    let constrained_manual = policy == AttachedWindowSizePolicy::Manual;
    if constrained_manual
        && !manual_size.is_constrained()
        && control_candidates
            .iter()
            .all(|candidate| candidate.window_limit.is_none())
    {
        return None;
    }
    let mut selected = if constrained_manual {
        SelectedContentSize {
            size: manual_size.size(),
            row_basis: ClientSizeBasis::Content,
            stored_rows: manual_size.size().rows,
        }
    } else {
        selected_attached_size(policy, &attached_candidates)?
    };
    for limit in control_candidates
        .iter()
        .filter_map(|candidate| candidate.window_limit)
    {
        selected.size.cols = selected.size.cols.min(limit.cols);
        if selected.size.rows > limit.rows {
            selected.size.rows = limit.rows;
            selected.row_basis = ClientSizeBasis::Content;
            selected.stored_rows = limit.rows;
        }
    }
    let terminal_size = TerminalSize {
        cols: selected.size.cols,
        rows: selected.stored_rows,
    };
    Some(SelectedWindowSize {
        constrained_manual,
        terminal_size,
        content_size: selected.size,
    })
}

pub(super) fn policy_from_option_value(value: Option<&str>) -> AttachedWindowSizePolicy {
    match value {
        Some("largest") => AttachedWindowSizePolicy::Largest,
        Some("smallest") => AttachedWindowSizePolicy::Smallest,
        Some("manual") => AttachedWindowSizePolicy::Manual,
        Some("latest") | None => AttachedWindowSizePolicy::Latest,
        Some(_) => AttachedWindowSizePolicy::Latest,
    }
}

fn selected_attached_size(
    policy: AttachedWindowSizePolicy,
    candidates: &[AttachedSizeCandidate],
) -> Option<SelectedContentSize> {
    match policy {
        AttachedWindowSizePolicy::Manual => None,
        AttachedWindowSizePolicy::Latest => candidates
            .iter()
            .max_by_key(|candidate| candidate.sequence)
            .map(|candidate| SelectedContentSize {
                size: candidate.size,
                row_basis: candidate.basis,
                stored_rows: candidate.stored_rows,
            }),
        AttachedWindowSizePolicy::Largest => selected_extreme_attached_size(candidates, u16::max),
        AttachedWindowSizePolicy::Smallest => selected_extreme_attached_size(candidates, u16::min),
    }
}

fn selected_extreme_attached_size(
    candidates: &[AttachedSizeCandidate],
    select: impl Fn(u16, u16) -> u16,
) -> Option<SelectedContentSize> {
    let first = candidates.first()?;
    let size = candidates
        .iter()
        .skip(1)
        .fold(first.size, |selected, candidate| TerminalSize {
            cols: select(selected.cols, candidate.size.cols),
            rows: select(selected.rows, candidate.size.rows),
        });
    let row_basis = if candidates.iter().any(|candidate| {
        candidate.size.rows == size.rows && candidate.basis == ClientSizeBasis::Content
    }) {
        ClientSizeBasis::Content
    } else {
        ClientSizeBasis::Terminal
    };
    let stored_rows = candidates
        .iter()
        .filter(|candidate| candidate.size.rows == size.rows && candidate.basis == row_basis)
        .map(|candidate| candidate.stored_rows)
        .reduce(select)
        .expect("the selected row has at least one owning candidate");
    Some(SelectedContentSize {
        size,
        row_basis,
        stored_rows,
    })
}
