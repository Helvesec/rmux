# RMUX Documentation

Versioned hosted guides for RMUX 0.11 are **pending**. The [README documentation
index](../README.md#documentation) links the immutable installer and configuration
source snapshots. Hosted guides must publish the same source SHA in their URL;
unversioned live pages do not qualify the candidate.

Release downloads remain pending independently of hosted guides. The `release`
entry in [the documentation allowlist](../.github/release/docs-allowlist.json)
keeps its URL and evidence fields null while pending. A `published` entry must
bind the release source commit and an archived publication receipt through
`source_ref` and `receipt_ref:receipt_path`; its version, source and annotated
tag must agree. This offline consistency check consumes the existing publication
receipt format; publication and attestation verification remain separate release
steps. A passing pending check establishes no external availability.

Repository docs:

- [Architecture](ARCHITECTURE.md)
- [Web Share](web-share.md) — experimental; beyond-loopback qualification pending
- [RMUX SDK](scripting-sdk.md)
- [Roadmap](ROADMAP.md)
- [Security assurance case](assurance-case.md)
- [tmux compatibility decisions](tmux-compat-decisions.md)
- [Human-friendly starter config](human-friendly-config.md)
- [Benchmark campaign artifact policy](benchmarks.md) — speed qualification pending
