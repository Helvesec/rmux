"""Strict, versioned input types for the Web compatibility catalogue."""
from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
import json
from pathlib import Path, PurePosixPath
import re
from typing import Any, Optional


class ArtifactKind(str, Enum):
    NATIVE = "native"
    DELEGATED = "delegated"
    DISABLED = "disabled"
    CRYPTO = "crypto"


class NetworkKind(str, Enum):
    LOOPBACK = "loopback"
    EXTERNAL = "external"
    PROVIDER = "provider"


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def unique_object(pairs: list) -> dict:
    result = {}
    for key, value in pairs:
        require(key not in result, f"duplicate JSON field: {key}")
        result[key] = value
    return result


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=unique_object,
                      parse_constant=lambda value: fail(f"invalid JSON number: {value}"))


def fail(message: str) -> Any:
    raise ValueError(message)


def fields(value: Any, expected: str) -> dict:
    require(isinstance(value, dict) and set(value) == set(expected.split()),
            f"expected fields: {expected}")
    return value


def string(value: Any) -> str:
    require(isinstance(value, str) and bool(value.strip()) and "\0" not in value,
            "expected nonempty string")
    return value


def identifier(value: Any) -> str:
    value = string(value)
    require(re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*", value) is not None,
            f"invalid identifier: {value}")
    return value


def strings(value: Any) -> tuple[str, ...]:
    require(isinstance(value, list), "expected string array")
    result = tuple(string(item) for item in value)
    require(len(set(result)) == len(result), "duplicate array item")
    return result


def positive_integer(value: Any) -> int:
    require(type(value) is int and value > 0, "expected positive integer")
    return value


@dataclass(frozen=True)
class Support:
    id: str
    path: str
    contains: str

    @classmethod
    def parse(cls, value: Any) -> Support:
        row = fields(value, "id path contains")
        path = string(row["path"])
        require(not PurePosixPath(path).is_absolute() and "\\" not in path
                and all(part not in {"", ".", ".."} for part in path.split("/")),
                "support path must be repository-relative")
        return cls(identifier(row["id"]), path, string(row["contains"]))


@dataclass(frozen=True)
class Profile:
    id: str
    protocol_version: int
    support: tuple[str, ...]
    requirements: tuple[str, ...]

    @classmethod
    def parse(cls, value: Any) -> Profile:
        row = fields(value, "id protocol_version support requirements")
        return cls(identifier(row["id"]), positive_integer(row["protocol_version"]),
                   strings(row["support"]), strings(row["requirements"]))


@dataclass(frozen=True)
class Artifact:
    id: str
    kind: ArtifactKind
    support: tuple[str, ...]
    requirements: tuple[str, ...]

    @classmethod
    def parse(cls, value: Any) -> Artifact:
        row = fields(value, "id kind support requirements")
        return cls(identifier(row["id"]), ArtifactKind(string(row["kind"])),
                   strings(row["support"]), strings(row["requirements"]))


@dataclass(frozen=True)
class Network:
    id: str
    kind: NetworkKind
    support: tuple[str, ...]
    requirements: tuple[str, ...]
    preset: Optional[str]

    @classmethod
    def parse(cls, value: Any) -> Network:
        row = fields(value, "id kind support requirements preset")
        kind = NetworkKind(string(row["kind"]))
        preset = None if row["preset"] is None else string(row["preset"])
        require((kind is NetworkKind.PROVIDER) == (preset is not None),
                "only provider networks must name a preset")
        return cls(identifier(row["id"]), kind, strings(row["support"]),
                   strings(row["requirements"]), preset)


def rows(value: Any, parser: Any) -> tuple:
    require(isinstance(value, list) and bool(value), "expected nonempty row array")
    result = tuple(parser(row) for row in value)
    require(len({row.id for row in result}) == len(result), "duplicate row identifier")
    return result


@dataclass(frozen=True)
class Catalogue:
    protocol_version: int
    capability: str
    support: tuple[Support, ...]
    frontends: tuple[Profile, ...]
    daemons: tuple[Profile, ...]
    artifacts: tuple[Artifact, ...]
    networks: tuple[Network, ...]

    @classmethod
    def load(cls, path: Path) -> Catalogue:
        value = fields(read_json(path),
                       "schema_version protocol_version capability support frontends daemons artifacts networks")
        require(type(value["schema_version"]) is int and value["schema_version"] == 1,
                "unsupported Web compatibility schema_version")
        result = cls(positive_integer(value["protocol_version"]), string(value["capability"]),
                     rows(value["support"], Support.parse), rows(value["frontends"], Profile.parse),
                     rows(value["daemons"], Profile.parse), rows(value["artifacts"], Artifact.parse),
                     rows(value["networks"], Network.parse))
        known = {item.id for item in result.support}
        used = set()
        for row in (*result.frontends, *result.daemons, *result.artifacts, *result.networks):
            require(bool(row.support) and set(row.support) <= known,
                    f"missing or orphan support reference: {row.id}")
            used.update(row.support)
        require(known == used, f"unconsumed support references: {sorted(known - used)}")
        return result
