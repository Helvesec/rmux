"""Bounded deb/ar/tar and RPM/cpio readers shared by cutoff and local SBOM.

Only bytes supplied by the caller are read. No extraction, subprocess, package
installation or signature claim. Historical pinned SBOM profiles stay separate.
zstd requires Python's compression.zstd; absence is an explicit closed failure.
"""
from __future__ import annotations

import bz2
from dataclasses import dataclass
from io import BytesIO
import lzma
import os
from pathlib import PurePosixPath
import re
import stat
import struct
import tarfile
import zlib

DEB_MAGIC = b"!<arch>\n"
RPM_MAGIC = b"\xed\xab\xee\xdb"
MAX_BYTES = 512 * 1024 * 1024
MAX_MEMBERS = 10000


def read_bounded(path, limit=MAX_BYTES):
    """One regular-file snapshot supplies both inventory and subject digest."""
    try:
        fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
        with os.fdopen(fd, "rb") as stream:
            before = os.fstat(stream.fileno())
            require(stat.S_ISREG(before.st_mode) and before.st_size <= limit,
                    "asset must be a bounded regular file")
            raw = stream.read(limit + 1)
            after, current = os.fstat(stream.fileno()), path.lstat()
        identity = lambda s: (s.st_dev, s.st_ino, s.st_size, s.st_mtime_ns, s.st_ctime_ns)
        require(identity(before) == identity(after) == identity(current)
                and not stat.S_ISLNK(current.st_mode) and len(raw) == before.st_size,
                "asset changed during reading")
        return raw
    except OSError as error:
        raise ValueError("asset must be a readable regular file") from error


@dataclass(frozen=True)
class Entry:
    section: str
    name: str
    data: bytes
    mode: int = 0o644


def require(condition, message):
    if not condition:
        raise ValueError(message)


def package_kind(name, raw):
    """Magic detects renamed packages; an advertised corrupt package still fails."""
    suffix = next((s for s in ("deb", "rpm") if name.lower().endswith("." + s)), None)
    magic = "deb" if raw.startswith(DEB_MAGIC) else "rpm" if raw.startswith(RPM_MAGIC) else None
    require(not suffix or not magic or suffix == magic, "package suffix/magic mismatch")
    return suffix or magic


def safe_name(name, directory=False):
    name = name.removeprefix("./")
    if directory:
        name = name.removesuffix("/")
        if name in ("", "."):
            return "."
    parts = PurePosixPath(name).parts
    require(parts and not name.startswith("/") and ".." not in parts
            and "\\" not in name and "\0" not in name and "!" not in name
            and str(PurePosixPath(name)) == name and name != ".", "unsafe package member name")
    return name


def decompress(raw, kind, limit):
    """Consume exactly one compressed stream, including its checksum and end."""
    require(limit >= 0, "package expansion limit")
    if kind in ("none", ""):
        require(len(raw) <= limit, "package expansion limit")
        return raw
    try:
        if kind == "gzip":
            reader = zlib.decompressobj(31)
        elif kind == "bzip2":
            reader = bz2.BZ2Decompressor()
        elif kind in ("xz", "lzma"):
            reader = lzma.LZMADecompressor(format=lzma.FORMAT_XZ if kind == "xz" else lzma.FORMAT_ALONE,
                                          memlimit=MAX_BYTES)
        elif kind == "zstd":
            try:
                from compression import zstd
            except ImportError as error:
                raise ValueError("zstd payload requires Python compression.zstd") from error
            try:
                reader = zstd.ZstdDecompressor(options={zstd.DecompressionParameter.window_log_max: 29})
                decoded = reader.decompress(raw, limit + 1)
            except zstd.ZstdError as error:
                raise ValueError("corrupt zstd package data") from error
        else:
            raise ValueError("unsupported package compression: " + kind)
        if kind != "zstd":
            decoded = reader.decompress(raw, limit + 1)
        require(len(decoded) <= limit, "package expansion limit")
        require(reader.eof and not reader.unused_data, "truncated or trailing compressed package data")
        return decoded
    except (OSError, EOFError, zlib.error, lzma.LZMAError) as error:
        raise ValueError("corrupt compressed package data") from error


def tar_entries(raw, section, max_members=MAX_MEMBERS):
    """Read regular files and validate directories, aliases and the tar ending."""
    seen, files = set(), 0
    try:
        with tarfile.open(fileobj=BytesIO(raw), mode="r:") as archive:
            for item in archive:
                name = safe_name(item.name, item.isdir())
                require(name not in seen and len(seen) < max_members, "duplicate package member or member limit")
                seen.add(name)
                if item.isdir():
                    require(item.size == 0, "package directory has content")
                    continue
                require(item.isfile() and not item.issparse(), "nonregular package member")
                require(0 <= item.size <= len(raw), "package member size bound")
                with archive.extractfile(item) as stream:
                    content = stream.read(item.size + 1)
                require(len(content) == item.size, "truncated package member")
                files += 1
                yield Entry(section, name, content, stat.S_IMODE(item.mode))
            end = archive.offset
        require(len(raw) >= end + 1024 and len(raw) % 512 == 0 and not any(raw[end:]),
                "truncated or trailing package tar data")
        require(files, "empty package tar")
    except (tarfile.TarError, EOFError, OSError) as error:
        raise ValueError("corrupt package tar") from error


def deb_entries(raw, max_bytes, max_members):
    require(raw.startswith(DEB_MAGIC), "Debian ar magic")
    offset, contents = 8, []
    while offset < len(raw):
        header = raw[offset:offset + 60]
        require(len(header) == 60 and header[-2:] == b"`\n", "truncated Debian ar header")
        try:
            name = header[:16].decode("ascii").rstrip().removesuffix("/")
            require(re.fullmatch(rb"[0-9]+ *", header[48:58]), "Debian ar size")
            size = int(header[48:58])
            for field, base in ((header[16:28], 10), (header[28:34], 10),
                                (header[34:40], 10), (header[40:48], 8)):
                require(int(field, base) >= 0, "Debian ar numeric field")
        except (UnicodeError, ValueError) as error:
            raise ValueError("invalid Debian ar header") from error
        start, end = offset + 60, offset + 60 + size
        require(end <= len(raw) and len(contents) < 3, "Debian ar member bounds/count")
        require(not size % 2 or raw[end:end + 1] == b"\n", "Debian ar padding")
        contents.append((name, raw[start:end]))
        offset = end + size % 2
    require(len(contents) == 3 and contents[0] == ("debian-binary", b"2.0\n"),
            "unsupported Debian layout/version")
    yield Entry("metadata", "debian-binary", contents[0][1])
    expanded, count = 0, 0
    codecs = {"": "none", ".gz": "gzip", ".xz": "xz", ".zst": "zstd",
              ".bz2": "bzip2", ".lzma": "lzma"}
    for (name, data), section in zip(contents[1:], ("control", "data")):
        prefix = section + ".tar"
        suffix = name[len(prefix):]
        require(name.startswith(prefix) and suffix in codecs
                and (section != "control" or suffix not in (".bz2", ".lzma")),
                "unsupported Debian control/data member")
        decoded = decompress(data, codecs[suffix], max_bytes - expanded)
        expanded += len(decoded)
        # Include decompressed archive metadata in the cutoff, not just leaf names.
        yield Entry("metadata", prefix, decoded)
        has_control = False
        for entry in tar_entries(decoded, section, max_members - count):
            has_control |= entry.name == "control"
            count += 1
            yield entry
        require(section != "control" or has_control, "missing Debian control file")


def rpm_header(raw, offset):
    require(raw[offset:offset + 8] == b"\x8e\xad\xe8\x01\0\0\0\0"
            and offset + 16 <= len(raw), "truncated RPM header")
    count, size = struct.unpack_from(">II", raw, offset + 8)
    start = offset + 16 + count * 16
    end = start + size
    require(count <= 4096 and end <= len(raw), "RPM header bounds")
    store, tags, indexed = raw[start:end], {}, 0
    view = memoryview(store)
    for i in range(count):
        tag, kind, pos, length = struct.unpack_from(">IIII", raw, offset + 16 + i * 16)
        require(tag not in tags and pos <= size and length <= MAX_BYTES, "RPM duplicate/outside tag")
        if kind in (1, 2, 3, 4, 5, 7):
            width = {3: 2, 4: 4, 5: 8}.get(kind, 1)
            require(pos % width == 0 and pos + length * width <= size, "RPM integer/binary bounds")
            stop = pos + length * width
        elif kind in (6, 8, 9):
            require(0 < length <= MAX_MEMBERS and (kind != 6 or length == 1), "RPM string count")
            stop = pos
            for _ in range(length):
                stop = store.find(b"\0", stop)
                require(stop >= 0, "RPM unterminated string")
                stop += 1
        else:
            raise ValueError("unsupported RPM header type")
        indexed += stop - pos
        require(indexed <= MAX_BYTES, "RPM indexed data bound")
        tags[tag] = (kind, view[pos:stop])
    return end, tags


def cpio_entries(raw, max_members=MAX_MEMBERS):
    offset, seen, inodes, files = 0, set(), set(), 0
    while True:
        header = raw[offset:offset + 110]
        require(len(header) == 110 and header[:6] in (b"070701", b"070702")
                and re.fullmatch(rb"[0-9a-fA-F]{104}", header[6:]), "truncated/unsupported RPM cpio header")
        fields = [int(header[i:i + 8], 16) for i in range(6, 110, 8)]
        ino, mode, _, _, links, _, size, devmajor, devminor, rmajor, rminor, namesize, checksum = fields
        offset += 110
        require(0 < namesize <= 4096 and offset + namesize <= len(raw), "RPM cpio name bounds")
        encoded = raw[offset:offset + namesize]
        require(encoded[-1:] == b"\0" and b"\0" not in encoded[:-1], "RPM cpio name terminator")
        try:
            name = encoded[:-1].decode("utf-8")
        except UnicodeError as error:
            raise ValueError("RPM cpio name encoding") from error
        end = offset + namesize
        offset = (end + 3) // 4 * 4
        require(offset <= len(raw) and not any(raw[end:offset]), "RPM cpio name padding")
        end = offset + size
        require(end <= len(raw), "truncated RPM cpio member")
        data = raw[offset:end]
        offset = (end + 3) // 4 * 4
        require(offset <= len(raw) and not any(raw[end:offset]), "RPM cpio data padding")
        require(checksum == (sum(data) & 0xffffffff if header[:6] == b"070702" else 0),
                "RPM cpio checksum mismatch")
        if name == "TRAILER!!!":
            require(size == 0 and not any(raw[offset:]) and files, "RPM cpio trailer/trailing data")
            break
        name = safe_name(name, stat.S_ISDIR(mode))
        require(name not in seen and len(seen) < max_members, "duplicate package member or member limit")
        seen.add(name)
        require(rmajor == rminor == 0, "RPM cpio device member")
        if stat.S_ISDIR(mode):
            require(size == 0, "RPM directory has content")
            continue
        inode = (devmajor, devminor, ino)
        require(stat.S_ISREG(mode) and links == 1 and inode not in inodes, "RPM nonregular/hardlinked member")
        inodes.add(inode)
        files += 1
        yield Entry("data", name, data, stat.S_IMODE(mode))


def rpm_entries(raw, max_bytes, max_members):
    require(len(raw) >= 96 and raw[:6] == RPM_MAGIC + b"\x03\x00"
            and raw[6:8] == b"\0\0" and raw[78:80] == b"\0\x05", "unsupported RPM lead")
    signature_end, _ = rpm_header(raw, 96)
    main = (signature_end + 7) // 8 * 8
    require(not any(raw[signature_end:main]), "RPM signature padding")
    payload, tags = rpm_header(raw, main)
    require(tags.get(1124) == (6, b"cpio\0"), "unsupported RPM payload format")
    compressor = tags.get(1125)
    require(compressor is not None and compressor[0] == 6, "missing RPM payload compressor")
    try:
        codec = bytes(compressor[1][:-1]).decode("ascii")
    except UnicodeError as error:
        raise ValueError("RPM compressor encoding") from error
    decoded = decompress(raw[payload:], codec, max_bytes)
    yield Entry("metadata", "headers", raw[:payload])
    yield Entry("metadata", "payload.cpio", decoded)
    yield from cpio_entries(decoded, max_members)


def entries(raw, kind, max_bytes=MAX_BYTES, max_members=MAX_MEMBERS):
    require(len(raw) <= max_bytes and max_members > 0, "package input bound")
    if kind == "deb":
        yield from deb_entries(raw, max_bytes, max_members)
    elif kind == "rpm":
        yield from rpm_entries(raw, max_bytes, max_members)
    else:
        raise ValueError("unsupported package kind")
