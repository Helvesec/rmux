"""Local container boundary regression; fixtures are not native package proof."""
import gzip
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "cutoff_assets", ROOT / "scripts/bench/cutoff_assets.py")
assets = importlib.util.module_from_spec(spec)
spec.loader.exec_module(assets)


class NestedLocalContainers(unittest.TestCase):
    def test_unsupported_local_container_cannot_gain_pass_when_wrapped(self):
        hidden = gzip.compress(b"RMUX_PERF_TRACE", mtime=0)
        formats = {"deb": b"!<arch>\n", "rpm": b"\xed\xab\xee\xdb",
                   "snap": b"hsqs", "nupkg": b"PK\x03\x04"}
        audit = ROOT / ".rmux-audit"
        audit.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as directory:
            root = Path(directory)
            for extension, magic in formats.items():
                for name in ("payload." + extension, "payload.bin"):
                    # ZIP's magic alone cannot identify a corrupt .bin ZIP;
                    # its advertised .nupkg suffix must still fail closed.
                    if extension == "nupkg" and name.endswith(".bin"):
                        continue
                    data = magic + hidden
                    raw = root / name
                    raw.write_bytes(data)
                    with self.subTest(format=extension, name=name, wrapper="none"):
                        with self.assertRaises(ValueError):
                            assets.scan(raw)
                    for wrapper in ("tar", "zip"):
                        outer = root / ("outer." + wrapper)
                        if wrapper == "tar":
                            with tarfile.open(outer, "w:gz") as archive:
                                item = tarfile.TarInfo("resources/" + name)
                                item.size = len(data)
                                archive.addfile(item, io.BytesIO(data))
                        else:
                            with zipfile.ZipFile(outer, "w", zipfile.ZIP_DEFLATED) as archive:
                                archive.writestr("resources/" + name, data)
                        with self.subTest(format=extension, name=name, wrapper=wrapper):
                            with self.assertRaises(ValueError):
                                assets.scan(outer)


if __name__ == "__main__":
    unittest.main()
