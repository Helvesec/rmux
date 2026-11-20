//! `#{wrap_flag}` follows the automatic return of the *targeted* pane, so a
//! script can find the pane that turned it off and go back to it.
//!
//! tmux reads the bit on the pane the format tree points at — `wp->base.mode &
//! MODE_WRAP`, `"1"` or `"0"`, and NULL with no pane at all
//! (`format.c:3031-3043`) — while `?7l` and `?7h` are what move that bit
//! (`input.c:1921` and `input.c:2020`). The expectations below are the literals
//! pinned before any measurement and then measured on a tmux 3.7b built for
//! this lot from the qualified sources
//! (`wrap1/native/oracle-a1`).
//!
//! The fixture is the MF1 one, reused: a real quiet pair, each pane's producer
//! awaited through its own marker, and payloads published on the pane's real
//! input path. Nothing here polls the flag it is about to read.

use super::terminal_mode_flags::{pin_quiet_producer, queue, wait_for_producer_marker};
use super::*;
use crate::pane_io::{PaneObservationItem, PaneOutputReceiver, PaneOutputSender};
use rmux_core::events::OutputCursorItem;
use tokio::time::{sleep, Instant};

/// `?7l`, then the cursor on line 1 column 78, then six letters. With no
/// automatic return the last column keeps the last letter written, so the line
/// ends `ABF`. The trailing `CUP 5;1` is the native recipe's own literal; in
/// process it only parks the cursor.
const TURN_OFF: &[u8] = b"\x1b[?7l\x1b[1;78HABCDEF\x1b[5;1H";
/// `?7h`, then line 2 column 78 and six more: `UVW` fills the end of line 2 and
/// the automatic return carries `XYZ` onto line 3.
const TURN_ON: &[u8] = b"\x1b[?7h\x1b[2;78HUVWXYZ\x1b[5;1H";

/// What `display-message` answers for one target, read directly.
async fn wrap(handler: &RequestHandler, target: &str) -> String {
    queue(
        handler,
        &format!("display-message -p -t {target} '#{{wrap_flag}}'"),
    )
    .await
}

/// The inventory a script would read, one line per pane.
async fn inventory(handler: &RequestHandler, name: &str) -> Vec<String> {
    queue(
        handler,
        &format!("list-panes -t {name}:0 -F '#{{pane_id}}|#{{wrap_flag}}'"),
    )
    .await
    .lines()
    .map(str::to_owned)
    .collect()
}

/// The single pane of that inventory whose automatic return is off.
fn the_one_that_is_off(lines: &[String]) -> String {
    let off: Vec<&String> = lines.iter().filter(|line| line.ends_with("|0")).collect();
    assert_eq!(off.len(), 1, "exactly one pane is off in {lines:?}");
    off[0]
        .split('|')
        .next()
        .expect("the inventory line carries a pane id")
        .to_owned()
}

async fn screen(handler: &RequestHandler, target: &str) -> Vec<String> {
    queue(handler, &format!("capture-pane -p -t {target}"))
        .await
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Columns 78, 79 and 80 of one captured line - the three cells at the right
/// edge this journey is about. The fixture's own producer announced itself on
/// the first line before any payload, which is why the edge is read by column
/// rather than the whole line being compared.
fn edge(lines: &[String], index: usize) -> &str {
    let line = &lines[index];
    assert_eq!(line.len(), 80, "line {} is {:?}", index + 1, line);
    &line[77..80]
}

async fn identities(handler: &RequestHandler, name: &str) -> String {
    queue(
        handler,
        &format!("list-panes -t {name}:0 -F '#{{pane_id}}|#{{pane_pid}}|#{{pane_width}}x#{{pane_height}}'"),
    )
    .await
}

/// WRAP1 R2 diagnostic — bounded to this one exact.
///
/// The Windows receipt for this test (`runs/integration/wrap1-r1/windows/
/// test-0.*`) shows line 1 still holding only the producer's marker, 22 bytes
/// instead of 80, while the flag had already moved to 0 and the selection of
/// the real PaneId had passed. The reviewed hypothesis is the fixture's own:
/// `feed` publishes behind ConPTY while the child's real console knows only its
/// marker, and selecting B->A re-applies the native size even when identical
/// (`handler_pane/selection.rs:244` -> `pane_terminals/window_selection.rs:163`
/// -> `native_resize.rs:171` -> `rmux-pty/backend/windows/pty.rs:239`), a
/// repaint already documented for silent children in
/// `handler_client/switch_atomicity_tests.rs:395` and `:1179`.
///
/// What the receipt does not carry is the bytes. These helpers record them, at
/// four named phases, WITHOUT touching one assertion, one sleep, one
/// normalisation or the reader and resize themselves. They print; they never
/// decide.
struct Diagnostic {
    receiver: PaneOutputReceiver,
    phase: usize,
}

impl Diagnostic {
    /// Registers a recoverable observer at the pane's own output boundary.
    fn open(output: &PaneOutputSender) -> Self {
        let (boundary, (), receiver) = output.capture_with_observer(|| ());
        println!(
            "[wrap1-diag] observer opened at generation={} next_sequence={} invalidation_revision={} process_exit_revision={}",
            boundary.generation,
            boundary.next_output_sequence,
            boundary.invalidation_revision,
            boundary.process_exit_revision
        );
        Self { receiver, phase: 0 }
    }

    /// Drains everything the pane's output has published since the last phase.
    ///
    /// An `Invalidated` carrying `Resize` followed by output holding only the
    /// marker is a repaint that erased the letters; output carrying the CUP
    /// while the letters land off the measured line is the other story. This
    /// prints both without choosing between them.
    fn drain(&mut self, label: &str) {
        let mut seen = 0usize;
        while let Some(item) = self.receiver.try_recv_observed() {
            seen += 1;
            match item {
                PaneObservationItem::Output(OutputCursorItem::Event(event)) => println!(
                    "[wrap1-diag] {label} output sequence={} bytes={} hex={}",
                    event.sequence(),
                    event.bytes().len(),
                    hex(event.bytes())
                ),
                PaneObservationItem::Output(OutputCursorItem::Gap(gap)) => {
                    println!("[wrap1-diag] {label} gap {gap:?}");
                }
                PaneObservationItem::Invalidated(invalidation) => println!(
                    "[wrap1-diag] {label} INVALIDATED reason={:?} generation={} next_sequence={} invalidation_revision={}",
                    invalidation.reason,
                    invalidation.boundary.generation,
                    invalidation.boundary.next_output_sequence,
                    invalidation.boundary.invalidation_revision
                ),
                PaneObservationItem::ProcessExited { output_sequence } => {
                    println!("[wrap1-diag] {label} process exited at {output_sequence:?}");
                }
            }
        }
        println!("[wrap1-diag] {label} drained {seen} item(s)");
    }

    /// One named phase: what the pane's output published, then what the live
    /// screen says about itself and what it is actually holding.
    async fn phase(&mut self, handler: &RequestHandler, name: &str, target: &str, label: &str) {
        self.phase += 1;
        let label = format!("phase{}:{label}", self.phase);
        self.drain(&label);
        let state = queue(
            handler,
            &format!(
                "display-message -p -t {target} '#{{pane_width}}x#{{pane_height}}|#{{cursor_x}},#{{cursor_y}}|#{{history_size}}|#{{origin_flag}}|#{{alternate_on}}|#{{wrap_flag}}|#{{insert_flag}}'"
            ),
        )
        .await;
        println!("[wrap1-diag] {label} size|cursor|history|origin|alternate|wrap|insert = {state}");
        let lines = screen(handler, target).await;
        for (index, line) in lines.iter().enumerate().take(3) {
            println!(
                "[wrap1-diag] {label} line{} len={} cols78_80={:?} text={:?}",
                index + 1,
                line.len(),
                line.get(77..80),
                line
            );
        }
        if let Some(first) = lines.first() {
            println!("[wrap1-diag] {label} line1 hex={}", hex(first.as_bytes()));
        }
        let neighbour = screen(handler, &format!("{name}:0.1")).await;
        println!(
            "[wrap1-diag] {label} neighbour lines={} first={:?}",
            neighbour.len(),
            neighbour.first()
        );
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(unix)]
/// Renders bytes as octal escapes for a POSIX `printf` format string.
///
/// Every byte is escaped, so the rendering can never be read as a conversion or
/// a shell metacharacter, and the business constants above stay the one source.
fn octal_format(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("\\{byte:03o}")).collect()
}

#[cfg(windows)]
/// Renders bytes as a PowerShell character-code list, from the same constants.
fn char_codes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| byte.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// The reserved line this journey never measures: the tool's receipts land
/// there, and so does the echo of the words typed to command it.
const PARK: &[u8] = b"\x1b[5;1H";
/// Erase to end of line, so a receipt never shows a previous one's tail.
const ERASE: &[u8] = b"\x1b[K";
const RECEIPT: &str = "WRAPRCPT";

/// A's tool: it announces itself, parks on the reserved line, and then writes
/// `TURN_OFF` or `TURN_ON` INTO ITS OWN STDOUT when the journey types the
/// matching word into its pane.
///
/// This is what the Windows diagnostic required. `feed` published those bytes
/// behind ConPTY, where the child's real console never knew them: the first
/// repaint that console performed - `ESC[?25l ESC[H <marker> ESC[K` then
/// fifteen `CRLF ESC[K`, 121 bytes observed at sequence 6 - restored the console
/// as it truly was and erased the letters. A console that wrote them itself
/// repaints them.
///
/// Each side keeps its own explicit boundary. The Windows tool names
/// `ENABLE_VIRTUAL_TERMINAL_PROCESSING` (0x4) on its own stdout, reports the
/// mode it found and the one it set, and restores the first before leaving;
/// without that flag a console takes the escapes for text. The receipt carries
/// the sequence, this run's nonce and the tool's own pid, and it is read back
/// through `capture-pane`: a ConPTY cursor report would attest that the console
/// answered, never that rmux consumed anything.
fn commanded_producer_command(marker: &str, nonce: &str) -> String {
    #[cfg(unix)]
    {
        let park = octal_format(PARK);
        let erase = octal_format(ERASE);
        format!(
            "printf '{marker}\\n'; printf '{park}'; seq=0;              while IFS= read -r word; do              case \"$word\" in              OFF) printf '{off}' ;;              ON) printf '{on}' ;;              *) continue ;;              esac;              seq=$((seq+1));              printf '{park}{RECEIPT} %s {nonce} %s vt=posix{erase}{park}' \"$seq\" $$;              done",
            off = octal_format(TURN_OFF),
            on = octal_format(TURN_ON),
        )
    }
    #[cfg(windows)]
    {
        crate::test_shell::powershell_encoded_command(&format!(
            "Add-Type -Namespace WrapOne -Name Con -MemberDefinition '             [DllImport(\"kernel32.dll\", SetLastError=true)] public static extern IntPtr GetStdHandle(int n);              [DllImport(\"kernel32.dll\", SetLastError=true)] public static extern bool GetConsoleMode(IntPtr h, out uint m);              [DllImport(\"kernel32.dll\", SetLastError=true)] public static extern bool SetConsoleMode(IntPtr h, uint m);';              $h=[WrapOne.Con]::GetStdHandle(-11); $before=0;              $read=[WrapOne.Con]::GetConsoleMode($h,[ref]$before);              $set=[WrapOne.Con]::SetConsoleMode($h, $before -bor 0x4); $after=0;              [void][WrapOne.Con]::GetConsoleMode($h,[ref]$after);              function Emit($c){{ [Console]::Out.Write((-join ($c | ForEach-Object {{[char]$_}}))); [Console]::Out.Flush() }};              [Console]::Out.Write('{marker}' + [char]13 + [char]10); [Console]::Out.Flush();              Emit @({park});              $seq=0;              try {{ while ($true) {{              $word=[Console]::In.ReadLine(); if ($word -eq $null) {{ break }}; $word=$word.Trim();              if ($word -eq 'OFF') {{ Emit @({off}) }} elseif ($word -eq 'ON') {{ Emit @({on}) }} else {{ continue }};              $seq++;              Emit @({park});              [Console]::Out.Write(\"{RECEIPT} $seq {nonce} $PID vt=$before-$after-$read$set\");              [Console]::Out.Flush();              Emit @({erase}); Emit @({park}) }} }}              finally {{ if ($read) {{ [void][WrapOne.Con]::SetConsoleMode($h, $before) }} }}",
            park = char_codes(PARK),
            erase = char_codes(ERASE),
            off = char_codes(TURN_OFF),
            on = char_codes(TURN_ON),
        ))
    }
}

/// The journey's pair: A is the commanded tool, B the silent neighbour, and the
/// split leaves B active exactly as it did before.
async fn commanded_pair(name: &str, nonce: &str) -> (RequestHandler, SessionName) {
    commanded_pair_with_handler(RequestHandler::new(), name, nonce).await
}

async fn commanded_pair_with_handler(
    handler: RequestHandler,
    name: &str,
    nonce: &str,
) -> (RequestHandler, SessionName) {
    let session = session_name(name);
    let marker_a = format!("MF1RDY-{name}-A");
    let marker_b = format!("MF1RDY-{name}-B");

    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::DefaultCommand,
            value: commanded_producer_command(&marker_a, nonce),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(session.clone(), 0, 0))
        .await;
    wait_for_producer_marker(&handler, name, 0, &marker_a).await;

    pin_quiet_producer(&handler, &marker_b).await;
    queue(&handler, &format!("split-window -v -l 7 -t {name}:0.0")).await;
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(session.clone(), 0, 1))
        .await;
    wait_for_producer_marker(&handler, name, 1, &marker_b).await;

    (handler, session)
}

/// Types one word into A's pane and waits for the receipt it produces.
///
/// The barrier is independent of everything this journey measures: it is the
/// tool's own receipt, on the reserved line 5, read back through `capture-pane` -
/// so it attests that rmux consumed the bytes, not merely that a console
/// answered. Nothing here polls a flag, a cell or a screen the test asserts on.
async fn command(
    handler: &RequestHandler,
    target: &str,
    word: &str,
    seq: u32,
    nonce: &str,
) -> String {
    queue(handler, &format!("send-keys -t {target} -l {word}")).await;
    queue(handler, &format!("send-keys -t {target} Enter")).await;
    let wanted = format!("{RECEIPT} {seq} {nonce} ");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let lines = screen(handler, target).await;
        if let Some(line) = lines.get(4) {
            if line.contains(&wanted) {
                println!("[wrap1-diag] receipt seq={seq} line5={line:?}");
                return line.clone();
            }
        }
        assert!(
            Instant::now() < deadline,
            "{word}: no {wanted:?} on the reserved line of {target}: {lines:?}"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

/// What one receipt says about the tool that wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Writer {
    seq: u32,
    nonce: String,
    pid: u32,
}

/// Reads `WRAPRCPT <seq> <nonce> <pid> vt=...` as the tool published it.
fn writer_of(receipt: &str) -> Writer {
    let fields: Vec<&str> = receipt.split_whitespace().collect();
    assert_eq!(
        fields.first().copied(),
        Some(RECEIPT),
        "the receipt does not start with its own marker: {receipt:?}"
    );
    assert!(fields.len() >= 4, "the receipt is short: {receipt:?}");
    Writer {
        seq: fields[1].parse().expect("the receipt carries a sequence"),
        nonce: fields[2].to_owned(),
        pid: fields[3]
            .parse()
            .unwrap_or_else(|_| panic!("the receipt carries a process id: {:?}", fields[3])),
    }
}

/// How far a bounded walk from the writer got towards the pane's own process.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Filiation {
    /// The pane's process was reached, through this chain of ancestors.
    Reached(Vec<u32>),
    /// A pid repeated: the walk was cut rather than followed round.
    Cycle(Vec<u32>, u32),
    /// An ancestor had no parent to follow.
    Lost(Vec<u32>),
    /// The platform refused to answer for one of the ancestors.
    Failed(Vec<u32>, String),
    /// Sixteen ancestors were not enough.
    Exhausted(Vec<u32>),
}

/// The most ancestors this walk will ever follow.
const MAX_ANCESTORS: usize = 16;

/// Walks from `pid` towards `target`, at most [`MAX_ANCESTORS`] steps.
///
/// Every way the walk can end is a value of its own: reaching the pane, meeting
/// a cycle, running out of parents, being refused an answer, or exhausting its
/// bound. None of them is silently the same as success.
fn filiation(pid: u32, target: u32) -> Filiation {
    let inspector = rmux_os::process::ProcessInspector;
    let mut chain = vec![pid];
    let mut seen = vec![pid];
    let mut current = pid;
    if current == target {
        return Filiation::Reached(chain);
    }
    for _ in 0..MAX_ANCESTORS {
        match inspector.parent_pid(current) {
            Ok(Some(parent)) => {
                chain.push(parent);
                if parent == target {
                    return Filiation::Reached(chain);
                }
                if seen.contains(&parent) {
                    return Filiation::Cycle(chain, parent);
                }
                seen.push(parent);
                current = parent;
            }
            Ok(None) => return Filiation::Lost(chain),
            Err(error) => return Filiation::Failed(chain, error.to_string()),
        }
    }
    Filiation::Exhausted(chain)
}

/// Holds the writer to a real, live process that belongs to this pane.
///
/// The pid must be a non-zero `u32`, the platform must say it is live, and a
/// BOUNDED walk of at most sixteen ancestors must reach the pane's own process.
/// On Unix the tool IS that process, so the equality this journey has always
/// asserted is kept and the walk reaches at its first step; on Windows the pane
/// records the process it launched while the writer is the PowerShell holding
/// the console, so the filiation is what carries the claim - a non-empty
/// numeric string never did.
fn assert_writer_identity(phase: &str, writer: &Writer, pane_pid: &str) {
    let pane: u32 = pane_pid
        .parse()
        .unwrap_or_else(|_| panic!("{phase}: the pane reports a process id: {pane_pid:?}"));
    assert_ne!(
        writer.pid, 0,
        "{phase}: the writing process id is a real one"
    );
    let live = rmux_os::process::ProcessInspector
        .is_live(writer.pid)
        .unwrap_or_else(|error| panic!("{phase}: liveness of {} refused: {error}", writer.pid));
    assert_eq!(
        live,
        Some(true),
        "{phase}: the writer {} is not live",
        writer.pid
    );
    let walked = filiation(writer.pid, pane);
    println!(
        "[wrap1-diag] {phase} writer={:?} pane_pid={pane} filiation={walked:?}",
        writer
    );
    match &walked {
        Filiation::Reached(chain) => {
            #[cfg(unix)]
            assert_eq!(
                writer.pid, pane,
                "{phase}: on Unix the tool is the pane's own child"
            );
            #[cfg(unix)]
            assert_eq!(chain.len(), 1, "{phase}: that filiation is the pane itself");
            #[cfg(windows)]
            assert!(
                chain.len() <= MAX_ANCESTORS + 1,
                "{phase}: the filiation is within its bound"
            );
        }
        other => panic!("{phase}: the writer does not descend from the pane: {other:?}"),
    }
}

// Only the native Windows events have this collector.
#[cfg(windows)]
#[path = "wrap1/conpty_attest.rs"]
mod conpty_attest;

#[tokio::test]
async fn the_tool_that_turned_its_return_off_is_found_by_that_flag_and_resumes() {
    #[cfg(windows)]
    conpty_attest::collect(wrap_journey).await;
    #[cfg(not(windows))]
    wrap_journey().await;
}

async fn wrap_journey(
    #[cfg(windows)] collector: std::sync::Arc<conpty_attest::ConptyCollector>,
    #[cfg(windows)] installed: bool,
) {
    let name = "wrap1-journey";
    let nonce = format!("n{}", std::process::id());
    // The scoped dispatch is active before either pane is spawned.
    let (handler, session) = commanded_pair(name, &nonce).await;

    let before = identities(&handler, name).await;
    let neighbour = screen(&handler, &format!("{name}:0.1")).await;

    // The R2 diagnostic rides along: it observes, it never intervenes.
    let output = {
        let state = handler.state.lock().await;
        state
            .pane_output_for_target(&session, 0, 0)
            .expect("the pane has an output")
    };
    let mut diagnostic = Diagnostic::open(&output);
    diagnostic
        .phase(&handler, name, &format!("{name}:0.0"), "before-feed")
        .await;

    // Both tools return automatically, in the direct reading and in the
    // inventory alike.
    assert_eq!(wrap(&handler, &format!("{name}:0.0")).await, "1");
    assert_eq!(wrap(&handler, &format!("{name}:0.1")).await, "1");
    let start = inventory(&handler, name).await;
    assert!(
        start.iter().all(|line| line.ends_with("|1")),
        "both panes start with their automatic return on: {start:?}"
    );

    // A turns it off and writes past the right edge - itself, into its own
    // stdout, because the journey types the word that commands it.
    let receipt = command(&handler, &format!("{name}:0.0"), "OFF", 1, &nonce).await;
    let writer_off = writer_of(&receipt);
    assert_eq!(writer_off.seq, 1, "the OFF receipt is the first");
    assert_eq!(
        writer_off.nonce, nonce,
        "the OFF receipt carries this run's nonce"
    );
    let pane_pid_a = queue(
        &handler,
        &format!("display-message -p -t {name}:0.0 '#{{pane_pid}}'"),
    )
    .await;
    assert_writer_identity("OFF", &writer_off, &pane_pid_a);

    // The console THIS pane ended up on. The attestation is the per-child
    // record emitted after that child's companion startup and matched on its
    // own pid: a global `ConPTY created` line is emitted BEFORE any spawn,
    // belongs to no child in particular, and a later fallback may have
    // recreated the console since. Nothing here requires the companion - what
    // was retained is reported, and only an attestation that cannot be trusted
    // is a failure.
    #[cfg(windows)]
    {
        let attestation = conpty_attest::conpty_attestation(&collector, installed);
        for (target, rendered) in &attestation.records {
            println!("[wrap1-diag] {target} {rendered}");
        }
        let pane_pid: u32 = pane_pid_a
            .parse()
            .unwrap_or_else(|_| panic!("the pane reports a process id: {pane_pid_a:?}"));
        if let Some(reason) = attestation.invalid_because() {
            panic!("the backend of pane A is unattested: {reason}");
        }
        let per_child = attestation.per_child();
        let mine: Vec<&(u32, String)> = per_child
            .iter()
            .filter(|(pid, _)| *pid == pane_pid)
            .collect();
        assert_eq!(
            mine.len(),
            1,
            "exactly one per-child attestation belongs to pane A (pid {pane_pid}); collected {per_child:?}"
        );
        println!("[wrap1-diag] pane A backend: {}", mine[0].1);
    }
    diagnostic
        .phase(&handler, name, &format!("{name}:0.0"), "after-feed")
        .await;
    assert_eq!(wrap(&handler, &format!("{name}:0.0")).await, "0");
    assert_eq!(
        wrap(&handler, &format!("{name}:0.1")).await,
        "1",
        "the neighbour never asked for anything"
    );

    // The script finds it by that flag alone, selects the identity that came
    // back, and keeps it as its target from here on.
    let found = the_one_that_is_off(&inventory(&handler, name).await);
    diagnostic
        .phase(&handler, name, &found, "before-selection")
        .await;
    queue(&handler, &format!("select-pane -t {found}")).await;
    diagnostic
        .phase(&handler, name, &found, "after-selection")
        .await;
    assert_eq!(
        queue(
            &handler,
            &format!("display-message -p -t {name}:0 '#{{pane_index}}'")
        )
        .await,
        "0",
        "the pane the script selected is the one it found"
    );
    assert_eq!(wrap(&handler, &found).await, "0");

    let drawn = screen(&handler, &found).await;
    assert_eq!(
        edge(&drawn, 0),
        "ABF",
        "with no automatic return the last column keeps the last letter"
    );
    assert_eq!(
        screen(&handler, &format!("{name}:0.1")).await,
        neighbour,
        "the neighbour's screen is the one it started with"
    );

    // A turns the mode back on: the next line returns by itself.
    let receipt_on = command(&handler, &found, "ON", 2, &nonce).await;
    // The same tool, proved at both phases: the second receipt carries this
    // run's nonce, the sequence that follows the first, and the SAME process,
    // still live and still descending from the pane the script selected.
    let writer_on = writer_of(&receipt_on);
    assert_eq!(writer_on.seq, 2, "the ON receipt is the second");
    assert_eq!(
        writer_on.nonce, nonce,
        "the ON receipt carries this run's nonce"
    );
    assert_eq!(
        writer_on.pid, writer_off.pid,
        "the tool that turned the return back on is the one that turned it off"
    );
    assert_writer_identity(
        "ON",
        &writer_on,
        &queue(
            &handler,
            &format!("display-message -p -t {found} '#{{pane_pid}}'"),
        )
        .await,
    );
    assert_eq!(wrap(&handler, &found).await, "1");
    assert_eq!(wrap(&handler, &format!("{name}:0.1")).await, "1");
    let back = inventory(&handler, name).await;
    assert!(
        back.iter().all(|line| line.ends_with("|1")),
        "nothing is off any more: {back:?}"
    );

    let drawn = screen(&handler, &found).await;
    assert_eq!(edge(&drawn, 0), "ABF", "the first line is untouched");
    assert_eq!(edge(&drawn, 1), "UVW", "the second line fills to the edge");
    assert_eq!(
        drawn[2], "XYZ",
        "and the automatic return carried the rest onto the third"
    );
    assert_eq!(
        screen(&handler, &format!("{name}:0.1")).await,
        neighbour,
        "the neighbour's screen is still the one it started with"
    );

    assert_eq!(
        identities(&handler, name).await,
        before,
        "the same panes, the same pids and the same sizes throughout"
    );
}
