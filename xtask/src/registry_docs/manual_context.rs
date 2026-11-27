//! Maintained explanations accompanying the generated command and option tables.
//! Kept verbatim from the central manual during the L18.1 composition.

pub(super) const TEXT: &str = r##".SH DESCRIPTION
RMUX is a cross-platform terminal multiplexer built around a local client and
server architecture. Unix builds use OS socket credentials; Windows builds use
same-user named pipes and ConPTY.
It preserves tmux-compatible command-line behavior while keeping RMUX product
labels in binary output, help text, and status text.
.SH TMUX COMPATIBILITY POLICY
RMUX treats tmux compatibility as a script and workflow contract, not as a
requirement to copy known parser bugs. Command names, supported flags, target
resolution, common diagnostics, config loading, and scriptable output should
match tmux where that behavior is intentional and useful.
.PP
When tmux behavior is a known quirk that silently hides errors or leaks an
internal sentinel, RMUX keeps safer product semantics only when the release
divergence ledger lists the behavior. For example, RMUX rejects non-numeric
.B capture-pane
bounds, and preserves a trailing literal
.B #
in format strings.
.PP
For
.BR "if-shell -F" ,
RMUX follows the pinned tmux oracle's numeric false handling, including
zero-prefixed numeric strings.
.PP
Very large positive absolute pane sizes may be clamped to the available layout
space, while zero, negative, non-numeric, and overflowed sizes are rejected.
.SH TOP-LEVEL OPTIONS
.TP
.B -2
Accept the tmux-compatible 256-colour compatibility flag.
.TP
.B -C
Enter tmux text control mode.
.TP
.B -D
Run the hidden server in the foreground.
.TP
.B -h
Print the tmux-style top-level usage line to standard output and exit 0.
.TP
.B -l
Accept the tmux-compatible login-shell flag.
.TP
.B -N
Disable automatic server startup for commands that otherwise auto-start.
.TP
.B -u
Force UTF-8 mode for the client process.
.TP
.B -V
Print the RMUX package version and exit 0.
For build and support details, use
.B rmux diagnose --json
or
.BR rmux\ diagnose\ --human .
In short-option clusters,
.B -V
stops the
.B -h
scan, so
.B -Vh
prints the version.
.TP
.B -v
Accept the tmux-compatible verbose flag.
.TP
.BI "-c " shell-command
Start or connect to the rmux server, then execute the resolved shell with
.B "-c shell-command"
using the current terminal.
.TP
.BI "-f " file
Load the given configuration file.
.TP
.BI "-L " socket-name
Use the named socket below the rmux UID socket directory.
.TP
.BI "-S " socket-path
Use the given absolute socket path.
.TP
.BI "-T " features
Declare tmux-compatible terminal features for this client, as a
comma-separated list. The option may be repeated. A
.B -T
declaration is applied whatever
.B TERM
holds, including when it is empty, whereas
.B terminal-features
and
.B terminal-overrides
are matched against the client's terminal name and, for an empty name, match
only a selector consisting entirely of
.B *
characters. See
.BR "TERMINAL GRAPHICS" .
.SH COMMAND HELP
Command help uses
.BR "rmux <command> --help" .
Short
.B -h
remains available for tmux command semantics such as
.BR "rmux split-window -h" .
.SH LIST-COMMANDS
.B list-commands
is client-local and does not contact the server.
With no arguments it prints one line per implemented command in tmux command
table order.
.PP
.B list-keys
can render the compiled default key table without a running server when no
runtime key-table mutations are available.
.SH TERMINAL GRAPHICS
RMUX forwards a pane's image escape sequences to the terminal the client is
attached from. RMUX does not decode or draw the image itself, so the outer
terminal produces what is displayed.
.PP
Forwarding is off by default and requires two independent gates to be open.
First,
.B allow-passthrough
must be set for the pane; it defaults to
.BR off ,
and
.B on
or
.B all
opens it. Second, the outer terminal must advertise the protocol:
.B sixel
for SIXEL, or
.B kitty-graphics
for the Kitty graphics protocol.
.PP
To enable SIXEL from a configuration file:
.nf

set -g allow-passthrough on
set -as terminal-features 'xterm*:sixel'

.fi
.B terminal-features
and
.B terminal-overrides
selectors are matched against the attaching client's terminal name, taken from
.BR TERM .
A client may attach with no terminal name at all, an empty or unset
.BR TERM ,
and for such a client only a selector consisting entirely of one or more
.B *
characters
.RB ( * ,
.BR ** ,
and equivalent all-star forms) matches:
.nf

set -as terminal-features '*:sixel'

.fi
The
.B terminal-overrides
equivalent is
.BR *:Sxl .
Everything else stays inert against an empty name: named selectors such as
.BR xterm* ,
.BR ? ,
bracket classes such as
.BR [a-z] ,
escaped literals such as
.BR \e* ,
and empty or malformed selectors. The shipped defaults are named selectors and
are unaffected.
.PP
tmux 3.7b refuses to attach at all with an empty or unset
.BR TERM ,
while RMUX accepts that attach and applies all-star selectors to it. This is a
measured RMUX product divergence, not tmux parity.
.PP
The other route is to declare the capability on the client with the top-level
.B -T
flag, which applies whatever
.B TERM
holds:
.nf

rmux -T sixel attach -t work

.fi
The same flag serves every command that attaches, such as
.BR "rmux -T sixel new-session -s work" ,
and
.B "-T kitty-graphics"
selects the Kitty graphics protocol.
Changes to
.BR allow-passthrough ,
.B terminal-features
and
.B terminal-overrides
are re-evaluated for already attached clients, while a
.B -T
declaration is fixed for the lifetime of the client that passed it.
.PP
Native Windows attaches with no
.BR TERM ,
so named selectors such as
.B xterm*
never match there. Two routes work: the client-side declaration
.BR "rmux -T sixel attach -t work" ,
or an all-star selector such as
.B "set -as terminal-features '*:sixel'"
in the configuration file. RMUX then forwards the sequence to its outer handle.
Both routes are verified as configuration routes only: whether ConPTY and the
terminal emulator carry the sequence through and render it is not measured by
the project, so end-to-end delivery and display on native Windows remain
unverified. Windows also contributes its own context features late in
resolution, so a
.B terminal-overrides
removal is not guaranteed to win there.
Setting
.B RMUX_CONPTY_NO_PASSTHROUGH=1
selects the system console and disables the historical inbox paste compatibility
bit, for diagnosis. OSC query/reply forwarding requires matching Microsoft
.B conpty.dll
and
.B OpenConsole.exe
files (version 1.24.2607.10001 or later) beside the executable hosting the pane,
normally rmux-daemon.exe. These optional files are not included by the current
packaging scripts. OS build numbers alone do not establish OSC support.
.PP
SIXEL is detected automatically for foot, mintty, mlterm, and WezTerm. Kitty
graphics is detected automatically for Kitty, Ghostty, and WezTerm. Detection
reads
.B TERM
and
.BR TERM_PROGRAM ;
any other terminal needs the explicit
.B terminal-features
entry or the
.B -T
flag above.
.PP
Only the attached session's active pane forwards graphics.
.B all
is accepted for tmux configuration compatibility but currently behaves like
.BR on .
Forwarding is suspended while the pane is in copy mode or another pane mode.
Graphics are forwarded live: they are not part of the text grid, are not
replayed on a later attach, and are not repainted when the pane is redrawn.
.SH JSON OUTPUT AND ERRORS
For commands that accept
.BR --json ,
results are JSON on stdout.
Command errors (such as invalid arguments, unresolved targets or transport
failures) use a nonzero status, a plain-text diagnostic on stderr and empty
stdout. They do not emit partial JSON or a new JSON error object.
.PP
Existing version 1 result reports for unsuccessful outcomes remain JSON on
stdout with a nonzero status: wait-pane timeout or output lag, unmet
expect-pane assertions, and collect-pane-output lag. Check both the exit
status and the command's documented result fields.
.SH ENVIRONMENT
.TP
.B RMUX_DISABLE_TINY_CLI
When set to a non-empty value, disables the packaged tiny CLI fast path and
routes commands through the full helper binary.
.TP
.B RMUX_DISABLE_CLI_TARGET_ACTIONS
When set to a non-empty value, disables CLI target-action fast paths and routes
those commands through the full helper path.
.TP
.B RMUX_FULL_BINARY_PATH
Overrides the private full helper binary used by packaged tiny CLI builds.
This is intended for packaging tests and recovery diagnostics.
.TP
.B RMUX_TINY_TRACE
When set to a non-empty value, emits tiny CLI routing diagnostics to standard
error. This is intended for packaging and release-smoke diagnostics.
.SH NOTES
.B display-popup
keeps background pane updates off the attached client's screen until the popup
closes. Pane output continues to be captured, and closing the popup restores
the latest pane contents. Status updates that overlap a popup or menu are
held back while it is open; status bars outside the overlay continue updating.
.PP
.B link-window
and
.B unlink-window
ship in the public CLI surface with tmux-style shared window links across
sessions, including linked-window format variables and hook context.
.PP
.B web-share
local mode opens
.B https://share.rmux.io/
against
.B ws://127.0.0.1:<port>/share .
The
.B -t
target may be a pane or a session name. Pane targets share one pane. Session
targets mirror the active window layout and can switch windows through the web
client.
By default both URLs are minted: a private operator URL and a spectator URL for
sharing. Pass
.B --operator-only
or
.B --spectator-only
to restrict the share to one role. Operator session shares can use rmux prefix
commands from the browser.
Treat the private operator URL as equivalent to a local attached rmux client:
an authenticated operator is trusted to control the targeted session.
It requires a browser that allows public-origin access to loopback
WS connections. Chromium-based browsers may block this under Local Network
Access; allow Local Network access for
.B share.rmux.io .
Pass
.B --tunnel-url
to use a bring-your-own public endpoint, pass
.B --tunnel-provider NAME
to spawn a configured tunnel preset, and pass
.B --frontend-url
to use a self-hosted frontend.
The out-of-band pairing code printed by
.B web-share
is required by default before a browser can authenticate. Pass
.B --no-pin
to disable it. Shares do not expire unless
.B --ttl
or
.B --expires-at
is supplied. Pass
.B --ttl SECONDS
or
.B --expires-at RFC3339
to set the share expiry. Expiry revokes the share and closes browser clients;
it does not stop the rmux daemon. Pass
.B --kill-session-on-expire
only on a session share when the target session should be destroyed at expiry.
Pass
.B --max-spectators COUNT
to cap concurrent spectators, and
.B --max-operators COUNT
to cap concurrent operators. Spectator caps require a spectator URL; operator
caps require an operator URL.
Pass
.B --no-navbar
or
.B --no-disclaimer
to generate a minimal spectator URL.
The live connected browser count is shown in the navbar by default. Pass
.B --hide-viewers
to hide it.
Use
.B web-share stop SHARE-ID
or
.B web-share disconnect SHARE-ID
to revoke one active browser share and close its clients. These commands do not
stop the rmux daemon or the web-share listener.
Pass
.B start-server --web-port PORT --frontend-url URL
to configure the local web listener and the default frontend URL; port 0 is
rejected.
In-app webviews are not guaranteed.

.PP
"##;
