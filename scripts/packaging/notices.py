#!/usr/bin/env python3
"""Build-derived Cargo notices and their binding to packaged payloads."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def sha(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def payloads(directory):
    suffix = ".exe" if (directory / "rmux.exe").is_file() else ""
    return {name: sha(directory / (name + suffix))
            for name in ("rmux", "rmux-full", "rmux-daemon")}


def policy_inputs():
    names = ["Cargo.lock", "deny.toml", "xtask/src/third_party_notice.rs",
             "xtask/src/third_party_notice/supplements.rs", "scripts/packaging/notices.py"]
    names += [str(path.relative_to(ROOT)) for path in
              sorted((ROOT / "scripts/packaging/licenses").glob("*/*")) if path.is_file()]
    return {name: sha(ROOT / name) for name in names}


def cache_path(directory):
    return directory / "third-party-notices" / "current.json"


def atomic(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=path.name + ".", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(temporary, 0o644)
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def capture(command, log):
    """A failed child cannot be hidden by redirection or a later generator."""
    log.parent.mkdir(parents=True, exist_ok=True)
    with log.open("wb") as stdout, Path(str(log) + ".stderr").open("wb") as stderr:
        result = subprocess.run(command, cwd=ROOT, stdout=stdout, stderr=stderr)
    if result.returncode:
        sys.stderr.buffer.write(Path(str(log) + ".stderr").read_bytes())
        raise RuntimeError(f"child exited {result.returncode}; build output retained at {log}")


def record(directory, logs):
    before = policy_inputs()
    work = Path(tempfile.mkdtemp(prefix="notice-", dir=directory / "third-party-notices"))
    metadata, notice = work / "metadata.json", work / "NOTICE.txt"
    capture(["cargo", "metadata", "--locked", "--offline", "--format-version=1"], metadata)
    command = ["cargo", "run", "--locked", "--offline", "--quiet", "--package", "xtask", "--",
               "third-party-notice", "--metadata", str(metadata), "--policy", str(ROOT / "deny.toml"),
               "--output", str(notice)]
    for log in logs:
        command += ["--build-log", str(log.resolve())]
    capture(command, work / "generator.stdout")
    if before != policy_inputs():
        raise RuntimeError("Cargo.lock or license policy changed during notice generation")
    value = dict(schema=1, inputs=before, payloads=payloads(directory),
                 notice=notice.read_bytes().decode("utf-8"),
                 notice_sha256=sha(notice),
                 logs=[dict(path=str(log.resolve()), sha256=sha(log)) for log in logs])
    atomic(cache_path(directory), (json.dumps(value, sort_keys=True) + "\n").encode())


def build(directory, command):
    if not command or command[0] != "--":
        raise RuntimeError("build requires -- followed by the Cargo build command")
    command = command[1:]
    if any(arg.startswith("--message-format") for arg in command):
        raise RuntimeError("notice build owns the Cargo message format")
    inputs = policy_inputs()
    parent = directory / "third-party-notices"
    parent.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="build-", dir=parent))
    suffix = ".exe" if sys.platform == "win32" else ""
    full, tiny, daemon = [work / (role + ".jsonl") for role in ("full", "tiny", "daemon")]
    target = command[command.index("--target") + 1] if "--target" in command else None
    if ("--release" in command and target and sys.platform != "win32"
            and "windows" not in target.split("-")):
        mappings = [
            (ROOT, "/rmux-source"),
            (Path(os.environ.get("CARGO_HOME") or Path.home() / ".cargo"), "/cargo"),
            (Path(os.environ.get("RUSTUP_HOME") or Path.home() / ".rustup"), "/rustup"),
        ]
        flags = [f"--remap-path-prefix={path.resolve()}={label}" for path, label in mappings]
        # Cargo merges target config arrays (including linker flags). Avoid
        # replacing the caller's compiler environment or modifying host tools.
        command += ["--config", f"target.{target}.rustflags=" + json.dumps(flags, ensure_ascii=False)]
    command += ["--message-format=json-render-diagnostics"]
    capture(command + ["--bin", "rmux"], full)
    original = sha(directory / ("rmux" + suffix))
    shutil.copy2(directory / ("rmux" + suffix), directory / ("rmux-full" + suffix))
    if sha(directory / ("rmux-full" + suffix)) != original:
        raise RuntimeError("full helper changed while copying")
    capture(command + ["--features", "tiny-cli", "--bin", "rmux"], tiny)
    capture(command + ["--bin", "rmux-daemon"], daemon)
    if original != sha(directory / ("rmux-full" + suffix)) or inputs != policy_inputs():
        raise RuntimeError("full helper, lockfile or policy changed during the builds")
    record(directory, [full, tiny, daemon])


def verified(directory, required):
    path = cache_path(directory)
    if not path.exists() and not required:
        return None
    value = json.loads(path.read_bytes())
    if value["schema"] != 1 or value["inputs"] != policy_inputs() or value["payloads"] != payloads(directory):
        raise RuntimeError("third-party notice does not match the payloads, lockfile or license policy; rebuild")
    if not value["notice"].startswith("RMUX third-party notices\n"):
        raise RuntimeError("invalid third-party notice cache")
    if hashlib.sha256(value["notice"].encode("utf-8")).hexdigest() != value["notice_sha256"]:
        raise RuntimeError("third-party notice text changed after generation")
    for log in value["logs"]:
        if sha(log["path"]) != log["sha256"]:
            raise RuntimeError("Cargo build log changed after notice generation")
    return value


def stage(directory, root, variant, required):
    """Keep the generated license text in the payload as well as its sidecar."""
    import layout
    value = verified(directory, required)
    if value is None:
        return
    from layout_profiles import with_notices
    descriptor = with_notices(layout.load())
    entry, placement = next((entry, placement) for entry, placement in
                            layout.entries(descriptor, variant)
                            if entry["id"] == "third-party-notices")
    relative = entry["source"]["name"] if variant == "rpm" else placement["path"]
    atomic(root / relative, value["notice"].encode("utf-8"))


def publish(directory, archive, required):
    value = verified(directory, required)
    if value is None:
        return
    path = Path(str(archive) + ".NOTICE.txt")
    content = f"Package: {archive.name}\nSHA256: {sha(archive)}\n\n{value['notice']}"
    atomic(path, content.encode("utf-8"))
    manifest = archive.parent / "SHA256SUMS.txt"
    rows = manifest.read_text().splitlines() if manifest.exists() else []
    rows = [row for row in rows if row.split("  ", 1)[-1] != path.name]
    rows.append(f"{sha(path)}  {path.name}")
    atomic(manifest, ("\n".join(sorted(rows, key=lambda row: row.split("  ", 1)[-1])) + "\n").encode())
    print(f"notice={path}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="mode", required=True)
    for name in ("build", "record", "verify", "publish", "stage"):
        command = commands.add_parser(name)
        command.add_argument("--binary-dir", type=Path, required=True)
        if name == "build":
            command.add_argument("command", nargs=argparse.REMAINDER)
        if name == "record":
            command.add_argument("--log", type=Path, action="append", required=True)
        if name in ("verify", "publish", "stage"):
            required = command.add_mutually_exclusive_group()
            required.add_argument("--required", action="store_true")
            required.add_argument("--optional", action="store_false", dest="required")
            command.set_defaults(required=False)
        if name == "stage":
            command.add_argument("--root", type=Path, required=True)
            command.add_argument("--variant", choices=("unix-archive", "windows-archive", "debian", "rpm"), required=True)
        if name == "publish":
            command.add_argument("--archive", type=Path, required=True)
    command = commands.add_parser("capture")
    command.add_argument("--log", type=Path, required=True)
    command.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.mode == "capture":
        if not args.command or args.command[0] != "--":
            raise RuntimeError("capture requires -- followed by a command")
        capture(args.command[1:], args.log.resolve())
        return
    directory = args.binary_dir.resolve()
    if args.mode == "build":
        build(directory, args.command)
    elif args.mode == "record":
        cache_path(directory).parent.mkdir(parents=True, exist_ok=True)
        record(directory, args.log)
    elif args.mode == "verify":
        verified(directory, args.required)
    elif args.mode == "stage":
        stage(directory, args.root.resolve(), args.variant, args.required)
    else:
        publish(directory, args.archive.resolve(), args.required)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, RuntimeError) as error:
        print(f"third-party notice: {error}", file=sys.stderr)
        sys.exit(1)
