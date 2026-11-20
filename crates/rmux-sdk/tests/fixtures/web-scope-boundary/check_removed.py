"""Check each removed Web Share exposure against locked build artifacts.

Use --expect available against ddfbc3ac (without --private) to prove the old
expressions compile. The SDK-only positive consumer has its own Cargo manifest.
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
cases = [
    ("v: &rmux_sdk::WebShareSummary", "let _: &rmux_proto::WebShareScope = &v.scope;", "E0308"),
    ("v: &rmux_sdk::WebShareSummary", "let _: Option<&rmux_proto::PaneTargetRef> = v.pane_target();", "E0308"),
    ("v: &rmux_sdk::WebShareHandle", "let _: &rmux_proto::WebShareScope = v.scope();", "E0308"),
    ("v: &rmux_sdk::WebShareHandle", "let _: Option<&rmux_proto::PaneTargetRef> = v.pane_target();", "E0308"),
    ("v: &rmux_sdk::WebShareLookup", "let _: &rmux_proto::WebShareScope = v.scope();", "E0308"),
    ("v: &rmux_sdk::WebShareLookup", "let _: Option<&rmux_proto::PaneTargetRef> = v.pane_target();", "E0308"),
    ("v: &mut rmux_sdk::WebShareSummary, scope: rmux_proto::WebShareScope", "v.scope = scope;", "E0308"),
]
for sdk, proto in (("WebShareSummary", "WebShareSummary"), ("WebConfigInfo", "WebShareListener")):
    cases.extend([
        (f"v: rmux_proto::{proto}", f"let _ = <rmux_sdk::{sdk} as From<rmux_proto::{proto}>>::from(v);", "E0277"),
        (f"v: rmux_proto::{proto}", f"let _: rmux_sdk::{sdk} = v.into();", "E0277"),
    ])
    if args.private:
        cases.append((f"v: rmux_proto::{proto}", f"let _ = rmux_sdk::{sdk}::from_proto(v);", "E0624"))
if args.private:
    for sdk, proto in (("WebShareScope", "WebShareScope"), ("WebSharePaneTarget", "PaneTargetRef")):
        cases.append((f"v: rmux_proto::{proto}", f"let _ = rmux_sdk::{sdk}::from_proto(v);", "E0624"))
        for source, dest in ((f"rmux_sdk::{sdk}", f"rmux_proto::{proto}"),
                             (f"rmux_proto::{proto}", f"rmux_sdk::{sdk}")):
            cases.append((f"v: {source}", f"let _ = <{dest} as From<{source}>>::from(v);", "E0277"))
            cases.append((f"v: {source}", f"let _: {dest} = v.into();", "E0277"))
source = args.out_dir / "removed.rs"
source.write_text("\n".join(
    f"pub fn check_{i}({params}) {{ {body} }}" for i, (params, body, _) in enumerate(cases)
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
    assert result.returncode == 1, result.stderr
    assert len(errors) == len(cases), result.stderr
    for line, (_, body, code) in enumerate(cases, 1):
        matching = [item for item in errors if any(
            span["is_primary"] and span["line_start"] == line for span in item["spans"])]
        assert len(matching) == 1 and matching[0]["code"]["code"] == code, (
            f"expected exactly {code} for {body} at line {line}: {result.stderr}")
print(f"{len(cases)} expressions {args.expect}; rustc exit {result.returncode}")
