#!/usr/bin/env python3
"""Native compiler canaries. Accepts the same required tool/output flags as the gate."""

from pathlib import Path
import shutil
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from public_api_gate import Runner, baseline_source, digest, parser, tree_digest
from public_api_consumers import check_consumer
from public_api_policy import Cell, cells

FIXTURES = Path(__file__).resolve().parent / "fixtures" / "public-api-gate"


def demand(condition, message):
    if not condition:
        raise RuntimeError(message)


def tiny(runner, text, name, version="1.0.0", features=()):
    directory = runner.output / name
    directory.mkdir()
    source = directory / "lib.rs"
    source.write_text(text)
    cfg = [arg for feature in features for arg in ("--cfg", f'feature="{feature}"')]
    common = [source, "--crate-name", "api_canary", "--edition", "2021",
              "--target", runner.target, *cfg]
    runner.run(name + "-rustdoc", [runner.rustdoc, *common, "--crate-version", version,
                                    "-Z", "unstable-options", "--output-format", "json",
                                    "-o", directory])
    library = directory / "libapi_canary.rlib"
    runner.run(name + "-library", [runner.rustc, *common, "--crate-type", "rlib",
                                    "-C", "debug-assertions=yes", "-C", "overflow-checks=yes",
                                    "-o", library])
    return directory / "api_canary.json", library, cfg


def consumer(runner, artifact, name, expected=0, diagnostic=None):
    _, library, cfg = artifact
    executable = runner.output / (name + (".exe" if sys.platform == "win32" else ""))
    text = runner.run(name, [runner.rustc, FIXTURES / "consumer.rs", "--edition", "2021",
                             "--target", runner.target, "--extern", f"api_canary={library}",
                             *cfg, "-o", executable], expected=expected)
    if diagnostic:
        demand(diagnostic in text, f"wrong compiler failure: {text}")
    if expected == 0:
        runner.run(name + "-execute", [executable])


def tiny_canaries(runner):
    frozen = (FIXTURES / "baseline.rs").read_text()
    baseline_hash = digest(FIXTURES / "baseline.rs")
    old = tiny(runner, frozen, "tiny-baseline")
    for name, text in (("unchanged", frozen), ("additive", frozen + "\npub fn added() {}\n")):
        new = tiny(runner, text, "tiny-" + name, "99.0.0")
        runner.compare(old[0], new[0], name + "-semver")
        consumer(runner, new, name + "-consumer")
    for name, text, diagnostic in (
        ("removed", frozen.replace("pub fn retained", "fn retained"), "E0603"),
        ("changed", frozen.replace("value: u32) -> u32", "value: u64) -> u64"), "E0308"),
    ):
        new = tiny(runner, text, "tiny-" + name, "99.0.0")
        # Positive control: a sufficiently large version bump ordinarily hides it.
        runner.compare(old[0], new[0], name + "-major-control", release_type="major")
        # 0.49.0 does not detect this parameter/return type change. The mandatory
        # frozen compiler consumer catches it; retain that observed limitation.
        message = runner.compare(old[0], new[0], name + "-semver", expected=int(name == "removed"))
        if name == "removed":
            demand("semver requires new major version" in message, message)
        consumer(runner, new, name + "-consumer", expected=1, diagnostic=diagnostic)

    # Exactly regex+web, without crossterm, exposes this API. All-features alone
    # cannot detect its removal. Every powerset cell gets real compiler evidence.
    mutant = frozen.replace("pub fn combination_only", "fn combination_only")
    sdk_cells = [c for c in cells(runner.metadata(runner.source, "canary-metadata"), runner.target)
                 if c.package == "rmux-sdk" and c.mode == "no-default"]
    red = []
    for index, cell in enumerate(sdk_cells):
        old = tiny(runner, frozen, f"combination-{index}-old", features=cell.features)
        new = tiny(runner, mutant, f"combination-{index}-new", "99.0.0", cell.features)
        broken = cell.features == ("regex", "web")
        runner.compare(old[0], new[0], f"combination-{index}-semver", expected=int(broken))
        consumer(runner, old, f"combination-{index}-baseline-consumer")
        consumer(runner, new, f"combination-{index}-consumer", expected=int(broken),
                 diagnostic="E0603" if broken else None)
        if broken:
            red.append(cell.id)
    demand(red == ["rmux-sdk/no-default/regex+web"], "combination canary missed its cell")
    demand(digest(FIXTURES / "baseline.rs") == baseline_hash, "frozen fixture changed")
    invalid = runner.output / "invalid.rs"
    invalid.write_text("this is not Rust;\n")
    try:
        runner.run("compiler-failure", [runner.rustdoc, invalid, "-o", runner.output / "invalid"])
    except RuntimeError:
        pass
    else:
        raise RuntimeError("failed compiler became green")
    try:
        runner.compare(runner.output / "absent.json", old[0], "missing-baseline")
    except RuntimeError:
        pass
    else:
        raise RuntimeError("missing baseline became green")
    empty = runner.output / "empty-repository"
    empty.mkdir()
    runner.run("empty-repository", ["git", "init", empty])
    try:
        baseline_source(runner, empty, "missing-git-")
    except RuntimeError:
        pass
    else:
        raise RuntimeError("missing immutable Git baseline became green")


def product_canary(runner):
    baseline = baseline_source(runner, runner.source)
    before = tree_digest(baseline)
    mutant = runner.output / "candidate-mutant"
    shutil.copytree(runner.source, mutant,
                    ignore=shutil.ignore_patterns(".git", ".rmux-*", "target", "__pycache__"))
    path = mutant / "crates/ratatui-rmux/src/lib.rs"
    text = path.read_text()
    original = "pub use theme::{cell_style, color, glyph_symbol, modifier};"
    demand(text.count(original) == 1, "candidate export canary needs adaptation")
    path.write_text(text.replace(original, "pub use theme::{color, glyph_symbol, modifier};"))
    cell = Cell("ratatui-rmux", "default")
    old = runner.document(baseline, cell, "control")
    control = runner.document(runner.source, cell, "candidate")
    runner.compare(old, control, "candidate-control")
    check_consumer(runner, baseline, runner.source, cell, "control-")
    for side in ("mutant", "bumped"):
        if side == "bumped":
            manifest = mutant / "crates/ratatui-rmux/Cargo.toml"
            version = '\nversion = "0.10.0"\n'
            demand(manifest.read_text().count(version) == 1, "candidate version canary needs adaptation")
            manifest.write_text(manifest.read_text().replace(version, '\nversion = "99.0.0"\n'))
            lock = mutant / "Cargo.lock"
            package = 'name = "ratatui-rmux"\nversion = "0.10.0"'
            demand(lock.read_text().count(package) == 1, "candidate lock canary needs adaptation")
            lock.write_text(lock.read_text().replace(package, 'name = "ratatui-rmux"\nversion = "99.0.0"'))
        new = runner.document(mutant, cell, side)
        if side == "bumped":
            runner.run("candidate-version-bump-control", [runner.semver, "--baseline-rustdoc", old,
                       "--current-rustdoc", new, "--target", runner.target, "--color", "never"])
        message = runner.compare(old, new, side + "-removed-export", expected=1)
        demand("cell_style" in message, message)
        try:
            check_consumer(runner, baseline, mutant, cell, side + "-")
        except RuntimeError:
            message = (runner.output / (side + "-consumer-ratatui-rmux-default.log")).read_text()
            demand("E0425" in message and "cell_style" in message, message)
        else:
            raise RuntimeError("removed candidate export compiled downstream")
    demand(tree_digest(baseline) == before, "baseline was changed by canary")


if __name__ == "__main__":
    args = parser().parse_args()
    runner = Runner(args.output, args.toolchain_bin, args.cargo_home, args.semver_checks, args.target)
    runner.source = args.source.resolve()
    runner.preflight()
    tiny_canaries(runner)
    product_canary(runner)
    print("PASS: frozen compiler canaries and candidate export control/mutation")
