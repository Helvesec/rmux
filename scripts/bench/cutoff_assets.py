#!/usr/bin/env python3
"""Inspect exact payload bytes; never extract or execute an archive member.

A source crate can ship a regression test which names retired controls. Such a
reference remains a finding (not an implicit exemption). Callers decide scope
from the member inventory; this scanner never turns missing assets into PASS.
"""
from __future__ import annotations
import argparse
import hashlib
import json
from io import BytesIO
from pathlib import Path, PurePosixPath
import tarfile
import stat
import sys
import zipfile
import zlib

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "packaging"))
import package_payloads

MARKERS = ("RMUX_INITIAL_TRACE_CONFIG", "RMUX_PERF_TRACE", "INITIAL_TRACE",
           "initial_trace", "initial-trace", "perf-instrument")
MAX_BYTES = 512 * 1024 * 1024
MAX_MEMBERS = 10000
OPAQUE_CONTAINER_SUFFIXES = (".deb", ".rpm", ".snap", ".nupkg")
OPAQUE_CONTAINER_MAGIC = (b"!<arch>\n", b"\xed\xab\xee\xdb", b"hsqs", b"sqsh")
TAR_SUFFIXES = (".crate", ".tar.gz", ".tgz", ".tar", ".tar.xz", ".tar.bz2",
                ".tar.zst", ".tar.lzma")
COMPRESSION_SUFFIXES = ((".gz", "gzip"), (".bz2", "bzip2"), (".xz", "xz"),
                        (".zst", "zstd"), (".lzma", "lzma"))

def digest(data):
    return hashlib.sha256(data).hexdigest()

def member(name, data):
    parts = PurePosixPath(name).parts
    if not parts or name.startswith("/") or ".." in parts or "\\" in name:
        raise ValueError("unsafe member name: " + name)
    hits = []
    for marker in MARKERS:
        encodings = [encoding for encoding in ("utf-8", "utf-16-le", "utf-16-be")
                     if marker.encode(encoding) in data]
        if marker in name or encodings:
            hits.append({"marker": marker, "path": marker in name, "encodings": encodings})
    return {"name": name, "bytes": len(data), "sha256": digest(data), "findings": hits}


def raw_tar(data):
    # Detection must not invoke tarfile's implicit, unbounded decompressors.
    # A recognizable header must reach validation even with a bad checksum.
    if data[257:263] in (b"ustar\0", b"ustar "):
        return True
    try:
        with tarfile.open(fileobj=BytesIO(data), mode="r:"):
            return True
    except tarfile.TarError:
        return False


def lzma_header(data):
    if len(data) < 13 or data[0] >= 225:
        return False
    dictionary = int.from_bytes(data[1:5], "little")
    return dictionary in {size for shift in range(12, 32)
                          for size in (1 << shift, 3 << (shift - 1))}


def decoded_name(name):
    for suffix, _ in COMPRESSION_SUFFIXES:
        if name.lower().endswith(suffix):
            return name[:-len(suffix)]
    return name


def scan(path):
    path = Path(path)
    raw = package_payloads.read_bounded(path, MAX_BYTES)
    rows, names, total = [], set(), 0

    def record(name, data):
        nonlocal total
        total += len(data)
        if name in names or len(names) >= MAX_MEMBERS or total > MAX_BYTES:
            raise ValueError("duplicate member or asset expansion limit")
        names.add(name)
        rows.append(member(name, data))

    def add(name, data, depth):
        record(name, data)
        expand(name, data, name + "!", depth)

    def expand(name, data, prefix, depth):
        kind = package_payloads.package_kind(name, data)
        nested = BytesIO(data)
        is_zip = zipfile.is_zipfile(nested)
        codec = next((codec for magic, codec in (
            (b"\x1f\x8b", "gzip"), (b"BZh", "bzip2"),
            (b"\xfd7zXZ\x00", "xz"), (b"\x28\xb5\x2f\xfd", "zstd"))
            if data.startswith(magic)), None)
        if codec is None and lzma_header(data):
            codec = "lzma"
        if codec is None:
            codec = next((kind for suffix, kind in COMPRESSION_SUFFIXES
                          if name.lower().endswith(suffix)), None)
        is_tar = not (kind or is_zip or codec) and raw_tar(data)
        advertised_tar = name.lower().endswith(TAR_SUFFIXES)
        advertised = advertised_tar or name.lower().endswith(".zip")
        if not (kind or is_zip or codec or is_tar or advertised):
            if name.lower().endswith(OPAQUE_CONTAINER_SUFFIXES) or data.startswith(OPAQUE_CONTAINER_MAGIC + (b"PK\x03\x04",)):
                raise ValueError("unsupported/corrupt nested container: " + name)
            return False
        if depth >= 4:
            raise ValueError("nested archive depth limit")
        if kind:
            for entry in package_payloads.entries(data, kind, MAX_BYTES - total, MAX_MEMBERS - len(names)):
                child = prefix + entry.section + "!" + entry.name
                if entry.section == "metadata":
                    record(child, entry.data)
                else:
                    add(child, entry.data, depth + 1)
        elif is_zip:
            seen = set()
            with zipfile.ZipFile(BytesIO(data)) as archive:
                for child in archive.infolist():
                    clean = package_payloads.safe_name(child.filename, child.is_dir())
                    if clean in seen or len(seen) >= MAX_MEMBERS:
                        raise ValueError("duplicate member or asset expansion limit")
                    seen.add(clean)
                    mode = stat.S_IFMT(child.external_attr >> 16)
                    if child.is_dir():
                        if mode not in (0, stat.S_IFDIR) or child.file_size:
                            raise ValueError("nonregular ZIP directory")
                        continue
                    if mode not in (0, stat.S_IFREG) or child.file_size > MAX_BYTES - total:
                        raise ValueError("nonregular ZIP member or expansion limit")
                    try:
                        content = archive.read(child)
                    except (RuntimeError, NotImplementedError, EOFError, zlib.error) as error:
                        raise ValueError("unsupported/corrupt ZIP member") from error
                    add(prefix + child.filename, content, depth + 1)
        else:
            decoded = package_payloads.decompress(data, codec or "none", MAX_BYTES - total)
            if codec:
                # A reserved synthetic path retains every decoded byte, including
                # archive metadata. Real member names cannot contain "!".
                record(prefix + "@decoded!" + str(depth), decoded)
                if not (advertised_tar or raw_tar(decoded)):
                    # Compression does not imply tar. Preserve any advertised
                    # inner suffix and inspect recognized/opaque containers again.
                    expand(decoded_name(name), decoded, prefix, depth + 1)
                    return True
            for child in package_payloads.tar_entries(decoded, "data", MAX_MEMBERS - len(names)):
                add(prefix + child.name, child.data, depth + 1)
        return True

    if not expand(path.name, raw, "", 0):
        executable_magic = (b"\x7fELF", b"MZ", b"\xfe\xed\xfa\xce", b"\xce\xfa\xed\xfe",
                            b"\xfe\xed\xfa\xcf", b"\xcf\xfa\xed\xfe", b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca")
        if not raw.startswith(executable_magic):
            raise ValueError("unsupported/corrupt container or nonexecutable asset")
        record(path.name, raw)
    if not rows:
        raise ValueError("empty asset")
    return {"asset": str(path), "bytes": len(raw), "sha256": digest(raw), "members": rows,
            "status": "FAIL" if any(row["findings"] for row in rows) else "PASS",
            "scope": "exact bytes and member paths only; no source/build or platform inference"}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("assets", nargs="+", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    results = []
    for path in args.assets:
        try:
            results.append(scan(path))
        except (OSError, ValueError, tarfile.TarError, zipfile.BadZipFile) as error:
            results.append({"asset": str(path), "status": "FAIL", "error": str(error)})
    args.output.write_text(json.dumps(results, indent=2) + "\n")
    return int(any(row["status"] != "PASS" for row in results))

if __name__ == "__main__":
    raise SystemExit(main())
