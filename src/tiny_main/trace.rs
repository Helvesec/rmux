use std::env;
use std::io::{self, Write};

const TINY_TRACE_ENV: &str = "RMUX_TINY_TRACE";

pub(super) fn trace_direct(command: &str) {
    trace("direct", command);
}

pub(super) fn trace_fallback(reason: &str) {
    trace("fallback", reason);
}

fn trace(kind: &str, detail: &str) {
    if env::var_os(TINY_TRACE_ENV).is_none() {
        return;
    }
    let _ = writeln!(io::stderr().lock(), "rmux tiny: {kind}: {detail}");
}
