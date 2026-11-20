//! Controlled EOF for the small Unix clock-paste fixtures.
//!
//! The PTY keeps raw byte mappings, signals and echo disabled. Only canonical
//! framing is enabled, with editing disabled and VEOF reserved for the harness.
//! After all awaited writes, two VEOFs flush the final unterminated line then
//! end `cat`. Publication means EOF was read, not that N bytes arrived or a
//! quiet interval elapsed. The whole capture, including surplus bytes, is
//! compared by `FinalSinkSlot`.
//!
//! This is deliberately limited to short fixtures: canonical queues are
//! bounded, and arbitrary binary payloads may contain the reserved VEOF byte.
//! Windows keeps its existing raw reader; this is no claim about ConPTY EOF.

use super::FinalSinkSlot;

impl FinalSinkSlot {
    pub(crate) fn until_controlled_eof(mut self) -> Self {
        assert!(self.expected.len() < 128, "EOF fixture must stay short");
        assert!(
            !self.expected.contains(&4) && !self.expected.contains(&0),
            "EOF fixture reserves VEOF and disabled control characters"
        );
        self.capture_until_eof = true;
        self
    }
}

pub(super) fn pane_command(slot: &FinalSinkSlot) -> Vec<String> {
    let quote = crate::test_shell::command_quote;
    let announce = if slot.bracket_aware {
        r"printf '\033[?2004h'"
    } else {
        ":"
    };
    let script = format!(
        "set -u\n\
         park() {{\n\
         i=0\n\
         while [ \"$i\" -lt {ticks} ] && [ ! -e {stop} ]; do\n\
         sleep 0.05\n\
         i=$((i+1))\n\
         done\n\
         }}\n\
         fail() {{\n\
         printf '%s' \"$1\" > {error}\n\
         park\n\
         printf '' > {done}\n\
         exit 1\n\
         }}\n\
         stty raw -echo icanon -isig -iexten eof '^D' eol undef eol2 undef \
         erase undef kill undef 2>/dev/null || fail 'controlled EOF setup failed'\n\
         {announce} || fail 'capability announcement failed'\n\
         printf '' > {ready} || fail 'readiness failed'\n\
         cat > {partial} || fail 'read through controlled EOF failed'\n\
         mv {partial} {out} || fail 'EOF capture publication failed'\n\
         park\n\
         printf '' > {done}\n",
        ready = quote(&slot.path(super::READY_FILE)),
        partial = quote(&slot.path(super::OUT_PARTIAL_FILE)),
        out = quote(&slot.path(super::OUT_FILE)),
        error = quote(&slot.path(super::ERROR_FILE)),
        stop = quote(&slot.path(super::STOP_FILE)),
        done = quote(&slot.path(super::DONE_FILE)),
        ticks = super::CHILD_PARK_SECONDS * 20,
    );
    vec!["/bin/sh".to_owned(), "-c".to_owned(), script]
}
