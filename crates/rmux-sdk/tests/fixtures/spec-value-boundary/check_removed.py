"""Check exact diagnostics for the eight removed impls (From and Into each).

Point at SDK/proto rlibs from an offline locked SDK build. Use --expect available
on the pre-spec-value-cutoff base as a fail-before control. --private also checks
the retained process helper (E0624) and three removed helpers (E0599).
"""
import argparse
import json
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
for option in ("rustc", "sdk", "proto", "deps", "out-dir"):
    parser.add_argument("--" + option, required=True, type=Path)
parser.add_argument("--expect", choices=("available", "unavailable"), default="unavailable")
parser.add_argument("--private", action="store_true")
args = parser.parse_args()
args.out_dir.mkdir(parents=True, exist_ok=True)
cases = []
for sdk, proto, reverse, borrowed in (
    ("ProcessCommandSpec", "ProcessCommand", False, False),
    ("ClientTerminalSpec", "ClientTerminalContext", True, False),
    ("SplitDirectionSpec", "SplitDirection", True, False),
    ("SplitTargetSpec", "SplitWindowTarget", True, True),
):
    sdk, proto = "rmux_sdk::" + sdk, "rmux_proto::" + proto
    directions = [(sdk, proto)]
    if reverse:
        directions.append((proto, sdk))
    if borrowed:
        directions.append(("&" + sdk, proto))
    for source, dest in directions:
        cases.append((source, f"<{dest} as From<{source}>>::from(value)", "E0277"))
        cases.append((source, f"Into::<{dest}>::into(value)", "E0277"))
    if args.private:
        cases.append((sdk, "value.into_proto()", "E0624" if sdk.endswith("ProcessCommandSpec") else "E0599"))
source = args.out_dir / "removed.rs"
source.write_text("\n".join(
    f"pub fn check_{index}(value: {ty}) {{ let _ = {expression}; }}"
    for index, (ty, expression, _) in enumerate(cases)
) + "\n" + "\n".join(
    f"pub fn retained_{sdk.lower()}(value: rmux_sdk::{sdk}) -> rmux_sdk::RmuxCommand {{ value.into() }}"
    for sdk in ("NewSessionSpec", "AttachSessionSpec", "SplitSpec", "RefreshClientSpec")
) + "\n")
command = [str(args.rustc), "--edition=2021", "--crate-type=lib", "--emit=metadata",
           "--error-format=json", str(source), "--out-dir", str(args.out_dir),
           "--extern", "rmux_sdk=" + str(args.sdk), "--extern", "rmux_proto=" + str(args.proto),
           "-L", "dependency=" + str(args.deps)]
result = subprocess.run(command, text=True, capture_output=True, check=False)
(args.out_dir / "diagnostics.jsonl").write_text(result.stderr)
errors = [item for line in result.stderr.splitlines()
          if (item := json.loads(line)).get("level") == "error" and item.get("code")]
if args.expect == "available":
    assert result.returncode == 0, result.stderr
else:
    assert result.returncode == 1, f"expected rustc exit 1, got {result.returncode}: {result.stderr}"
    assert len(errors) == len(cases), result.stderr
    for line, (_, expression, code) in enumerate(cases, 1):
        matching = [item for item in errors if any(
            span["is_primary"] and span["line_start"] == line for span in item["spans"])]
        assert len(matching) == 1 and matching[0]["code"]["code"] == code, (
            f"expected exactly {code} for {expression} at line {line}: {result.stderr}")
print(f"{len(cases)} expressions {args.expect}; rustc exit {result.returncode}")
