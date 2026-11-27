"""Check the deliberate associated-error migration using exact locked build rlibs.

--expect available characterizes the old signatures on eb6dc711.
The sibling supported.rs is compiled separately with only --extern rmux_sdk.
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
    'let _: Result<rmux_sdk::SessionName, rmux_proto::RmuxError> = rmux_sdk::SessionName::new("a");',
    'let _: <rmux_sdk::SessionName as std::str::FromStr>::Err = rmux_proto::RmuxError::EmptySessionName;',
    'let _: <rmux_sdk::SessionName as TryFrom<&str>>::Error = rmux_proto::RmuxError::EmptySessionName;',
    'let _: <rmux_sdk::SessionName as TryFrom<String>>::Error = rmux_proto::RmuxError::EmptySessionName;',
    'let _: rmux_proto::RmuxError = rmux_sdk::SessionName::new("").unwrap_err();',
    'let _ = matches!(rmux_sdk::SessionName::new(""), Err(rmux_proto::RmuxError::EmptySessionName));',
]
source = args.out_dir / "old_errors.rs"
source.write_text("\n".join(f"pub fn check_{i}() {{ {body} }}" for i, body in enumerate(cases)) + "\n")
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
    for line in range(1, len(cases) + 1):
        matching = [item for item in errors if any(
            span["is_primary"] and span["line_start"] == line for span in item["spans"])]
        assert len(matching) == 1 and matching[0]["code"]["code"] == "E0308", result.stderr
print(f"{len(cases)} old error signatures {args.expect}; rustc exit {result.returncode}")
