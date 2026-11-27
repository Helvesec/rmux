"""Check the error cutoff against rlibs from a locked root build, without resolving dependencies.

--expect available characterizes the public leaks on the exact parent.
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
cases = [
    ("rmux_proto::RmuxError", "rmux_sdk::RmuxError::protocol(value)", "E0308"),
    ("rmux_proto::RmuxError", "<rmux_sdk::RmuxError as From<rmux_proto::RmuxError>>::from(value)", "E0277"),
    ("rmux_proto::RmuxError", "Into::<rmux_sdk::RmuxError>::into(value)", "E0277"),
    ("rmux_proto::ErrorResponse", "<rmux_sdk::RmuxError as From<rmux_proto::ErrorResponse>>::from(value)", "E0277"),
    ("rmux_proto::ErrorResponse", "Into::<rmux_sdk::RmuxError>::into(value)", "E0277"),
    ("rmux_proto::RmuxError", "rmux_sdk::protocol_diagnostic(&value)", "E0308"),
    ("rmux_proto::RmuxError", "rmux_sdk::unsupported_feature_id(&value)", "E0308"),
    ("rmux_proto::RmuxError", "rmux_sdk::diagnostics::protocol_diagnostic(&value)", "E0308"),
    ("rmux_proto::RmuxError", "rmux_sdk::diagnostics::unsupported_feature_id(&value)", "E0308"),
    ("rmux_sdk::RmuxError", "matches!(value, rmux_sdk::RmuxError::Protocol { source: rmux_proto::RmuxError::SessionNotFound(_), .. })", "E0308"),
    ("rmux_sdk::RmuxError", "{ if let rmux_sdk::RmuxError::Protocol { source, .. } = value { let _: rmux_proto::RmuxError = source; } }", "E0308"),
]
if args.expect == "unavailable":
    cases += [
        ("rmux_proto::RmuxError", "rmux_sdk::RmuxError::from_proto(value)", "E0624"),
        ("rmux_sdk::RmuxError", "value.protocol_source()", "E0624"),
        ("rmux_sdk::ProtocolError", "value.into_proto()", "E0624"),
        ("rmux_sdk::ProtocolError", "value.inner", "E0616"),
        ("rmux_sdk::RmuxError", "<rmux_sdk::RmuxError as Clone>::clone(&value)", "E0277"),
        ("rmux_sdk::CollectError", "<rmux_sdk::CollectError as Clone>::clone(&value)", "E0277"),
        ("rmux_sdk::ProtocolError", "<rmux_sdk::ProtocolError as Clone>::clone(&value)", "E0277"),
    ]
source = args.out_dir / "removed.rs"
source.write_text("\n".join(
    f"pub fn check_{index}(value: {ty}) {{ let _ = {expression}; }}"
    for index, (ty, expression, _) in enumerate(cases)
) + "\n")
result = subprocess.run([
    str(args.rustc), "--edition=2021", "--crate-type=lib", "--emit=metadata",
    "--error-format=json", str(source), "--out-dir", str(args.out_dir),
    "--extern", "rmux_sdk=" + str(args.sdk), "--extern", "rmux_proto=" + str(args.proto),
    "-L", "dependency=" + str(args.deps),
], text=True, capture_output=True, check=False)
(args.out_dir / "diagnostics.jsonl").write_text(result.stderr)
errors = [item for line in result.stderr.splitlines()
          if (item := json.loads(line)).get("level") == "error" and item.get("code")]
if args.expect == "available":
    assert result.returncode == 0, result.stderr
else:
    assert result.returncode == 1, result.stderr
    assert len(errors) == len(cases), result.stderr
    for line, (_, expression, code) in enumerate(cases, 1):
        matching = [item for item in errors if any(
            span["is_primary"] and span["line_start"] == line for span in item["spans"])]
        assert len(matching) == 1 and matching[0]["code"]["code"] == code, (
            f"expected exactly {code} for {expression} at line {line}: {result.stderr}")
print(f"{len(cases)} expressions {args.expect}; rustc exit {result.returncode}")
