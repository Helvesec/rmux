# RMUX publication runbook

This procedure guides maintainers through candidate rehearsal, release
publication and recovery. Use the approved release label, source SHA and
channels for the version being released. The local Cargo rehearsal does not
certify release identities; that requires a qualified RC rehearsal and its
identity evidence.

Release references use `vMAJOR.MINOR.PATCH` or `vMAJOR.MINOR.PATCH-rc.N`.
Numeric components have no leading zeros, and RC numbering starts at one.
The complete `v1.*` line is supported, with no minor-version ceiling. Other
prerelease labels and build metadata are outside this release contract. The
package version remains `MAJOR.MINOR.PATCH`, including during an RC rehearsal.
The signed-tag message validator and local Cargo rehearsal enforce this same
identity syntax before signature verification or Cargo execution.

## Publication prerequisites

Before publication, the release manager must approve the release label, source
history route, guaranteed channels and publication dossier. An explicit written
authorization must cover each remote dispatch or publication. Resolve the
crates.io first-publication and Trusted Publishing policy, or defer that channel
before publishing any crate. External submissions are not guaranteed availability.

Do not infer approval from a green dry-run. If a channel is deferred, obtain a
supported execution plan before dispatching a workflow that still requires it;
the current receipt route includes the crates.io OIDC preflight.

## Bind the rehearsal to the candidate

Retain the candidate manifest, intent, resolution, verified artifact list, policy
audit, and their attestations in a private directory under `Projects`. Record:

- repository `Helvesec/rmux`, repository ID `1239918790`;
- `source_git_sha`, `release_intent_id`, `planned_release_ref`, release kind,
  `package_version` (RC references use the stable package version);
- fast run, candidate run, manifest run and artifact IDs, attempt **1**, artifact
  digests, manifest SHA256, policy SHA256 and contract blob OID;
- each canonical platform's build record, toolchain, archive hash and provenance;
- approved public SSH signing principal and fingerprint from
  `.github/release/release-signers.json`, release App ID `4339867`, downstream
  App identity and approved environments/workflows. Use public metadata only.

During the RC rehearsal, compare these fields with the identities configured for
the real publication route. Keep the same candidate bytes, source SHA, policy
and public identities through the rehearsal; never replace missing evidence with a local
signer or a synthetic run ID. The existing promotion simulation uses a synthetic
tag and principal, so its success alone does not prove the same real identities.
Actual signer, App, environment and OIDC authority checks remain pending until
the release manager authorizes the hosted rehearsal. Do not extract private keys
or tokens to make a local rehearsal resemble production.

From a clean checkout at the candidate source SHA, with the recorded artefacts:

```bash
scripts/check-release-versions.sh
python3 scripts/release/verify-candidate-artifacts.py verify-downloaded \
  --candidate-run-id "$candidate_run_id" --expected-source-sha "$source_sha" \
  --fast-run-id "$fast_run_id" --release-intent-id "$release_intent_id" \
  --planned-release-ref "$release_ref" --release-kind "$release_kind" \
  --resolution "$evidence/candidate-resolution.json" \
  --downloads-dir "$evidence/canonical" \
  --output "$evidence/verified-candidate-artifacts.json"
bash scripts/release/verify-candidate-attestations.sh \
  "$evidence/canonical" "$source_sha" "$gh_binary"
```

Use Bash 4+ for scripts using `mapfile`; `gh_binary` is an absolute verifier path.
Attestation verification can read remote public verification material; run it
only in the authorized evidence phase. Preserve originals and use an absent
output path for each new verification. A changed or expired manifest requires a
new qualified candidate and a new rehearsal, never a changed hash expectation.

## Local Cargo rehearsal: no publication authority

Run this phase on a clean local HEAD and repeat it on the last qualified RC. For
the local run, explicitly record the local version/reference. Get the source SHA
from that checkout and the package version from its manifest, or from the
candidate's `package_version` during the RC rehearsal.

Use an **absolute Cargo executable from the chosen Rust toolchain**, next to its
`rustc` and `rustdoc`, rather than a rustup proxy. `rustup which cargo` identifies
that executable without a registry credential. The output directory must be
absent, private, and durable. Example, after setting the recorded values:

```bash
python3 scripts/release/publication-dry-run.py \
  --source-dir "$source_dir" --source-sha "$source_sha" \
  --release-ref "$release_ref" --version "$package_version" \
  --cargo "$cargo_binary" \
  --target-dir "$evidence/cargo-rehearsal"
```

For Windows, use Python and the absolute toolchain `cargo.exe`, next to
`rustc.exe` and `rustdoc.exe`, with the same named arguments. Establish the native
compiler environment before running; do not treat an unplayed platform as green.

For an inert contract check without Cargo, signing keys or publication access:

```bash
python3 -B -m unittest discover -s scripts/release -p 'test_publication*.py'
```

These fixtures cover stable and RC references across the major-one line,
out-of-contract references, signature validation boundaries and topological
publication arguments. Cryptographic verification and Cargo execution are
substituted in the fixtures. Record this as an inert check; the actual Cargo
rehearsal, native gates and rehearsal with production identities remain unplayed
until each has its own execution evidence.

The harness reuses `canonical-crate-set.py` for the publishable crate population
and dependency graph, including build and target dependencies. A
dev-dependency that declares a version is also an edge: Cargo keeps it in the
packaged manifest and resolves it on crates.io, so its crate is published first.
Only a path-only dev-dependency is excluded, because Cargo strips it. The
candidate crate set then checks every packaged manifest: a workspace crate that
it names must precede it in the order. It reuses `publish-crate-set.py` for clean exact-SHA checkout validation
and the Cargo publication arguments. Every selected crate is invoked once, in
topological order, as `cargo publish --dry-run --locked --package NAME`. It has
no live publication option and never calls the publisher's registry writer.

Cargo receives fresh HOME/USERPROFILE and CARGO_HOME directories, private target
and temporary directories, no inherited tokens, SSH agent or Rust wrappers, and
disabled global Git config, credential helpers, SSH and hooks. This prevents
use of personal registry credentials/configuration. The harness refuses
`.cargo/config` and `.cargo/config.toml` in the source checkout and every parent
before Cargo runs, since Cargo searches these paths even with a fresh HOME.
It is not a sandbox for
untrusted build scripts. Run only the reviewed RMUX candidate. The registry may
be read to resolve dependencies. No registry, GitHub, tag, or release write is
part of this phase.

Retain `publication-dry-run.json` and `logs/*.log`. The JSON binds source SHA,
reference, package version, Cargo path, graph, command and exit code per crate.
An ordinary crate failure is recorded and the remaining crates are attempted;
the harness exits nonzero after the complete population. An infrastructure or
space failure marks the report `interrupted` and leaves evidence in place.
Recheck source cleanliness after Cargo verification.

Space checks occur before and after each crate: stop below **35 GiB free** or
above **6 GiB** in the private rehearsal target. Monitor disk while a Cargo
command is running and interrupt if either bound is crossed; never delete
another target or credentials to continue. Full build admission requires at
least 24 GiB available after the host's reserved capacity.

Unpublished internal dependencies can fail verification even in topological
order: a dry-run does not upload its predecessor. Keep the actual Cargo error
and failing crate as a causal observation. Do not publish a predecessor, add
`--no-verify`, edit dependency expectations, or claim the population passed.
Resolve first-publication policy before real publication; keep the local
observation separate from the RC rehearsal acceptance.

## Candidate rehearsal

1. Select the last fully qualified RC; compare all recorded identities above.
   Verify the canonical archives, signatures, checksums, notes and migration
   guide. Native gates and installation evidence must concern those bytes.
2. Play the local Cargo command above against the RC's exact checkout, with a
   new absent target. Retain every result, including unresolved dependencies.
3. After explicit authorization for the hosted nonpublishing simulation, use
   `.github/workflows/release-promotion-simulation.yml` on `main`, entering the
   exact candidate/manifest IDs, digests, intent, reference and policy fields
   from the retained evidence. Give it a unique simulation ID and retain its
   exact run identity and simulation artifact. It does not create the real tag.
4. Compare the real route's public signer/App/workflow/environment identities
   with the rehearsal record. List any authority checks the simulation cannot
   perform and have them resolved in the approved rehearsal. A synthetic tag
   receipt does not replace that evidence or validate production secrets.
5. Record date, candidate identifiers, commands, observations and identity
   comparison. Require independent review and release manager acceptance.
   Until that rehearsal exists, its acceptance remains pending. Renew invalid
   secrets only through the maintainer's authorized credential process.

## Release publication

These steps perform remote writes. Execute them only after written approval of
the final label, history route, guaranteed channels and publication dossier.
Verify the frozen source SHA and the final stable candidate; repeat the identity
and artifact checks if the final stable candidate differs from the rehearsed RC.
The release manager validates final notes, migration and the list of deferrals
before publication. Do not promote an RC manifest as a different stable reference.

If the canonical set introduces a new crate name, agree on first-publication
authority before approving any upload. Trusted Publishing cannot create the
name. Never publish `rmux-bootstrap` ahead of its workspace dependencies.

For authorized onboarding, wait for the stable publication receipt, then use
the receipt-bound source, Cargo toolchain and canonical crate set. Follow its
`publish_order` through the new name: dry-run each package, compare its
generated archive hash against the manifest, then publish and redownload it
before advancing. Skip an existing version only if its bytes match. Stop on
any mismatch. Configure Trusted Publishing for the new name, revoke the
onboarding credential and resume the regular writer; it verifies existing
bytes and completes the remaining packages. Never run two writers concurrently.
If authority, receipt or hashes are missing, defer the whole channel before
the first upload. Local rehearsal does not grant registry authority.

Use a clean `main` checkout at the final candidate SHA. First inspect the
promotion plan (it reads authenticated GitHub metadata, so it is **not** the
credential-free local Cargo rehearsal):

```bash
bash scripts/release/dispatch-release-promotion.sh --plan \
  "$candidate_run_id" "$manifest_run_id" "$source_sha" \
  "$release_intent_id" "$release_ref" stable
```

Compare its complete inputs with the final dossier. Then, only with the written
publication approval, execute the same command without `--plan`:

```bash
bash scripts/release/dispatch-release-promotion.sh \
  "$candidate_run_id" "$manifest_run_id" "$source_sha" \
  "$release_intent_id" "$release_ref" stable
```

Retain the returned `tag_authoring_run_id` and URL, not the most recent run.
The tag-authoring workflow verifies the candidate and creates an annotated SSH
tag via the release App; it dispatches the exact-tag promoter on that tag. Watch
those exact runs and approve only the expected protected-environment requests.
The promoter verifies candidate, signed tag, provenance, policy audit and
authorization before publishing GitHub Release bytes and creating the immutable
receipt. The receipt gates downstream publication. No manual force-push or
direct ad hoc Cargo publication replaces this chain.

For crates.io, `publish-crate-set.py` validates the canonical package-set tar,
checks source SHA and cleanliness, and processes its dependency order. For a
missing version it dry-runs, compares Cargo package bytes with canonical bytes,
publishes, then verifies downloaded registry bytes. An already published exact
version is a no-op; different bytes are an error. Other channels use their
receipt-gated writers and channel results; external moderation is still pending
until independently visible.

## Verify delivery and handle partial failure

Retain the immutable Release receipt, authorization and attestation, then the
downstream summary and channel evidence. Re-download final archives and compare
their SHA256 with the qualified canonical bytes; verify Sigstore provenance and
the signed checksum policy in `SECURITY.md`. Check installer behavior from those
archives on each supported native OS. Confirm exact package versions and
downloaded crate bytes, repository indexes and tap payloads for each guaranteed
channel. Record submissions separately from public availability.

On failure, preserve the exact failed run ID, workflow, SHA, reference, receipt
and channel results. Dispatch `release-receipt.yml` on healthy `main`, setting the original
`failed_receipt_run_id`, for receipt recovery and `release-channel-retry.yml` for one failed channel, with
their required evidence and independent authorization. The corresponding
`receipt-recovery.py` and `prepare-channel-retry.py` reject mismatched identities;
do not rerun the whole publisher, move the tag, overwrite package bytes or
rebuild assets to conceal a partial result. A recovery runs on healthy `main`
control code but preserves the original release source and exact canonical bytes.

Delivery is accepted only with verified artefacts, receipts and public channel
evidence, plus the published limitations and deferrals. Local success, workflow
dispatch success, an external submission, or missing output is not delivery.
