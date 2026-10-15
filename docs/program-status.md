# Program status (OSC 7501)

rmux implements revision 0.4 of the terminal-local
[Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status).
A program can report `idle`, `working`, `done`, `blocked` or `error`, or clear
records. Query `OSC 7501 ; ? ST` is answered directly by the pane's terminal
parser, including when detached; no supporting outer terminal is required.
Both BEL and ESC-backslash termination are accepted, with matching replies.

## Formats

Use existing detached RPC or CLI format expansion:

```sh
rmux display-message -p -t work:0.0 '#{pane_program_status_json}'
rmux list-sessions -F '#{session_name} #{session_program_status_json}'
```

`pane_program_status_json` is a JSON object containing `version: 1`, `observed`,
`summary` and `records`. `observed` distinguishes never-reported from a previously
reported terminal whose records were cleared/expired. `summary` is null when
empty; `records` contains all validated records, oldest update first.

The summary prioritizes `blocked > working > error > done > idle`, newest first
on ties, and resolves inherited application IDs. The stored records preserve
explicit application IDs rather than materializing inheritance.

Scalar formats expose the summary:

- `pane_program_status` (state)
- `pane_program_status_app`, `pane_program_status_id`, `pane_program_status_kind`
- `pane_program_status_progress`, `pane_program_status_title`, `pane_program_status_message`

`session_program_status_json` returns `version: 1` and `panes`, each with
`pane_id`, `window_index`, `pane_index`, `observed` and `summary`. It supports a
single batched `list-sessions` request without changing binary protocol frames.
Missing summary values are empty in scalar expansion. JSON should be preferred
when consuming free text through a delimited format.

## Lifetime and bounds

Records belong to the pane, not its primary/alternate screen. Resize, scrollback
clearing, screen switching and DECSTR preserve records. RIS clears records and
resets `observed`. A respawn creates a new transcript with no old records.

A shell prompt (OSC 133 A) or attached-process exit expires working, blocked and
idle records. Done/error results survive. Exit observed before PTY drain cannot
be undone by late buffered active reports. Reader generations still prevent an
old process reader from populating a respawned pane.

On Unix, foreground process-group changes also expire active reports. Groups are
sampled before parsing output and before exposing status, so returning to an
uninstrumented shell can be detected even without another PTY byte. This is a
heuristic, not authenticated writer identity; background writers, pipelines and
children sharing their parent's group cannot always be distinguished. Failure
to inspect the group is not treated as an exit. Sampling is serialized with
transcript updates. After an observed foreground-group handoff, queued/fragmented reports
cannot resurrect active status until the old input drain completes. An ambiguous
handoff therefore fails closed, including an unparsed first report. This can drop
a new application's first active report if it skips negotiation; a report after
the drain establishes activity. Pi negotiates before reporting.
There is no heartbeat timeout.

Parsing is atomic and bounded: 4096-byte sequences, protocol text/key/ID limits,
strict base64 UTF-8 decoding without control characters, hierarchical clears,
complete record replacement and nearest-ancestor application inheritance. At
most 256 records are retained, evicting the least recently updated record.
Reports never echo untrusted data back into the PTY. Presentation consumers must
treat decoded text as plain text and disarm invisible direction controls.
