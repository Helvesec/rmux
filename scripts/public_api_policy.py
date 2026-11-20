"""Preliminary post-cutoff Rust facade contract; changes require review.

Cargo metadata remains the authority for feature definitions. This allowlist
deliberately does not infer API promises from package publishability.
"""

from dataclasses import dataclass
from itertools import combinations

BASELINE_COMMIT = "654a4092c1df36d5539630fb3e29240f60e77698"
BASELINE_TREE = "c8e34c602a6d13a455d6546b134f5f4502c6653e"
RUST_VERSION = "1.96.1"
SEMVER_VERSION = "0.49.0"
TARGETS = ("x86_64-unknown-linux-gnu", "aarch64-apple-darwin", "x86_64-pc-windows-msvc")
FROZEN_FIXTURES = {
    "ratatui-consumer.rs": "d98a9ed153949d979bf974db300d77ce24fc831d33826a46dca9372b2d81ca5c",
    "sdk-dependencies.rs": "658d8d8e1ea67f969fe804dc480abdb573b246c9350ad4ee0e765fbe43e45102",
    "baseline.rs": "87b745b468a7781e4d2200992e48ba697d237d5bf23366d4b06d0e0c84df84c2",
    "consumer.rs": "783d670fca405317a46e92d152640a2f403e0887557d2c5f35977cfc05913291",
}
FACADES = {
    "rmux-sdk": {
        "default": ["web"],
        "crossterm": ["dep:crossterm"],
        "regex": ["dep:regex"],
        "web": [],
    },
    "ratatui-rmux": {},
}


@dataclass(frozen=True)
class Cell:
    package: str
    mode: str
    features: tuple = ()

    @property
    def id(self):
        return f"{self.package}/{self.mode}" + (
            "/" + "+".join(self.features) if self.features else ""
        )

    @property
    def flags(self):
        if self.mode == "default":
            return []  # Cargo defaults, never cargo-semver-checks' heuristic.
        if self.mode == "all-features":
            return ["--all-features"]
        return ["--no-default-features"] + (
            ["--features", ",".join(self.features)] if self.features else []
        )


def cells(metadata, target, selection=None):
    if target not in TARGETS:
        raise ValueError(f"unqualified target selection: {target}")
    packages = {p["name"]: p for p in metadata["packages"]
                if p["id"] in metadata["workspace_members"]}
    result = []
    for name, expected in FACADES.items():
        if name not in packages:
            raise ValueError(f"missing facade: {name}")
        package = packages[name]
        if package["features"] != expected:
            raise ValueError(f"feature contract changed for {name}: {package['features']}")
        if name == "ratatui-rmux":
            sdk = [d for d in package["dependencies"] if d["name"] == "rmux-sdk" and d["kind"] is None]
            if (len(sdk) != 1 or not sdk[0]["uses_default_features"] or sdk[0]["features"]
                    or sdk[0]["optional"] or sdk[0]["target"] is not None):
                raise ValueError("ratatui-rmux must consume the unconditional SDK default surface")
        libraries = [t for t in package["targets"] if "lib" in t["kind"]]
        if len(libraries) != 1 or libraries[0]["name"] != name.replace("-", "_"):
            raise ValueError(f"missing or ambiguous library for {name}")
        features = sorted(set(package["features"]) - {"default"})
        for size in range(len(features) + 1):
            result.extend(Cell(name, "no-default", combo)
                          for combo in combinations(features, size))
        result.extend([Cell(name, "default"), Cell(name, "all-features")])
    required = [cell.id for cell in result]
    if selection is not None and (len(selection) != len(required) or set(selection) != set(required)):
        raise ValueError("selection must contain every required cell exactly once; "
                         f"expected {required}, got {selection}")
    return result


def require_complete(required, completed):
    if len(completed) != len(required) or set(completed) != {c.id for c in required}:
        raise ValueError("incomplete or duplicated cell execution")
