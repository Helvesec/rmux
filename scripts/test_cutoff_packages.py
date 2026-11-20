"""Adversarial package bytes, not installation, signature or native evidence."""
import bz2
import gzip
import hashlib
import importlib.util
import io
import json
import lzma
from pathlib import Path
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

from package_fixture_helpers import ar_bytes, cpio_bytes, deb_bytes, rpm_bytes, tar_bytes

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts/bench"))
import cutoff_assets as assets
import package_payloads as reader


def zipped(files):
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, data in files:
            archive.writestr(name, data)
    return output.getvalue()


def codecs():
    result = {"none": ("", lambda b: b), "gzip": (".gz", gzip.compress),
              "bzip2": (".bz2", bz2.compress), "xz": (".xz", lzma.compress),
              "lzma": (".lzma", lambda b: lzma.compress(b, format=lzma.FORMAT_ALONE))}
    try:
        from compression import zstd
    except ImportError:
        pass
    else:
        result["zstd"] = (".zst", zstd.compress)
    return result


class PackageBytes(unittest.TestCase):
    def setUp(self):
        audit = ROOT / ".rmux-audit/s4207"
        audit.mkdir(parents=True, exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(dir=audit)
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    def scan(self, data, name="package.bin"):
        path = self.root / name
        path.write_bytes(data)
        return assets.scan(path)

    def reject(self, data, name="package.bin"):
        with self.assertRaises(ValueError):
            self.scan(data, name)

    def test_compression_matrix_and_decoded_member_hashes(self):
        files = [("./usr/bin/rmux", b"\x7fELF safe fixture")]
        control = tar_bytes([("control", b"Package: fixture\n")])
        for codec, (suffix, compress) in codecs().items():
            package = ar_bytes([("debian-binary", b"2.0\n"),
                                ("control.tar.gz", gzip.compress(control)),
                                ("data.tar" + suffix, compress(tar_bytes(files)))])
            rpm = rpm_bytes(files, codec, compress(cpio_bytes(files)))
            for kind, raw in (("deb", package), ("rpm", rpm)):
                with self.subTest(kind=kind, codec=codec):
                    result = self.scan(raw, "package." + kind)
                    self.assertEqual(result["status"], "PASS")
                    self.assertTrue(any(row["name"].endswith("usr/bin/rmux")
                                        and row["sha256"] == hashlib.sha256(files[0][1]).hexdigest()
                                        for row in result["members"]))
            if suffix in ("", ".gz", ".xz", ".zst"):
                self.assertEqual(self.scan(ar_bytes([("debian-binary", b"2.0\n"),
                    ("control.tar" + suffix, compress(control)), ("data.tar", tar_bytes(files))]))["status"], "PASS")

    def test_missing_zstd_dependency_is_closed(self):
        with patch.dict(sys.modules, {"compression": None}):
            with self.assertRaisesRegex(ValueError, "requires Python"):
                reader.decompress(b"\x28\xb5\x2f\xfd", "zstd", 100)

    def test_control_scripts_and_rpm_script_tags_are_scanned_never_run(self):
        canary = self.root / "executed"
        script = b"#!/bin/sh\ntouch " + str(canary).encode() + b"\n# RMUX_PERF_TRACE\n"
        for raw in (deb_bytes([("usr/bin/rmux", b"safe")], [("control", b"Package: fixture\n"),
                                                               ("postinst", script)]),
                    rpm_bytes([("usr/bin/rmux", b"safe")], extra_tags=[(1024, 6, script.decode())])):
            with patch.object(subprocess, "Popen", side_effect=AssertionError("package execution forbidden")):
                result = self.scan(raw)
            self.assertEqual(result["status"], "FAIL")
            self.assertFalse(canary.exists())

    def test_markers_encodings_paths_and_nested_payloads(self):
        for build in (deb_bytes, rpm_bytes):
            for marker in assets.MARKERS:
                for encoding in ("utf-8", "utf-16-le", "utf-16-be"):
                    with self.subTest(format=build.__name__, marker=marker, encoding=encoding):
                        self.assertEqual(self.scan(build([("usr/bin/rmux", marker.encode(encoding))]))["status"], "FAIL")
                self.assertEqual(self.scan(build([("usr/share/" + marker, b"safe")]))["status"], "FAIL")
            for wrap in (tar_bytes, zipped):
                nested = wrap([("payload", b"RMUX_PERF_TRACE")])
                result = self.scan(build([("usr/share/resource.bin", nested)]))
                self.assertEqual(result["status"], "FAIL")
                self.assertTrue(any(row["name"].endswith("resource.bin!payload") and row["findings"]
                                    for row in result["members"]))

    def test_packages_wrapped_as_renamed_tar_zip_members(self):
        for build in (deb_bytes, rpm_bytes):
            for wrap in (tar_bytes, zipped):
                for payload, status in ((b"safe", "PASS"), (b"RMUX_PERF_TRACE", "FAIL")):
                    with self.subTest(format=build.__name__, wrap=wrap.__name__, status=status):
                        result = self.scan(wrap([("resources/package.bin", build([("usr/bin/rmux", payload)]))]))
                        self.assertEqual(result["status"], status)

    def test_debian_ar_order_duplicates_headers_and_missing_control(self):
        control = ("control.tar", tar_bytes([("control", b"Package: fixture\n")]))
        data = ("data.tar", tar_bytes([("usr/bin/rmux", b"safe")]))
        version = ("debian-binary", b"2.0\n")
        for members in ([version, data, control], [version, control, control],
                        [version, control, data, data], [version, data],
                        [("debian-binary", b"3.0\n"), control, data],
                        [version, ("control.tar", tar_bytes([("postinst", b"safe")])), data]):
            self.reject(ar_bytes(members))
        valid = ar_bytes([version, control, data])
        for index, value in ((8 + 48, b"-"), (8 + 58, b"!")):
            self.reject(valid[:index] + value + valid[index + 1:])
        self.reject(valid + b"trailing")

    def test_package_truncations_at_headers_payload_and_footer(self):
        for build in (deb_bytes, rpm_bytes):
            raw = build([("usr/bin/rmux", b"safe")])
            for length in (1, 4, 8, 67, 96, 108, len(raw) // 2, len(raw) - 1, len(raw) - 8):
                with self.subTest(format=build.__name__, length=length):
                    self.reject(raw[:length], "broken." + ("deb" if build == deb_bytes else "rpm"))

    def test_compressed_corruption_trailing_and_unknown_codecs(self):
        cpio = cpio_bytes([("usr/bin/rmux", b"safe")])
        for codec, (_, compress) in codecs().items():
            if codec == "none":
                continue
            encoded = compress(cpio)
            for payload in (encoded[:-1], encoded + b"trailing", encoded[:len(encoded) // 2]):
                with self.subTest(codec=codec):
                    self.reject(rpm_bytes([], codec, payload))
        self.reject(rpm_bytes([], "unknown", cpio))
        damaged = bytearray(gzip.compress(cpio))
        damaged[-8] ^= 1
        self.reject(rpm_bytes([], "gzip", bytes(damaged)))

    def test_rpm_header_bounds_types_and_duplicate_tags(self):
        raw = rpm_bytes([("usr/bin/rmux", b"safe")])
        end, _ = reader.rpm_header(raw, 96)
        main = (end + 7) // 8 * 8
        for offset, value in ((main + 8, 4097), (main + 12, 0xffffffff),
                              (main + 20, 10), (main + 24, 0xffffffff), (main + 28, 2)):
            damaged = bytearray(raw)
            struct.pack_into(">I", damaged, offset, value)
            self.reject(bytes(damaged))
        self.reject(rpm_bytes([], extra_tags=[(1124, 6, "cpio")]))
        self.reject(raw[:4] + b"\x04\0" + raw[6:])
        self.reject(raw + b"trailing")

    def test_cpio_path_aliases_duplicates_links_devices_and_padding(self):
        for name in ("../escape", "/absolute", "a/../b", "a//b", "a\\b", "a!b", "a\0b"):
            self.reject(rpm_bytes([(name, b"safe")]))
        for files in ([('same', b'a'), ('same', b'b')], [('same', b'a'), ('./same', b'b')]):
            self.reject(rpm_bytes(files))
        cpio = cpio_bytes([("usr/bin/rmux", b"safe")])
        for field, value in ((1, 0o120777), (1, 0o020600), (4, 2), (6, 0xffffffff), (11, 0), (12, 1)):
            offset = 6 + field * 8
            changed = cpio[:offset] + f"{value:08x}".encode() + cpio[offset + 8:]
            self.reject(rpm_bytes([], "none", changed))
        for changed in (cpio[:-4], cpio + b"junk", cpio[:110] + b"\0" + cpio[111:]):
            self.reject(rpm_bytes([], "none", changed))

    def test_cpio_crc_variant_checks_each_file(self):
        cpio = bytearray(cpio_bytes([("usr/bin/rmux", b"safe")]))
        cpio[:6] = b"070702"
        cpio[102:110] = f"{sum(b'safe'):08x}".encode()
        self.assertEqual(self.scan(rpm_bytes([], "none", bytes(cpio)))["status"], "PASS")
        cpio[102:110] = b"00000001"
        self.reject(rpm_bytes([], "none", bytes(cpio)))

    def test_tar_links_aliases_devices_truncation_and_trailing(self):
        for name in ("../escape", "/absolute", "a/../b", "a//b", "a\\b"):
            self.reject(deb_bytes([(name, b"safe")]))
        self.reject(deb_bytes([("same", b"a"), ("./same", b"b")]))
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.CHRTYPE, tarfile.FIFOTYPE):
            stream = io.BytesIO()
            with tarfile.open(fileobj=stream, mode="w") as archive:
                entry = tarfile.TarInfo("link")
                entry.type, entry.linkname = kind, "usr/bin/rmux"
                archive.addfile(entry)
            self.reject(ar_bytes([("debian-binary", b"2.0\n"),
                ("control.tar", tar_bytes([("control", b"Package: fixture\n")])), ("data.tar", stream.getvalue())]))
        tar = tar_bytes([("file", b"safe")])
        for broken in (tar[:1024], tar + b"junk", tar[:512 + 2]):
            self.reject(deb_bytes([("resource.tar", broken)]))
            self.reject(rpm_bytes([("resource.tar", broken)]))

    def test_opaque_nested_corrupt_and_depth_bound(self):
        for build in (deb_bytes, rpm_bytes):
            for name, raw in (("x.snap", b"opaque"), ("x.bin", b"hsqs"),
                              ("x.deb", b"opaque"), ("x.rpm", b"opaque"),
                              ("x.bin", b"\xed\xab\xee\xdbtruncated"),
                              ("x.zip", b"PK\x03\x04truncated")):
                self.reject(build([(name, raw)]))
        raw = rpm_bytes([("file", b"safe")])
        for _ in range(4):
            raw = rpm_bytes([("nested.bin", raw)])
        self.reject(raw)

    def test_expansion_member_and_input_bounds(self):
        for build in (deb_bytes, rpm_bytes):
            with patch.object(assets, "MAX_BYTES", 40000):
                self.reject(build([("huge", b"a" * 100000)]))
            with patch.object(assets, "MAX_MEMBERS", 3):
                self.reject(build([(str(i), b"a") for i in range(5)]))
        with patch.object(assets, "MAX_BYTES", 10):
            self.reject(b"\x7fELF" + b"a" * 20)

    def test_inventory_and_digest_use_one_byte_snapshot(self):
        original = rpm_bytes([("usr/bin/rmux", b"safe")])
        replacement = rpm_bytes([("usr/bin/rmux", b"RMUX_PERF_TRACE")])
        path = self.root / "race.rpm"
        path.write_bytes(original)
        read = reader.read_bounded
        def replace_after_read(*args):
            data = read(*args)
            path.write_bytes(replacement)
            return data
        with patch.object(reader, "read_bounded", side_effect=replace_after_read):
            result = assets.scan(path)
        self.assertEqual(result["status"], "PASS")
        self.assertEqual(result["sha256"], hashlib.sha256(original).hexdigest())
        self.assertNotEqual(result["sha256"], hashlib.sha256(path.read_bytes()).hexdigest())

    def test_symlink_input_and_magic_suffix_disagreement(self):
        raw = rpm_bytes([("file", b"safe")])
        self.reject(raw, "wrong.deb")
        target = self.root / "target"
        target.write_bytes(raw)
        link = self.root / "link"
        link.symlink_to(target)
        with self.assertRaises(ValueError):
            assets.scan(link)

    def test_cutoff_cli_reports_clean_sink_and_corrupt_package_results(self):
        paths = []
        for name, raw in (("clean.deb", deb_bytes([("usr/bin/rmux", b"safe")])),
                          ("sink.rpm", rpm_bytes([("usr/bin/rmux", b"RMUX_PERF_TRACE")])),
                          ("corrupt.rpm", b"\xed\xab\xee\xdbtruncated")):
            path = self.root / name
            path.write_bytes(raw)
            paths.append(path)
        output = self.root / "cutoff.json"
        command = [sys.executable, str(Path(assets.__file__)), *map(str, paths), "--output", str(output)]
        run = subprocess.run(command, capture_output=True, timeout=10)
        self.assertEqual(run.returncode, 1, run.stderr)
        results = json.loads(output.read_bytes())
        self.assertEqual([r["status"] for r in results], ["PASS", "FAIL", "FAIL"])
        self.assertIn("error", results[-1])
        run = subprocess.run([sys.executable, str(Path(assets.__file__)), str(paths[0]),
                              "--output", str(output)], capture_output=True, timeout=10)
        self.assertEqual(run.returncode, 0, run.stderr)

    def test_directory_entries_and_renamed_lzma_tar_are_supported(self):
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode="w") as archive:
            for name in (".", "usr", "usr/bin"):
                entry = tarfile.TarInfo(name)
                entry.type = tarfile.DIRTYPE
                archive.addfile(entry)
            entry = tarfile.TarInfo("usr/bin/rmux")
            entry.size = 4
            archive.addfile(entry, io.BytesIO(b"safe"))
        raw = ar_bytes([("debian-binary", b"2.0\n"),
                        ("control.tar", tar_bytes([("control", b"Package: fixture\n")])),
                        ("data.tar", stream.getvalue())])
        self.assertEqual(self.scan(raw)["status"], "PASS")
        nested = lzma.compress(tar_bytes([("payload", b"RMUX_PERF_TRACE")]), format=lzma.FORMAT_ALONE)
        self.assertEqual(self.scan(rpm_bytes([("resource.bin", nested)]))["status"], "FAIL")

    def test_rpm_overlapping_index_storage_has_a_cumulative_bound(self):
        from package_fixture_helpers import rpm_header
        raw = rpm_header([(1, 7, b"a" * 30), (2, 7, b"b" * 30)])
        with patch.object(reader, "MAX_BYTES", 40), self.assertRaisesRegex(ValueError, "indexed data"):
            reader.rpm_header(raw, 0)

    def test_rpm_v4_immutable_regions_file_tables_and_payload_agree(self):
        # Independent header-file inventory from the existing pinned SBOM path.
        # This is a format fixture, not an installed or authenticated package.
        import sbom
        def header(region, tags):
            store, indexes = bytearray(), []
            for tag, kind, values in sorted(tags):
                width = {3: 2, 4: 4}.get(kind, 1)
                store.extend(b"\0" * (-len(store) % width))
                if kind in (3, 4):
                    data = b"".join(v.to_bytes(width, "big") for v in values)
                elif kind == 7:
                    data = values
                else:
                    data = b"".join(v.encode() + b"\0" for v in values)
                indexes.append(struct.pack(">IIII", tag, kind, len(store), len(values)))
                store.extend(data)
            first = struct.pack(">IIII", region, 7, len(store), 16)
            store.extend(struct.pack(">IIiI", region, 7, -16 * (len(tags) + 1), 16))
            return (b"\x8e\xad\xe8\x01\0\0\0\0" + struct.pack(">II", len(tags) + 1, len(store))
                    + first + b"".join(indexes) + store)
        content = b"safe payload bytes"
        payload = gzip.compress(cpio_bytes([("./usr/bin/rmux", content)]))
        main = header(63, [(1000, 6, ["fixture"]), (1001, 6, ["1"]), (1002, 6, ["1"]),
                          (1004, 9, ["Format fixture"]), (1005, 9, ["Format fixture"]),
                          (1014, 6, ["MIT"]), (1016, 9, ["Development/Tools"]),
                          (1021, 6, ["linux"]), (1022, 6, ["noarch"]),
                          (1028, 4, [len(content)]), (1030, 3, [0o100644]),
                          (1035, 8, [hashlib.sha256(content).hexdigest()]), (1036, 8, [""]),
                          (1039, 8, ["root"]), (1040, 8, ["root"]), (1116, 4, [0]),
                          (1117, 8, ["rmux"]), (1118, 8, ["/usr/bin/"]),
                          (1124, 6, ["cpio"]), (1125, 6, ["gzip"]), (5011, 4, [8])])
        signature = header(62, [(269, 6, [hashlib.sha1(main).hexdigest()]),
                                (1000, 4, [len(main) + len(payload)]),
                                (1004, 7, hashlib.md5(main + payload).digest())])
        raw = rpm_bytes([])[:96] + signature + b"\0" * (-len(signature) % 8) + main + payload
        identity, expected, _ = sbom.rpm_headers(raw)
        self.assertEqual(identity, dict(name="fixture", version="1", release="1", arch="noarch"))
        actual = {e.name: dict(path=e.name, bytes=len(e.data), sha256=hashlib.sha256(e.data).hexdigest(), mode=e.mode)
                  for e in reader.entries(raw, "rpm") if e.section == "data"}
        self.assertEqual(actual, expected)
        self.assertEqual(self.scan(raw)["status"], "PASS")

    def test_decoded_toml_canaries_remain_rejected(self):
        spec = importlib.util.spec_from_file_location("product_cutoff", ROOT / "scripts/bench/check-perf-cutoff.py")
        cutoff = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cutoff)
        manifest = self.root / "Cargo.toml"
        manifest.write_text('[package]\nname="fixture"\nversion="1.0.0"\n')
        cutoff.check_product_cutoff(self.root)
        for encoded in ('[features]\n"initial\\u005ftrace"=[]\n',
                        '[features]\nclean=["dep/initial\\u002dtrace"]\n'):
            manifest.write_text(encoded)
            with self.assertRaisesRegex(RuntimeError, "instrumentation exposure"):
                cutoff.check_product_cutoff(self.root)


if __name__ == "__main__":
    unittest.main()
