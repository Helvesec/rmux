"""Check all removed command/full-spec expressions and precise compiler diagnostics.

Use --expect available with exactbase SDK/proto artifacts as a fail-before control.
No Cargo resolution is performed; pass rlibs from a locked build (see --help).
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
for source, dest in [
    ("rmux_proto::Request", "rmux_sdk::RmuxCommandKind"),
    ("rmux_proto::Request", "rmux_sdk::RmuxCommand"),
    ("rmux_sdk::RmuxCommandKind", "rmux_proto::Request"),
    ("rmux_sdk::RmuxCommand", "rmux_proto::Request"),
] + [
    ("rmux_sdk::" + sdk, "rmux_proto::" + proto)
    for sdk, payload in (
        ("NewSessionSpec", "NewSessionExtRequest"),
        ("AttachSessionSpec", "AttachSessionExt2Request"),
        ("SplitSpec", "SplitWindowExtRequest"),
        ("RefreshClientSpec", "RefreshClientRequest"),
    ) for proto in ("Request", payload)
]:
    cases.append((source, f"<{dest} as From<{source}>>::from(value)", "E0277"))
    cases.append((source, f"Into::<{dest}>::into(value)", "E0277"))
for ty in ("RmuxCommand", "RmuxCommandKind"):
    cases.append(("rmux_sdk::" + ty, "value.into_request()", "E0599"))
cases += [
    ("rmux_proto::Request", "rmux_sdk::RmuxCommandKind::Request(value)", "E0599"),
    ("rmux_proto::Request", "rmux_sdk::RmuxCommand::new(value)", "E0277"),
    ("rmux_proto::Request", "rmux_sdk::RmuxCommand::with_endpoint(rmux_sdk::RmuxEndpoint::Default, value)", "E0277"),
]
source = args.out_dir / "removed.rs"
source.write_text("\n".join(
    f"pub fn check_{index}(value: {ty}) {{ let _ = {expression}; }}"
    for index, (ty, expression, _) in enumerate(cases)
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
