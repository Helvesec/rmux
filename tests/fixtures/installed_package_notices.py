"""Exercise the real Unix packager/installer with a completed Cargo fixture."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/packaging"))
import notices


def run(*args, **kwargs):
    return subprocess.run(args, check=True, cwd=ROOT, **kwargs)


def main():
    fixture = Path(sys.argv[1]).resolve()
    host = subprocess.check_output(["rustc", "-vV"], text=True)
    target = next(line.split(": ", 1)[1] for line in host.splitlines() if line.startswith("host: "))
    directory = fixture / "package-target" / target / "debug"
    directory.mkdir(parents=True)
    for name in ("rmux", "rmux-full", "rmux-daemon"):
        shutil.copy2(fixture / "target/debug/notice-package-fixture", directory / name)
    # The notice and log were produced by the Rust generator and Cargo, not a
    # hand-written list. Bind those bytes to the fixture's real executable.
    notice = (fixture / "NOTICE.txt").read_text()
    log = fixture / "build.jsonl"
    notices.atomic(notices.cache_path(directory), json.dumps(dict(
        schema=1, inputs=notices.policy_inputs(), payloads=notices.payloads(directory),
        notice=notice, notice_sha256=notices.sha(fixture / "NOTICE.txt"),
        logs=[dict(path=str(log), sha256=notices.sha(log))])).encode())
    completions = directory / "completions"
    completions.mkdir()
    names = subprocess.check_output([str(ROOT / "scripts/packaging/layout"), "source-names",
                                    "--variant", "unix-archive", "--kind", "completion"], text=True)
    for name in names.splitlines():
        (completions / name).write_text("# fixture completion\n")
    dist = fixture / "dist"
    env = dict(os.environ, CARGO_TARGET_DIR=str(fixture / "package-target"),
               RMUX_MAX_SUPPORTED_GLIBC="999.0", RMUX_PACKAGE_CODESIGN_ADHOC="0")
    run("bash", "scripts/package-unix.sh", "--configuration", "debug", "--target", target,
        "--skip-build", "--allow-stale-binary", "--output-dir", str(dist), env=env)
    archive, = dist.glob("*.tar.gz")
    unpacked = fixture / "unpacked"
    with tarfile.open(archive) as package:
        package.extractall(unpacked)
    package, = unpacked.iterdir()
    bundled = package / "share/licenses/rmux/THIRD-PARTY-NOTICES.txt"
    assert bundled.is_file(), "build-derived notice missing from actual package payload"
    assert bundled.read_bytes() == (fixture / "NOTICE.txt").read_bytes()
    prefix = fixture / "prefix"
    run("bash", str(package / "install.sh"), "--prefix", str(prefix), "--no-verify")
    installed = prefix / "share/licenses/rmux"
    assert (installed / "THIRD-PARTY-NOTICES.txt").read_bytes() == bundled.read_bytes()
    for name in ("LICENSE-MIT", "LICENSE-APACHE"):
        assert (installed / name).read_bytes() == (ROOT / name).read_bytes()
    print("PASS actual archive and private installation retain generated notice and licenses")


if __name__ == "__main__":
    main()
