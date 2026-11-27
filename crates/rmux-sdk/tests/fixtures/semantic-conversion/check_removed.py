"""Reject public semantic-error transport conversions with exact compiler controls.

Use --expect available against clean 9dd to retain the independent positive
control. supported.rs also compiles as a test with only --extern rmux_sdk.
Supply rlibs from the same locked Cargo build, never whichever glob is newest.
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
source = Path(__file__).with_name("removed.rs").resolve()
result = subprocess.run([
    str(args.rustc), "--edition=2021", "--crate-type=lib", "--emit=metadata",
    "--error-format=json", str(source), "--out-dir", str(args.out_dir),
    "--extern", "rmux_sdk=" + str(args.sdk), "--extern", "rmux_proto=" + str(args.proto),
    "-L", "dependency=" + str(args.deps),
], text=True, capture_output=True, check=False)
(args.out_dir / "diagnostics.jsonl").write_text(result.stderr)
diagnostics = [json.loads(line) for line in result.stderr.splitlines()]
errors = [item for item in diagnostics if item.get("level") == "error" and item.get("code")]
if args.expect == "available":
    assert result.returncode == 0 and not errors, result.stderr
else:
    assert result.returncode == 1, result.stderr
    assert len(errors) == 4, result.stderr
    trait_error = "the trait bound `rmux_proto::RmuxError: {}<SessionNameError>` is not satisfied"
    expected = [
        (1, 26, 47, trait_error.format("From")),
        (2, 92, 96, trait_error.format("From")),
        (3, 113, 121, trait_error.format("TryFrom")),
        (4, 115, 116, "`?` couldn't convert the error to `rmux_proto::RmuxError`"),
    ]
    for error, (line, start, end, message) in zip(errors, expected):
        assert error["code"]["code"] == "E0277" and error["message"] == message, result.stderr
        primary = [span for span in error["spans"] if span["is_primary"]]
        assert len(primary) == 1, result.stderr
        span = primary[0]
        assert (span["file_name"], span["line_start"], span["line_end"],
                span["column_start"], span["column_end"]) == (str(source), line, line, start, end), result.stderr
        assert span["label"] == (
            "the trait `From<SessionNameError>` is not implemented for `rmux_proto::RmuxError`"
        ), result.stderr
    uncoded = [item["message"] for item in diagnostics if item.get("level") == "error" and not item.get("code")]
    assert uncoded == ["aborting due to 4 previous errors"], result.stderr
print(f"4 semantic-error transport conversions {args.expect}; rustc exit {result.returncode}")
