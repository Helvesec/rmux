#!/usr/bin/env python3
"""Classify one exact Chocolatey OData package entry."""

from __future__ import annotations

import argparse
import re
import sys
import xml.etree.ElementTree as ET
from datetime import datetime, timezone
from pathlib import Path

ATOM_ENTRY = "{http://www.w3.org/2005/Atom}entry"
DATA_NAMESPACE = "http://schemas.microsoft.com/ado/2007/08/dataservices"
MAX_DOCUMENT_SIZE = 1024 * 1024
PENDING_SENTINEL = datetime(1900, 1, 1, tzinfo=timezone.utc)
VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--document", type=Path, required=True)
    parser.add_argument("--expected-version", required=True)
    return parser.parse_args()


def single_property(root: ET.Element, name: str) -> str:
    values = root.findall(f".//{{{DATA_NAMESPACE}}}{name}")
    if len(values) != 1 or values[0].text is None:
        raise ValueError(f"Chocolatey metadata must contain one {name}")
    value = values[0].text.strip()
    if not value or list(values[0]):
        raise ValueError(f"Chocolatey metadata {name} is malformed")
    return value


def odata_datetime(raw: str) -> datetime:
    normalized = f"{raw[:-1]}+00:00" if raw.endswith("Z") else raw
    try:
        value = datetime.fromisoformat(normalized)
    except ValueError as error:
        raise ValueError("Chocolatey Published timestamp is malformed") from error
    if value.tzinfo is None:
        value = value.replace(tzinfo=timezone.utc)
    return value.astimezone(timezone.utc)


def package_status(document: Path, expected_version: str) -> str:
    if VERSION.fullmatch(expected_version) is None:
        raise ValueError("expected Chocolatey version is malformed")
    if document.is_symlink() or not document.is_file():
        raise ValueError("Chocolatey metadata must be one real file")
    size = document.stat().st_size
    if size <= 0 or size > MAX_DOCUMENT_SIZE:
        raise ValueError("Chocolatey metadata size is invalid")
    raw = document.read_bytes()
    if b"<!DOCTYPE" in raw.upper() or b"<!ENTITY" in raw.upper():
        raise ValueError("Chocolatey metadata cannot contain a DTD")
    try:
        root = ET.fromstring(raw)
    except ET.ParseError as error:
        raise ValueError("Chocolatey metadata is malformed XML") from error
    if root.tag != ATOM_ENTRY:
        raise ValueError("Chocolatey metadata is not one OData entry")

    version = single_property(root, "Version")
    if version != expected_version:
        raise ValueError("Chocolatey metadata version differs from the release")
    approved_raw = single_property(root, "IsApproved")
    if approved_raw not in {"true", "false"}:
        raise ValueError("Chocolatey IsApproved value is malformed")
    approved = approved_raw == "true"
    published = odata_datetime(single_property(root, "Published"))

    if not approved and published == PENDING_SENTINEL:
        return "pending"
    if approved and published > PENDING_SENTINEL:
        return "public"
    raise ValueError("Chocolatey approval and publication metadata disagree")


def main() -> int:
    args = parse_args()
    print(package_status(args.document, args.expected_version))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError) as error:
        print(f"chocolatey-package-status: {error}", file=sys.stderr)
        raise SystemExit(1) from error
