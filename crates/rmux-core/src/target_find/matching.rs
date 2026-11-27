use rmux_proto::{PaneTarget, RmuxError, SessionName, Target, WindowTarget};

use crate::{Pane, PaneId, Window};

use super::TargetFindType;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedParts {
    pub(super) session_name: SessionName,
    pub(super) window_index: u32,
    pub(super) pane_index: u32,
}

impl ResolvedParts {
    pub(super) fn window(session_name: SessionName, window_index: u32) -> Self {
        Self {
            session_name,
            window_index,
            pane_index: 0,
        }
    }

    pub(super) fn pane(session_name: SessionName, window_index: u32, pane_index: u32) -> Self {
        Self {
            session_name,
            window_index,
            pane_index,
        }
    }

    pub(super) fn to_window_string(&self) -> String {
        format!("{}:{}", self.session_name, self.window_index)
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum MatchMode {
    Exact,
    Prefix,
    Pattern,
}

pub(super) fn target_for_type(find_type: TargetFindType, parts: ResolvedParts) -> Target {
    match find_type {
        TargetFindType::Session => Target::Session(parts.session_name),
        TargetFindType::Window => Target::Window(WindowTarget::with_window(
            parts.session_name,
            parts.window_index,
        )),
        TargetFindType::Pane => Target::Pane(PaneTarget::with_window(
            parts.session_name,
            parts.window_index,
            parts.pane_index,
        )),
    }
}

pub(super) fn target_not_found(value: &str, kind: &str) -> RmuxError {
    RmuxError::invalid_target(value, format!("can't find {kind}: {value}"))
}

pub(super) fn unique_match<T>(
    matches: Vec<T>,
    value: &str,
    kind: &str,
) -> Result<Option<T>, RmuxError> {
    let count = matches.len();
    if count > 1 {
        return Err(RmuxError::invalid_target(
            value,
            format!("ambiguous {kind} match"),
        ));
    }
    Ok(matches.into_iter().next())
}

pub(super) fn parse_prefixed_id(value: &str, prefix: char) -> Result<Option<u32>, RmuxError> {
    if !value.starts_with(prefix) {
        return Ok(None);
    }
    parse_required_prefixed_id(value, prefix).map(Some)
}

pub(super) fn parse_required_prefixed_id(value: &str, prefix: char) -> Result<u32, RmuxError> {
    let Some(rest) = value.strip_prefix(prefix) else {
        return Err(RmuxError::invalid_target(
            value,
            format!("{prefix} id target must start with {prefix}"),
        ));
    };
    if rest.is_empty() || !rest.chars().all(|character| character.is_ascii_digit()) {
        return Err(RmuxError::invalid_target(
            value,
            format!("{prefix} id target must be followed by an unsigned integer"),
        ));
    }
    rest.parse::<u32>().map_err(|_| {
        RmuxError::invalid_target(
            value,
            format!("{prefix} id target must fit in an unsigned integer"),
        )
    })
}

pub(super) fn parse_offset(value: &str) -> Result<Option<i32>, RmuxError> {
    let Some(sign) = value
        .chars()
        .next()
        .filter(|sign| matches!(sign, '+' | '-'))
    else {
        return Ok(None);
    };
    let rest = &value[sign.len_utf8()..];
    let magnitude = if rest.is_empty() {
        1
    } else if rest.chars().all(|character| character.is_ascii_digit()) {
        rest.parse::<i32>()
            .map_err(|_| RmuxError::invalid_target(value, "offset must fit in a signed integer"))?
    } else {
        return Ok(None);
    };
    Ok(Some(if sign == '+' { magnitude } else { -magnitude }))
}

/// tmux window indices are `int`, so an index offset that lands below 0 or
/// past `i32::MAX` names no window, as `new-window -t :+N` measures on tmux.
pub(super) fn offset_index(index: u32, offset: i32) -> Option<u32> {
    let target = i32::try_from(i64::from(index) + i64::from(offset)).ok()?;
    u32::try_from(target).ok()
}

pub(super) fn cyclic_window_offset(
    session: super::storage::TargetSession<'_>,
    offset: i32,
) -> Option<u32> {
    let indices = session
        .windows()
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    cyclic_offset(&indices, session.active_window_index(), offset)
}

pub(super) fn cyclic_pane_offset(window: &Window, offset: i32) -> Option<u32> {
    let indices = window.panes().iter().map(Pane::index).collect::<Vec<_>>();
    cyclic_offset(&indices, window.active_pane_index(), offset)
}

/// tmux steps `offset` times through the list, wrapping at either end, so the
/// sum is taken in `i64`: `position + offset` overflows `i32` once `position`
/// is at least 1 and `offset` is near `i32::MAX`.
fn cyclic_offset(indices: &[u32], current: u32, offset: i32) -> Option<u32> {
    let position = i64::try_from(indices.iter().position(|index| *index == current)?).ok()?;
    let len = i64::try_from(indices.len()).ok()?;
    let next = (position + i64::from(offset)).rem_euclid(len);
    indices.get(usize::try_from(next).ok()?).copied()
}

pub(super) fn special_window_index(
    session: super::storage::TargetSession<'_>,
    value: &str,
) -> Option<u32> {
    match value {
        "!" => session.last_window_index(),
        "^" => session.windows().next().map(|(index, _)| index),
        "$" => session.windows().next_back().map(|(index, _)| index),
        _ => None,
    }
}

pub(super) fn unique_window_name_match(
    session: super::storage::TargetSession<'_>,
    value: &str,
    mode: MatchMode,
) -> Result<Option<u32>, RmuxError> {
    let matches = session
        .windows()
        .filter_map(|(window_index, window)| {
            let name = window.name()?;
            let matched = match mode {
                MatchMode::Exact => name == value,
                MatchMode::Prefix => name.starts_with(value),
                MatchMode::Pattern => crate::fnmatch(value, name),
            };
            matched.then_some(window_index)
        })
        .collect::<Vec<_>>();
    unique_match(matches, value, "window")
}

/// Where a pane sits in one session: its active link when that link holds the
/// pane's window, and otherwise the first index that does.
///
/// tmux answers the same way once a session is chosen: the current winlink if it
/// contains the window, otherwise the first one found in index order
/// (`cmd_find_best_winlink_with_window`, `cmd-find.c:205-231`). A window linked
/// twice in one session therefore answers where that session is looking, not
/// wherever the link table happens to be walked from.
pub(super) fn find_pane_id_in_session(
    session: super::storage::TargetSession<'_>,
    pane_id: PaneId,
) -> Option<(u32, u32)> {
    let in_window = |window: &crate::Window| {
        window
            .panes()
            .iter()
            .find(|pane| pane.id() == pane_id)
            .map(crate::Pane::index)
    };
    let active = session.active_window_index();
    if let Some(pane_index) = session.window_at(active).and_then(in_window) {
        return Some((active, pane_index));
    }
    session
        .windows()
        .filter_map(|(window_index, window)| {
            in_window(window).map(|pane_index| (window_index, pane_index))
        })
        .min_by_key(|(window_index, _)| *window_index)
}
