"""Small package-format fixtures; never installed or executed, no native credit."""
import gzip
import hashlib
import io
import struct
import tarfile


def tar_bytes(files):
    stream = io.BytesIO()
    with tarfile.open(fileobj=stream, mode="w", format=tarfile.USTAR_FORMAT) as archive:
        for name, data in files:
            entry = tarfile.TarInfo(name)
            entry.size, entry.mode = len(data), 0o644
            archive.addfile(entry, io.BytesIO(data))
    return stream.getvalue()


def ar_bytes(files):
    result = bytearray(b"!<arch>\n")
    for name, data in files:
        result.extend(f"{name:<16}{0:<12}{0:<6}{0:<6}{'100644':<8}{len(data):<10}`\n".encode())
        result.extend(data)
        if len(data) % 2:
            result.extend(b"\n")
    return bytes(result)


def deb_bytes(files, control=None):
    control = control if control is not None else [("control", b"Package: fixture\nVersion: 1\nArchitecture: all\nMaintainer: Fixture\nDescription: format test\n")]
    return ar_bytes([("debian-binary", b"2.0\n"),
                     ("control.tar.gz", gzip.compress(tar_bytes(control), mtime=0)),
                     ("data.tar.gz", gzip.compress(tar_bytes(files), mtime=0))])


def cpio_bytes(files):
    result = bytearray()
    for ino, (name, data) in enumerate([*files, ("TRAILER!!!", b"")], 1):
        encoded = name.encode() + b"\0"
        fields = (ino, 0o100644, 0, 0, 1, 0, len(data), 0, 0, 0, 0, len(encoded), 0)
        result.extend(b"070701" + b"".join(f"{v:08x}".encode() for v in fields) + encoded)
        result.extend(b"\0" * (-len(result) % 4))
        result.extend(data)
        result.extend(b"\0" * (-len(result) % 4))
    return bytes(result)


def rpm_header(tags):
    index, store = bytearray(), bytearray()
    for tag, kind, value in sorted(tags):
        if kind == 6:
            data, count = value.encode() + b"\0", 1
        elif kind == 7:
            data, count = value, len(value)
        elif kind == 4:
            store.extend(b"\0" * (-len(store) % 4))
            data, count = struct.pack(">I", value), 1
        else:
            raise AssertionError(kind)
        index.extend(struct.pack(">IIII", tag, kind, len(store), count))
        store.extend(data)
    return b"\x8e\xad\xe8\x01\0\0\0\0" + struct.pack(">II", len(tags), len(store)) + index + store


def rpm_bytes(files, compressor="gzip", payload=None, extra_tags=()):
    if payload is None:
        payload = gzip.compress(cpio_bytes(files), mtime=0)
    header = rpm_header([(1000, 6, "fixture"), (1001, 6, "1"), (1002, 6, "1"),
                         (1004, 6, "Format test"), (1005, 6, "Format fixture"),
                         (1014, 6, "MIT"), (1016, 6, "Development/Tools"),
                         (1021, 6, "linux"), (1022, 6, "noarch"),
                         (1124, 6, "cpio"), (1125, 6, compressor),
                         (1126, 6, "9"), *extra_tags])
    signature = rpm_header([(1000, 4, len(header) + len(payload)),
                            (1004, 7, hashlib.md5(header + payload).digest())])
    lead = struct.pack(">4sBBHH66sHH16s", b"\xed\xab\xee\xdb", 3, 0, 0, 1,
                       b"fixture-1-1", 1, 5, b"\0" * 16)
    return lead + signature + b"\0" * (-len(signature) % 8) + header + payload
