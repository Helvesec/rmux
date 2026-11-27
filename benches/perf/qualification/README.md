# R5-B population and attempt accounting

`scripts/bench/bench_unix.py` uses this frozen population of 33 operation IDs
and 10 public IDs. The screen population remains eight operations and the
zellij population ten. Unsupported combinations have no expected attempts;
unavailable optional tools remain explicit in `unavailable_tools`.

The existing CLI, iteration count, operation recipes, setup, measured arguments,
timers and cleanup implementations are preserved. `qualification.verify_source`
checks the ID/function mapping and the frozen AST of each existing recipe and
helper. Five batch factories register their existing cleanup with the sampler
so an interrupted pair can retire the other tool's open fixture. The operation
bodies themselves remain unchanged, and cancellation never executes another
measurement.

Before sampling, the artifact records a fresh run ID, selected population and
complete expected schedule. Each identity includes the run, operation, tool,
pair, position within the pair, global sequence and attempt number one. For
each ID, RMUX/tmux order alternates AB/BA. The first order also alternates by
the ID's position in the full population, even when a subset is selected.
Even iteration counts are exactly balanced; odd counts differ by one. No extra
iteration is introduced to balance the default count of 15. Other tools keep
their original operation mappings and follow the pair in deterministic order.

The collector atomically writes `complete=false` and a running attempt before
calling an operation. It checkpoints the returned value or exception, including
nonzero status, timeout and any additional cleanup cause. Interrupted processes
leave readable intent and prior results; a hard exit can leave a running attempt,
which the gate rejects. These are process-interruption guarantees, not claims
of persistence through a host power loss.

There is one attempt per planned identity and no automatic retry. On the first
sampling error, remaining identities stay in `unattempted`, cancellation runs
outside all measured operations, the partial artifact is preserved and the
collector raises. Completion requires the exact expected set, order and
cardinality, one successful finite result for every identity, matching metric
samples, and no sampling or cancellation error. A saved artifact can be checked
again with `qualification.validate_artifact`; its `complete` flag is not trusted.
Explicit `sampling_finished` and `cleanup_finished` receipts are required too:
a partial checkpoint with every sample still fails until finalization completes.
Success keeps each recipe's existing semantics, including screen's intentionally
unchecked listing status. Propagated child errors are retained; this accounting
gate does not add the real postconditions still missing from those recipes.
Full JSON checkpoints run outside operation timing. Their workload effects must
still be considered when a later performance campaign is qualified.

Run the finite synthetic controls on a new evidence directory:

```sh
python3 -B benches/perf/qualification/test_accounting.py /private/new-evidence
```

The controls exercise the real collector with fake operations, own finite Python
children and injected outcomes. They preserve a successful 168-attempt artifact
(two iterations across all supported tool/ID combinations) and rejected artifacts
for nonzero status, timeout, partial writes, interruption, missing results,
duplicate/unknown identities, retry, order and cancellation faults. The portable
sampler branch is exercised without launching a product.

These outputs are synthetic and provide no performance evidence. R5-B does not
prove real postconditions for all 33 recipes, installed release/helper identity,
R6 observation, thermal conditions, p99/paired confidence intervals or a complete
performance campaign. Existing labels retain their original meaning: an
`attached` split recipe alone does not prove an attached terminal client, and
the inherited `round_trip` label does not establish application delivery latency.
