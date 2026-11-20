# Generated options bank

`options::tests::registry_metadata::generated_options_bank_inventory` exports
every entry of `registry::registry()` and the independent, authenticated tmux
inventory from the existing `frozen.rs` scanner. Select the ignored test with
`RMUX_REQUIRE_FROZEN_TMUX=1`, `RMUX_FROZEN_OPTIONS_TABLE` and a **fresh**
`RMUX_OPTIONS_BANK_OUTPUT` path. The exporter includes the actual types, aliases,
scope/show masks, defaults, array flags, separators and declared effect masks.
It does not equate a declared effect mask with proof of a real consumer.

`options_bank_contract.json` declares the common axes, the historical Linux
message recipe and a supported global-default recipe by stable option ID. It contains no maintained option-name inventory.
For an execution, copy this template to a private frozen contract, add the
registry/source/tool/binary file pins and `native_source` commit, and hash the
canonical historical JSON recipe into `recipe_sha256`. For the default
adapter, also hash `default_recipe` into `default_recipe_sha256` and pin
`default_driver`, `python`, `default_oracle_baseline` and `source_path`. `artifacts` pins the exporter,
generated registry, oracle source, template, generator, adapter and tools.

Run `python3 -B tests/options_bank.py generate REGISTRY CONTRACT OUTPUT` to
materialize the exact ordered cell IDs in `cells.txt.gz`. Every cell inherits
`FAIL_MISSING` in `SCOREBOARD.json` unless an explicit two-product contribution
is present. This compact representation does not omit missing cells: the gate
regenerates and compares every ID, order, count and digest. `check` exits 1
while coverage is incomplete, even if its structural checks pass. `canaries`
exercises renaming, cell removal/duplication, missing oracle calls/artifacts,
and altered identity/symbol/bytes. These are pure controls, not native results.

The Linux adapter is `options_bank_linux.py run CONTRACT oracle|candidate OUT`.
Freeze inputs before either run. Close and pin the oracle's `RESULT.json` in
`ORACLE-FROZEN.json` before admitting the candidate. The current adapter executes only the global-default recipe. Both runs use one
private foreground server and one inert pane helper, without an attached client;
retained pidfds and birth identities establish closure. No shared terminal is used. The
scoreboard contribution maps a cell to the two immutable receipt file pins.

The archived first recipe preloaded two messages, set `message-limit=1`, then observed
value and retained history. It is not replayed by the current adapter. tmux logs commands themselves; that observable
interference is retained as a failure, not removed to manufacture parity.
Raw values, logs and an additional append observation distinguish the causes.

This is an initial bank, not closure of the whole options criterion. Detailed
argument-position and invalid-scope recipes, other real consumers, and Mac /
Windows adapters remain explicitly unimplemented. Cross-product obligations
are conservatively retained until applicability is validated. Existing accepted
per-option scripts are unchanged; their historical results are not replayed or
reattributed to the new candidate.

The common gate now validates schema and unique nonempty ID components before
iteration. Duplicate axes, selectors across scopes, or ambiguous separators fail
before the first cell; no in-memory set of all matrix IDs is required.

The frozen contract also carries receipt_binding, mapping the existing recipe
steps and full cell context to ordered roles and exact command arguments.
$option is resolved from the authenticated generated registry; $client comes
from the recorded attached client. Every command must use the recorded private
socket and executable, with the exact flags/targets, unique ordered role/index,
valid timezone-aware start/end and nonoverlapping chronology. Markers must equal
the original recipe. This binding adds no new effect expectation and does not
change recipe or oracle output. Old contracts without it fail closed; do not
rewrite old contracts or receipts. New frozen contracts must include it.

Synthetic canaries use explicitly labelled files, literal independent command
fixtures and an opt-in pure validation path. The default native scoreboard path
rejects those labelled fixtures. Common canaries include the four R1-060 F1
counterexamples and duplicate-axis F2, with neighboring target/flag/time checks.
A synthetic positive is only a validator control, never a native PASS.

The historical receipt_binding schema1 supports only message-limit-preloaded-v1. Its operation templates
and full cell context must match that implemented semantic profile. An edited
binding cannot relabel the same CLI receipt as another route/option/flag family.
Additional profiles require their own concrete frozen consumer observations.

## Global default observations

The common bank also supports global-window-default-v1: a Linux by-name CLI
read, before input, without an attached TTY, isolated, successful, operation
default. The declaration identifies options through generated registry IDs.
One unfiltered window listing, a named read and a value-only read are bound to
each cell. The whole command sequence, setup, process closure, source, binary,
driver and output bytes are checked. The freshly qualified oracle must match
an authenticated archived oracle baseline before the candidate can run.
This is evidence of the default and its serialization, not a rendered consumer
effect or evidence for other routes.

options_bank_linux.py is the common Linux adapter for this recipe. It uses a
private foreground daemon and one inert pane, keeps owned process identities
and pidfds, and closes the oracle before candidate execution. Helper exit
numbers remain unknown when reaped by the daemon; intent markers are not waits.
No FFI, debugger, instrumentation, tracing or shared daemon is involved.

default_canaries mutates copies of exact receipts and labels them synthetic.
Such copies cannot receive native credit. Existing F1/F2 gates and population
rules remain active. The test-only registry exporter is selected explicitly and
requires an authenticated tmux source; it creates a fresh output file.

### message-limit observation conflict remains

The original message-limit-preloaded-v1 postcondition is unchanged. A printed
value of 1 alone is insufficient, as are both retained preload payloads, a later
append payload or the observer command. Their negative controls stay in the
common gate. The Linux adapter deliberately does not replay this recipe: pinned
tmux3.7b trims its global journal on add and logs each post-startup command before
execution. The archived original FAIL remains attached to its original source.
A pilot classification is needed for this contract conflict; any shared runtime
change belongs to L2. No semantic lines may be discarded to manufacture PASS.


Receipt consistency (L6-035): the common default reader checks closure facts as
well as its PASS label: empty errors, Linux ENOENT/ECONNREFUSED, retained process
births, pidfd exit, absence after exit and recorded wait status. A daemon wait
is numeric; a helper wait may be unknown (null) after its parent reaps it.
Neither a pidfd event nor planned helper exit synthesizes a numeric wait.
The exact hashed public-inventory stdout must equal the public fields and
match the recorded endpoint and daemon/helper identities; @0 and %0 remain
legal. Every CLI PID is joined to its recorded birth and collector parent.
These checks run identically for actual receipts and explicitly admitted
synthetic controls. Default canaries retain contradictory closures, rehashed
forged inventory and wrong CLI birth as negative controls. Archived L6-034
observations keep their original native source; reader changes do not replay
or reattribute them. F5 and the original raw ordering failure remain open.


Registry field and default profile (L6-036)
-----------------------------------------
The Rust export includes numeric lower/upper bounds and observations from the
real OptionStore default, alias, scope and show consumers for every canonical
entry. `registry-oracle` parses the authenticated tmux C table, aliases, choice
arrays and literal macros independently; `registry-check` compares every entry
and field. Its platform bindings must be qualified against the pinned source,
build context and actual binary. Missing or duplicate entries fail the gate.

The `registry-global-default-v1` recipe in the same Linux adapter reads all
registry entries at their natural global root, aliases and four full scope
listings. Its fixed isolated environment is recorded. The three setup options
(exit-empty, status, automatic-rename) are queried but cannot receive default
cell credit. All other cells require exact oracle/candidate named and value
bytes, source/binary/recipe identities and the common C1/C2 closure checks.
The profile never executes the message-limit retention recipe.

`registry_canaries` mutates the real Rust export and requires a new causal
failure for each corrupted field; existing baseline failures cannot satisfy a
canary. `registry_receipt_canaries` checks population, environment, independently
expected oracle values, aliases, default setup exclusions and the new profile.
Synthetic controls never earn native cell credit.

The raw field gate deliberately retains the empty default-shell sentinel versus
tmux's declared /bin/sh difference. Runtime derivation requires separately pinned
shell resolver/show consumer source plus a matching native environment. The
VISUAL/EDITOR startup branches and Windows status-right default need explicit
provenance; a Linux observation or an intentional-looking cfg is insufficient.
Full window listing order remains a distinct compatibility obligation.


L6-037 corrections
------------------
Named inherited empty arrays follow tmux's named print path and omit the star;
unfiltered inherited arrays retain it. The v2 registry recipe adds window/pane
named and value-only reads plus a full inherited window listing. Both paths are
checked against literal independently qualified tmux behavior.

Platform bindings now require schema-valid provenance for every symbol. The
versioned Linux context pins immutable evidence bytes in the checker itself:
compiler identity and reproduced preprocessor output, the qualified tmux release
archive, and the reviewed oracle receipt/contract. Integer/path constants are
derived from those macros; the terminal build default comes from the actual
oracle observation. Every C input must match the authenticated archive member.
Caller-supplied hashes, including self-consistent hashes of forged files, cannot
replace these anchors. A new platform/context requires explicit qualification;
this context does not establish missing conditional branches or whole coverage.

Stable option IDs are derived from the committed canonical table and wire enum
in the checker's checkout. The exhaustive parser authenticates working bytes
against Git and checks the entire wire domain, independently of the tested JSON
row. There is no maintained name-to-ID list. Coherent ID/symbol substitutions and
typed-to-named conversions now add causal identity failures. Existing C1/C2 and
source-bound historical observations retain their source attribution; historical
registry receipts need an explicitly qualified provenance supplement for replay
through this stricter reader, without altering their original contracts.
