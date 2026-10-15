//! Bounded terminal-local OSC 7501 records (protocol revision 0.4).
//! https://www.superlogical.com/rex/docs/build/program-status
use base64::engine::{general_purpose::GeneralPurpose, DecodePaddingMode, GeneralPurposeConfig};
use base64::{alphabet, Engine as _};
use rmux_proto::{ProgramStatusRecord, ProgramStatusState};

const MAX_RECORDS: usize = 256;
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// An oldest-to-newest set of terminal records, independent of alternate screens.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgramStatus {
    records: Vec<ProgramStatusRecord>,
    observed: bool,
}

impl ProgramStatus {
    /// Stored records, oldest update first. Missing applications inherit dynamically.
    #[must_use]
    pub fn records(&self) -> &[ProgramStatusRecord] {
        &self.records
    }

    /// Whether this terminal has accepted a report since its last full reset.
    #[must_use]
    pub const fn observed(&self) -> bool {
        self.observed
    }

    /// Apply a report body atomically. Invalid reports leave every record untouched.
    /// `body` excludes `OSC 7501 ;` and its terminator; `bel` selects its wire size.
    pub fn report(&mut self, body: &str, bel: bool) {
        let Some(report) = parse(body, bel) else {
            return;
        };
        self.observed = true;
        match report {
            Report::Clear(id) => {
                if id.is_empty() {
                    self.records.clear();
                } else {
                    let prefix = format!("{id}/");
                    self.records
                        .retain(|record| record.id != id && !record.id.starts_with(&prefix));
                }
            }
            Report::Set(record) => {
                self.records.retain(|old| old.id != record.id);
                if self.records.len() == MAX_RECORDS {
                    self.records.remove(0);
                }
                self.records.push(record);
            }
        }
    }

    /// A shell prompt or attached-process exit expires active/idle records, not results.
    pub fn expire_active(&mut self) {
        self.records.retain(|record| record.state.is_result());
    }

    /// A full terminal reset or a new pane process removes all prior records.
    pub fn clear(&mut self) {
        self.records.clear();
        self.observed = false;
    }

    /// Effective application, inherited from the nearest present ancestor.
    #[must_use]
    pub fn effective_app<'a>(&'a self, record: &'a ProgramStatusRecord) -> Option<&'a str> {
        if let Some(app) = record.app.as_deref() {
            return Some(app);
        }
        let mut id = record.id.as_str();
        while !id.is_empty() {
            id = id.rsplit_once('/').map_or("", |(parent, _)| parent);
            if let Some(app) = self
                .records
                .iter()
                .find(|candidate| candidate.id == id)
                .and_then(|parent| parent.app.as_deref())
            {
                return Some(app);
            }
        }
        None
    }

    /// Presentation summary: blocked > working > error > done > idle; newest wins ties.
    /// The returned clone includes the effective (inherited) application.
    #[must_use]
    pub fn summary(&self) -> Option<ProgramStatusRecord> {
        let record = self
            .records
            .iter()
            .max_by_key(|record| match record.state {
                ProgramStatusState::Blocked => 5,
                ProgramStatusState::Working => 4,
                ProgramStatusState::Error => 3,
                ProgramStatusState::Done => 2,
                ProgramStatusState::Idle => 1,
            })?;
        let mut result = record.clone();
        result.app = self.effective_app(record).map(str::to_owned);
        Some(result)
    }
}

enum Report {
    Clear(String),
    Set(ProgramStatusRecord),
}

fn segment(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
}

fn text(value: &str, encoded_limit: usize, decoded_limit: usize) -> Option<String> {
    if value.len() > encoded_limit {
        return None;
    }
    let bytes = BASE64.decode(value).ok()?;
    if bytes.len() > decoded_limit {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    (!text.chars().any(char::is_control)).then_some(text)
}

fn parse(body: &str, bel: bool) -> Option<Report> {
    // ESC ] + 7501 ; + body + BEL / ESC \\.
    if body.len() + 7 + if bel { 1 } else { 2 } > 4096 {
        return None;
    }
    let mut state = None;
    let mut id = None;
    let mut app = None;
    let mut kind = None;
    let mut progress = None;
    let mut title = None;
    let mut message = None;
    for pair in body.split(':') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if key.len() > 16 {
            return None;
        }
        // Limits apply even to pairs whose value would otherwise be skipped.
        // An invalid ID must never degrade into an update to the root record.
        if matches!(key, "app") && value.len() > 32
            || key == "msg" && value.len() > 2732
            || key == "title" && value.len() > 256
            || key == "id"
                && (value.len() > 128
                    || value.split('/').count() > 8
                    || !value.split('/').all(segment))
        {
            return None;
        }
        if key.is_empty()
            || !key.bytes().all(|b| b.is_ascii_lowercase())
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.,+/=-".contains(&b))
        {
            continue;
        }
        // Check all occurrences, including overwritten keys, before mutating storage.
        match key {
            "state" => state = Some(value),
            "id" => {
                if value.len() > 128
                    || value.split('/').count() > 8
                    || !value.split('/').all(segment)
                {
                    return None;
                }
                id = Some(value);
            }
            "app" => {
                if value.len() > 32 {
                    return None;
                }
                app = segment(value).then(|| value.to_owned());
            }
            "kind" => {
                kind = matches!(value, "permission" | "question" | "auth").then(|| value.to_owned())
            }
            "progress" => progress = value.parse::<u8>().ok().filter(|value| *value <= 100),
            "title" => title = Some(text(value, 256, 192)?),
            "msg" => message = Some(text(value, 2732, 2048)?),
            _ => {}
        }
    }
    let id = id.unwrap_or("").to_owned();
    let state = match state? {
        "idle" => ProgramStatusState::Idle,
        "working" => ProgramStatusState::Working,
        "done" => ProgramStatusState::Done,
        "blocked" => ProgramStatusState::Blocked,
        "error" => ProgramStatusState::Error,
        "clear" => return Some(Report::Clear(id)),
        _ => return None,
    };
    Some(Report::Set(ProgramStatusRecord {
        id,
        state,
        app,
        title,
        message,
        kind: if state == ProgramStatusState::Blocked {
            kind
        } else {
            None
        },
        progress: if matches!(
            state,
            ProgramStatusState::Working | ProgramStatusState::Blocked
        ) {
            progress
        } else {
            None
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_hierarchy_inheritance_and_lifetime() {
        let mut status = ProgramStatus::default();
        status.report("state=working:app=pi:msg=SGVsbG8", false);
        assert_eq!(status.records()[0].message.as_deref(), Some("Hello"));
        status.report(
            "state=blocked:id=job/child:kind=question:progress=20",
            false,
        );
        assert_eq!(
            status.summary().map(|record| (record.id, record.app)),
            Some(("job/child".into(), Some("pi".into())))
        );
        status.report("state=working:id=job:app=worker", true);
        assert_eq!(
            status.summary().and_then(|record| record.app),
            Some("worker".into())
        );
        status.report("state=done:id=job/child:kind=auth:progress=50", false);
        let child = &status.records()[2];
        assert_eq!(child.kind, None);
        assert_eq!(child.progress, None);
        status.expire_active();
        assert_eq!(status.records().len(), 1);
        assert_eq!(status.summary().and_then(|record| record.app), None);
        status.report("state=error:id=job/child/next", false);
        status.report("state=idle:id=job/children", false);
        status.report("state=clear:id=job/child", false);
        assert_eq!(status.records().len(), 1);
        assert_eq!(status.records()[0].id, "job/children");
        status.report("state=clear", false);
        assert!(status.records().is_empty());
    }

    #[test]
    fn malformed_reports_are_atomic_and_bounded() {
        let mut status = ProgramStatus::default();
        status.report("state=working:app=pi", false);
        let original = status.clone();
        for bad in [
            "state=new-state",
            "state=blocked:id=",
            "state=blocked:id=/bad",
            "state=blocked:id=bad//id",
            "state=blocked:id=bad!",
            "state=blocked:id=bad,",
            "state=blocked:id=bad=id",
            "state=blocked:msg=abcde",
            "state=clear:msg=Cg==",
            "state=blocked:title=/w==",
            "state=clear:msg=woA=",
            "state=clear:title=G1tt",
            "state=clear:id=1/2/3/4/5/6/7/8/9",
            "state=clear:abcdefghijklmnopq=x",
        ] {
            status.report(bad, false);
            assert_eq!(status, original, "{bad}");
        }
        for bad in [
            format!("state=clear:app={}", "a".repeat(33)),
            format!("state=clear:app={}", "!".repeat(33)),
            format!("state=clear:msg={}", "!".repeat(2733)),
            format!("state=clear:id={}", "a".repeat(33)),
            format!("state=clear:msg={}", "A".repeat(2733)),
            format!("state=clear:title={}", "A".repeat(257)),
            format!("state=clear:unknown={}", "a".repeat(4096)),
            format!("state=clear:msg={}", BASE64.encode(vec![b'a'; 2049])),
        ] {
            status.report(&bad, false);
            assert_eq!(status, original);
        }
        status.report(
            "not-a-pair:Bad=x:state=idle:state=blocked:unknown=x:msg=!:kind=other:progress=101",
            false,
        );
        assert_eq!(status.records()[0].state, ProgramStatusState::Blocked);
        assert_eq!(status.records()[0].app, None);
        assert_eq!(status.records()[0].kind, None);
        assert_eq!(status.records()[0].progress, None);
    }

    #[test]
    fn wire_size_caps_include_prefix_and_terminator() {
        use crate::TerminalScreen;
        use rmux_proto::TerminalSize;
        for terminator in ["\x07", "\x1b\\"] {
            let mut terminal = TerminalScreen::new(TerminalSize { cols: 80, rows: 24 }, 10);
            let prefix = "\x1b]00007501;state=working:extra=";
            let report = format!(
                "{prefix}{}{terminator}",
                "a".repeat(4096 - prefix.len() - terminator.len())
            );
            assert_eq!(report.len(), 4096);
            terminal.feed(report.as_bytes());
            assert_eq!(
                terminal.screen().program_status().summary().unwrap().state,
                ProgramStatusState::Working
            );
            let prefix = "\x1b]00007501;state=error:extra=";
            let report = format!(
                "{prefix}{}{terminator}",
                "a".repeat(4097 - prefix.len() - terminator.len())
            );
            terminal.feed(report.as_bytes());
            assert_eq!(
                terminal.screen().program_status().summary().unwrap().state,
                ProgramStatusState::Working
            );
        }
    }

    #[test]
    fn wire_negotiation_fragmentation_screen_switching_and_resets() {
        use crate::TerminalScreen;
        use rmux_proto::TerminalSize;
        let size = TerminalSize { cols: 80, rows: 24 };
        let mut terminal = TerminalScreen::new(size, 100);
        let mut other = TerminalScreen::new(size, 100);
        let reply = "\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\";
        for byte in b"\x1b]7501;?\x1b\\\x1b[c" {
            terminal.feed(&[*byte]);
        }
        assert_eq!(
            terminal.take_replies(),
            format!("{reply}\x1b[?1;2c").as_bytes()
        );
        terminal.feed(b"\x1b]7501;?\x07");
        assert_eq!(
            terminal.take_replies(),
            reply.replace("\x1b\\", "\x07").as_bytes()
        );
        for byte in b"\x1b]7501;state=working:app=pi\x1b\\" {
            terminal.feed(&[*byte]);
        }
        assert_eq!(terminal.screen().program_status().records().len(), 1);
        assert!(terminal.take_replies().is_empty());
        assert!(terminal.take_terminal_passthrough().is_empty());
        assert!(other.screen().program_status().records().is_empty());
        terminal.feed(b"\x1b[?1049h\x1b[?1049l\x1b[!p");
        terminal.resize(TerminalSize { cols: 90, rows: 30 });
        assert_eq!(terminal.screen().program_status().records().len(), 1);
        terminal.feed(b"\x1b]133;A;aid=1\x07");
        assert!(terminal.screen().program_status().records().is_empty());
        terminal.feed(b"\x1b]7501;state=done:app=pi\x07\x1b]133;A\x07");
        assert_eq!(terminal.screen().program_status().records().len(), 1);
        terminal.feed(b"\x1bc");
        assert!(terminal.screen().program_status().records().is_empty());
        other.feed(format!("\x1b]{}7501;state=working\x07", "0".repeat(4096)).as_bytes());
        assert!(other.screen().program_status().records().is_empty());
    }

    #[test]
    fn least_recently_updated_eviction_and_reset() {
        let mut status = ProgramStatus::default();
        for i in 0..MAX_RECORDS {
            status.report(&format!("state=done:id=job{i}"), false);
        }
        status.report("state=error:id=job0", false);
        status.report("state=done:id=extra", false);
        assert_eq!(status.records().len(), MAX_RECORDS);
        assert!(!status.records().iter().any(|record| record.id == "job1"));
        assert!(status.records().iter().any(|record| record.id == "job0"));
        status.clear();
        assert!(status.records().is_empty());
    }
}
