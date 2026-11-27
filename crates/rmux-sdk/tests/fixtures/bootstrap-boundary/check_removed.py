"""Check bootstrap removal against exact artifacts from a locked Cargo build.

Run separately for default, no-default and all SDK features on each native OS.
--expect available is the parent control for the former public routes.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
for option in ("rustc", "sdk", "tokio", "ipc", "deps", "out-dir"):
    parser.add_argument("--" + option, required=True, type=Path)
parser.add_argument("--expect", choices=("available", "unavailable"), default="unavailable")
args = parser.parse_args()
args.out_dir.mkdir(parents=True, exist_ok=True)
platform = "windows" if os.name == "nt" else "unix"
module = f"rmux_sdk::bootstrap::startup_{platform}"
stream = "rmux_ipc::BlockingLocalStream" if platform == "windows" else "tokio::net::UnixStream"
cases = [
    f"let _: fn({stream}) -> {module}::StartupOutcome = {module}::StartupOutcome::Started;",
    f"let _: fn({stream}) -> {module}::StartupOutcome = {module}::StartupOutcome::JoinedExisting;",
    f"let _: fn({module}::StartupOutcome) -> {stream} = {module}::StartupOutcome::into_stream;",
    f"let _: fn(&{module}::StartupOutcome) -> bool = {module}::StartupOutcome::is_owner;",
]
launcher = "|_: &std::path::Path|" if platform == "windows" else "||"
for route, extra in [
    ("connect_or_start", ""),
    ("connect_or_start_with", ", std::time::Duration::ZERO, std::time::Duration::ZERO"),
    ("connect_or_start_with_timeout", ", None, std::time::Duration::ZERO"),
]:
    cases.append(f"let _ = {module}::{route}(std::path::Path::new(\"\"), {launcher} async {{ Ok(()) }}{extra});")
if platform == "unix":
    cases.append(f"let _: fn(&{module}::StartupOutcome) -> &{stream} = {module}::StartupOutcome::stream;")
else:
    for route, timeout in [
        ("connect_or_start_blocking_with", "std::time::Duration::ZERO"),
        ("connect_or_start_blocking_with_timeout", "None"),
        ("connect_or_start_blocking_selected_with", "std::time::Duration::ZERO"),
    ]:
        cases.append(f"let _ = {module}::{route}(std::path::Path::new(\"\"), |_| Ok(()), {timeout}, std::time::Duration::ZERO);")
cases += [
    "let _ = rmux_sdk::bootstrap::discovery::resolve_timeout(None, None);",
    "let _ = rmux_sdk::bootstrap::discovery::resolve_endpoint(&rmux_sdk::RmuxEndpoint::Default);",
]
if args.expect == "unavailable":
    cases += [
        "use rmux_sdk::rmux_bootstrap;",
        "use rmux_sdk::startup_unix;",
        "use rmux_sdk::startup_windows;",
        "use rmux_sdk::bootstrap::deadline::StartupDeadline;",
    ]
for index, expression in enumerate(cases):
    source = args.out_dir / f"route_{index}.rs"
    source.write_text(f"pub fn check() {{ {expression} }}\n")
    command = [str(args.rustc), "--edition=2021", "--crate-type=lib", "--emit=metadata",
               "--error-format=json", str(source), "--out-dir", str(args.out_dir),
               "-L", "dependency=" + str(args.deps)]
    for name in ("sdk", "tokio", "ipc"):
        crate = name if name == "tokio" else "rmux_" + name
        command += ["--extern", crate + "=" + str(getattr(args, name))]
    result = subprocess.run(command, text=True, capture_output=True, check=False)
    (args.out_dir / f"route_{index}.jsonl").write_text(result.stderr)
    if args.expect == "available":
        assert result.returncode == 0, result.stderr
    else:
        errors = [item for line in result.stderr.splitlines()
                  if (item := json.loads(line)).get("level") == "error" and item.get("code")]
        assert result.returncode == 1 and errors, result.stderr
        for error in errors:
            assert error["code"]["code"] in {"E0432", "E0433", "E0603"}, result.stderr
            assert any(span["is_primary"] and span["line_start"] == 1
                       and span["file_name"] == str(source) for span in error["spans"]), result.stderr
print(f"{len(cases)} {platform} bootstrap routes {args.expect}")
