# Benchmark campaign artifacts

Speed is not qualified for RMUX 0.11. Publication of measured results is
**pending**. This source document contains no measured table or speed claim.

Generate a campaign from real JSON measurements only. Keep the generated
Markdown, CSVs and source JSON outside the source checkout. Bind the campaign to its measured source SHA and
attach it as a campaign artifact. Its immutable publication URL is pending;
no future URL or measurement is supplied here.

The native campaign excludes WSL columns and comparisons. Raw diagnostic
evidence retains compatibility-layer measurements without promoting them to
native Windows qualification.

## Reproducing

Run the scripts locally on each OS, gather the JSON files, and generate the Markdown:

```bash
scripts/bench/run-unix.sh --out ../rmux-benchmarks/linux.json
scripts/bench/run-windows.ps1 -Out ../rmux-benchmarks/windows.json
scripts/bench/run-unix.sh --out ../rmux-benchmarks/macos.json
python3 scripts/bench/render.py ../rmux-benchmarks/*.json --native-only --output ../rmux-benchmarks/report.md --asset-dir ../rmux-benchmarks/assets
```

Each OS measures itself locally, so no number here includes SSH or remote-shell latency.

Note: Scripts run safely in isolated sessions and only kill their own test processes.

## Methodology

The `render.py` script writes campaign Markdown and linked CSVs outside the source content, based on the JSON results. Attach them with the original JSON evidence to the campaign; publication URL pending until that campaign exists.

Each JSON artifact records the commit, platform, tool versions and every p50/p95 sample behind a cell.

`-`: Operation is unavailable or not comparable. Missing adapters and failed samples are omitted, never estimated.

Zellij and GNU screen: results only show exact equivalents or close approximations.

The campaign command uses `--native-only`: compatibility-layer columns and ratios are omitted. Windows comparators need an explicit native environment; RMUX is the native Windows collector. Without a native comparator, no ratio is displayed. The original JSON and raw CSV evidence retain all measurements. Without `--native-only`, the diagnostic renderer labels compatibility-layer tools; that output is not the publication campaign.

`≈ same speed` spans 0.80x to 1.25x, the spread repeated samples actually show on these hosts; a narrower claim would publish host noise as a result.

Generated at `pending` from commit `pending`.
