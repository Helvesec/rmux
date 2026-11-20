"""Compile each former public conversion and verify its precise rejection.

Use the same built rmux-sdk/rmux-proto artifacts and dependency directory as the
SDK package. --expect available is the fail-before control on the unchanged base.
The normal unavailable mode checks E0624 for methods and E0277 for From/Into,
on every individual expression, so missing crates/imports cannot pass the check.
"""

import argparse
import json
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
for option in ("rustc", "sdk", "proto", "deps", "out-dir"):
    parser.add_argument("--" + option, required=True, type=Path)
parser.add_argument("--expect", choices=("available", "unavailable"), default="unavailable")
args = parser.parse_args()
args.out_dir.mkdir(parents=True, exist_ok=True)
cases = []
for sdk, proto in (("WindowRef", "WindowTarget"), ("PaneRef", "PaneTarget"), ("TargetRef", "Target")):
    sdk, proto = "rmux_sdk::" + sdk, "rmux_proto::" + proto
    if not sdk.endswith("::TargetRef"):
        cases.append((sdk, "value.to_proto()", "E0624"))
    for source, dest in ((sdk, proto), ("&" + sdk, proto), (proto, sdk)):
        cases.append((source, f"<{dest} as From<{source}>>::from(value)", "E0277"))
        cases.append((source, f"Into::<{dest}>::into(value)", "E0277"))
source = args.out_dir / "removed.rs"
source.write_text("\n".join(
    f"fn check_{index}(value: {ty}) {{ let _ = {expression}; }}"
    for index, (ty, expression, _) in enumerate(cases)
) + "\n")
command = [str(args.rustc), "--edition=2021", "--crate-type=lib", "--emit=metadata",
           "--error-format=json", "-Adead_code", str(source), "--out-dir", str(args.out_dir),
           "--extern", "rmux_sdk=" + str(args.sdk), "--extern", "rmux_proto=" + str(args.proto),
           "-L", "dependency=" + str(args.deps)]
result = subprocess.run(command, text=True, capture_output=True, check=False)
(args.out_dir / "diagnostics.jsonl").write_text(result.stderr)
errors = [item for line in result.stderr.splitlines()
          if (item := json.loads(line)).get("level") == "error" and item.get("code")]
if args.expect == "available":
    assert result.returncode == 0, result.stderr
else:
    assert result.returncode != 0, "old public conversions still compile"
    assert all(item["code"]["code"] in {"E0624", "E0277"} for item in errors), result.stderr
    for line, (_, expression, code) in enumerate(cases, 1):
        assert any(item["code"]["code"] == code and any(
            span["is_primary"] and span["line_start"] == line for span in item["spans"]
        ) for item in errors), f"missing {code} for {expression} at line {line}: {result.stderr}"
print(f"{len(cases)} old conversion expressions {args.expect}; rustc exit {result.returncode}")
