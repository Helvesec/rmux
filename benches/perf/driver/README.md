# Darwin driver qualification (R5-A)

Run the functional canaries on native macOS, with Python 3.11+:

```sh
python3 -B benches/perf/driver/test_darwin_driver.py
python3 -B benches/perf/driver/test_cleanup.py
python3 -B benches/perf/driver/smoke_darwin.py \
  --rmux /absolute/path/to/rmux \
  --tmux /absolute/path/to/tmux-3.7b \
  --out /private/evidence/native-adapter-smoke.json
```

These commands perform no build. The second command runs six existing adapters
once per tool and deliberately discards their elapsed values. Reference/debug
binaries are permitted for this functional smoke. Its hashes identify those
fixtures, without attesting their build source or qualifying release performance.
It uses an empty configuration with `/bin/sh`, private temporary socket roots
and bounded socket names, and observes a native exit event for every daemon it
creates. No shared daemon is stopped.
Use `--cleanup-only` on the smoke command to check just cold/warm startup,
ordinary cleanup, and the operations that themselves stop a session/server.

On Darwin, `bench_unix.py` automatically uses `darwin_driver.py` for quiet,
captured and timed process launches. The same driver handles every selected
tool. Preparation, environment, executable lookup and output descriptors are
prepared outside timing; the window covers `posix_spawn` through collection of
the owned child by `waitpid`. `kqueue` waits for process exit without a sleep or
WNOHANG polling loop. Immediate child exit before event registration is handled
by collecting that same child. Spawn errors, nonzero exits and timeouts retain
their cause and output. Timeout or interruption kills the launch's own process
group and collects the direct child. A nonzero exit also cleans that group:
`waitid(WNOWAIT)` observes the status before `waitpid` releases the PID, avoiding
a signal to a recycled group. The adapter separately cleans up its named
daemon; a deliberately detached descendant is not a child of the driver.
Darwin's zombie-only `killpg` EPERM is accepted only when `waitid(WNOWAIT)`
established exit and `proc_listpgrppids` identifies exactly that zombie leader;
other permission errors remain failures.

The driver requires single-threaded process launching and default SIGCHLD
handling, so an external reaper cannot steal a child before cleanup. It closes
inheritable nonstandard descriptors in the child; Darwin kqueues and other
CLOEXEC descriptors remain private. The canaries check parent FD stability,
reaping, a descriptor placed above 255, an unrelated child, large stdout/stderr,
event failure, failed exec, nonzero status and timeout. They also observe the
old Python timeout polling and reject polling in the actual new driver.

Readiness uses a new mode-0600 FIFO in a mode-0700 directory and a fresh
single-use challenge. The real validator rejects stale, incorrect, extra and
missing frames, partial-frame timeout, cached stdout and a second wait. A native
event wait must remain blocked until the producer writes. No elapsed-time delay
or captured screen output is treated as a readiness acknowledgement.

The ordinary shell setup sends the acknowledgement command to the pane and
waits for that shell to execute it. This extra setup command appears in the
pane history for both products. The generated-output fixtures acknowledge
their final write before their existing sleep; this proves producer progress,
not that the multiplexer has already rendered or retained every output byte.
Cold/warm new-session commands keep their measured arguments, with the shell
acknowledgement checked after their measured interval. Setup acknowledgements
for other operations occur before timing. No operation is relabelled as
input-to-render or full-scroll-back verification by this change.

`population.json` freezes the 33 operation IDs, the 10 public IDs and the measured
argument expressions from source commit
`b592010d0f41f8a6deba30a526c84a668ff6c6e4`. The test compares these with the actual
adapter and renderer. The generated-output setup commands retain their
payload; the two measured capture adapters add the readiness signal outside
their timed command. Linux's existing process/readiness path is unchanged.

Each collector attempt is retained, including successful attempts preceding a
failure. A Darwin failure leaves `complete=false` and exits nonzero without
retrying or silently continuing to another sample. Batched list adapters clean
up their fixture immediately on failure. A missing explicitly selected native
tmux is an error. Missing optional adapters on other comparator tools keep their
existing documented availability rules.

Darwin cleanup obtains the daemon PID from the private fixture endpoint and
registers `NOTE_EXIT` outside timing. Success requires observed daemon exit;
`kill-server` returning zero alone is insufficient. Only the explicit cold
pre-cleanup accepts the exact missing-socket diagnostic without a daemon PID.
Nonzero cleanup status and output are retained as `FixtureCleanupError`.
If an operation already failed, its original exception stays primary and the
collector records a separate `cleanup_error`. No command or sample is retried.
After a cleanup failure, the adapter may signal only that known fixture PID
and observe its exit to dispose of the fixture; the sample still fails.
The synthetic regressions exercise the real driver, FIFO validator, adapter,
and measure path, including a living daemon after both nonzero and false-zero
cleanup, double failure, expected absence, batch setup failure, and FD stability.
Batch tests shorten only the untimed multi-window setup. These observations do
not discover or attest arbitrary helper processes (R6 remains separate).

This is the R5-A driver/readiness sub-contract only. It does not qualify the
33 scenarios' complete functional postconditions, thermal symmetry, paired
ordering, p99, confidence intervals, campaign population, release provenance,
transient-helper attestation (R6), or a performance baseline. Those obligations
must be completed before a global `0.harness` certificate or speed claim.
