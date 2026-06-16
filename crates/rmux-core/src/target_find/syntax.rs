use super::{TargetFindFlags, TargetFindType};

#[derive(Debug, Clone, Copy)]
pub(super) struct TargetParts<'a> {
    pub(super) session: Option<&'a str>,
    pub(super) window: Option<&'a str>,
    pub(super) pane: Option<&'a str>,
    pub(super) window_only: bool,
    pub(super) pane_only: bool,
}

impl<'a> TargetParts<'a> {
    pub(super) fn parse(raw: &'a str, find_type: TargetFindType) -> Self {
        let mut session = None;
        let mut window = None;
        let mut pane = None;
        let mut window_only = false;
        let mut pane_only = false;

        if let Some((session_part, rest)) = raw.split_once(':') {
            session = Some(session_part);
            window_only = true;
            if let Some((window_part, pane_part)) = rest.split_once('.') {
                window = Some(window_part);
                pane = Some(pane_part);
                pane_only = true;
            } else {
                window = Some(rest);
            }
        } else if raw.starts_with('$') {
            session = Some(raw);
        } else if raw.starts_with('@') {
            if let Some((window_part, pane_part)) = raw.split_once('.') {
                window = Some(window_part);
                pane = Some(pane_part);
                pane_only = true;
            } else {
                window = Some(raw);
            }
        } else if raw.starts_with('%') {
            pane = Some(raw);
        } else {
            match find_type {
                TargetFindType::Session => session = Some(raw),
                TargetFindType::Window => window = Some(raw),
                TargetFindType::Pane => {
                    if let Some((window_part, pane_part)) = raw.split_once('.') {
                        window = Some(window_part);
                        pane = Some(pane_part);
                        pane_only = true;
                    } else {
                        pane = Some(raw);
                    }
                }
            }
        }

        Self {
            session,
            window,
            pane,
            window_only,
            pane_only,
        }
    }

    pub(super) fn apply_exact_prefixes(&mut self, flags: &mut TargetFindFlags) {
        if let Some(session) = self.session.and_then(|value| value.strip_prefix('=')) {
            self.session = Some(session);
            flags.insert(TargetFindFlags::EXACT_SESSION);
        }
        if let Some(window) = self.window.and_then(|value| value.strip_prefix('=')) {
            self.window = Some(window);
            flags.insert(TargetFindFlags::EXACT_WINDOW);
        }
    }

    pub(super) fn drop_empty(&mut self) {
        if self.session == Some("") {
            self.session = None;
        }
        if self.window == Some("") {
            self.window = None;
        }
        if self.pane == Some("") {
            self.pane = None;
        }
    }

    pub(super) fn map_tokens(&mut self) {
        self.window = self.window.map(map_window_token);
        self.pane = self.pane.map(map_pane_token);
    }
}

fn map_window_token(value: &str) -> &str {
    match value {
        "{start}" => "^",
        "{last}" => "!",
        "{end}" => "$",
        "{next}" => "+",
        "{previous}" => "-",
        _ => value,
    }
}

fn map_pane_token(value: &str) -> &str {
    match value {
        "{last}" => "!",
        "{next}" => "+",
        "{previous}" => "-",
        "{top}" => "top",
        "{bottom}" => "bottom",
        "{left}" => "left",
        "{right}" => "right",
        "{top-left}" => "top-left",
        "{top-right}" => "top-right",
        "{bottom-left}" => "bottom-left",
        "{bottom-right}" => "bottom-right",
        _ => value,
    }
}
