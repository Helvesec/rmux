"""Reuse the package layout checker to bind migration launches to installed bytes."""
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts/release"))
sys.path.insert(0, str(ROOT / "scripts/packaging"))
from installed_consumer_packages import file_hash
from canonical_payload import embedded_commit
import layout
from layout_profiles import for_package


def native_image_path(pid):
    """Read the live image using the host's process API, without shell parsing."""
    if sys.platform == "linux":
        return Path(f"/proc/{pid}/exe").resolve(strict=True)
    import ctypes
    if sys.platform == "darwin":
        library = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
        library.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
        library.proc_pidpath.restype = ctypes.c_int
        buffer = ctypes.create_string_buffer(4096)
        if library.proc_pidpath(pid, buffer, len(buffer)) <= 0:
            raise OSError(ctypes.get_errno(), "cannot identify migration daemon image")
        return Path(os.fsdecode(buffer.value)).resolve(strict=True)
    if sys.platform == "win32":
        from ctypes import wintypes
        library = ctypes.WinDLL("kernel32", use_last_error=True)
        library.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        library.OpenProcess.restype = wintypes.HANDLE
        library.QueryFullProcessImageNameW.argtypes = [wintypes.HANDLE, wintypes.DWORD, wintypes.LPWSTR, ctypes.POINTER(wintypes.DWORD)]
        library.QueryFullProcessImageNameW.restype = wintypes.BOOL
        library.CloseHandle.argtypes = [wintypes.HANDLE]
        library.CloseHandle.restype = wintypes.BOOL
        handle = library.OpenProcess(0x1000, False, pid)
        if not handle:
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            size = wintypes.DWORD(32768)
            buffer = ctypes.create_unicode_buffer(size.value)
            if not library.QueryFullProcessImageNameW(handle, 0, buffer, ctypes.byref(size)):
                raise ctypes.WinError(ctypes.get_last_error())
            return Path(buffer.value).resolve(strict=True)
        finally:
            library.CloseHandle(handle)
    raise ValueError("unsupported native migration platform")


class InstalledPackage:
    def __init__(self, root, prefix, version, wire, fixture=False, commit=None):
        self.root = self.absolute(root)
        self.prefix = self.absolute(prefix)
        if self.root == self.prefix or self.prefix.is_relative_to(self.root):
            raise ValueError("package and installed prefix must be separate")
        self.variant = "windows-archive" if os.name == "nt" else "unix-archive"
        self.descriptor = for_package(layout.load(), self.root)
        metadata = self.root / "share/rmux/artifact-metadata.json"
        self.metadata = json.loads(metadata.read_text(encoding="utf-8-sig"))
        self.version, self.wire, self.fixture = version, wire, fixture
        if self.metadata.get("rmux_version") != version:
            raise ValueError("package version differs from migration pair")
        if fixture != (self.metadata.get("migration_fixture") is True):
            raise ValueError("fixture packages require fixture mode and receive no product credit")
        if not fixture and (not commit or self.metadata.get("git_commit") != commit):
            raise ValueError("package commit differs from expected source")
        legacy_uninstalled = {
            ("unix-archive", "rmux-package-v2"): {"package-manifest"},
            ("windows-archive", "rmux-windows-package-v2"): {
                "package-manifest", "installer-windows", "conpty-runtime-manifest",
                "conpty-license", "conpty-dll", "conpty-dll-helper",
                "conpty-openconsole", "conpty-openconsole-helper",
            },
        }
        if version == "0.10.0" and wire == 8:
            # v0.10.0 predates the manifest and bundled Windows ConPTY files;
            # its Windows installer also does not install itself. Keep these
            # historical exceptions local to migration, not current packages.
            omitted = legacy_uninstalled.get((self.variant, self.metadata.get("package_layout")), set())
            self.descriptor = {**self.descriptor, "entries": [entry for entry in self.descriptor["entries"]
                               if entry["id"] not in omitted]}
        executable_entries = {entry["id"]: placement for entry, placement
                              in layout.entries(self.descriptor, self.variant)}
        self.executables = [(executable_entries[name]["path"], executable_entries[name]["install"])
                            for name in ("cli", "daemon", "helper")]
        self.cli = self.prefix / self.executables[0][1]
        self.commit = commit
        self.baseline = self.verify()

    @staticmethod
    def absolute(value):
        path = Path(value)
        if not path.is_absolute() or path.resolve(strict=True) != path:
            raise ValueError("absolute, unredirected package paths required")
        return path

    def verify_candidate_identity(self, path):
        data = path.read_bytes()
        markers = set(re.findall(rb"RMUX_BUILD_IDENTITY_V1:([0-9a-f]{40}):[0-9a-f]{64}:(macos|linux|windows):END", data))
        host = {"darwin": "macos", "linux": "linux", "win32": "windows"}[sys.platform]
        if markers:
            if markers != {(self.commit.encode(), host.encode())}:
                raise ValueError("installed candidate lacks exact embedded commit/OS identity")
            return
        # Ordinary packages carry the canonical commit, without a verifier that
        # restricts child launches to the original compilation directory.
        embedded_commit(data, self.commit)
        target = self.metadata.get("target", "")
        magic = {"darwin": (b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca"),
                 "linux": (b"\x7fELF",), "win32": (b"MZ",)}[sys.platform]
        target_os = {"darwin": "-apple-darwin", "linux": "-linux-", "win32": "-windows-"}[sys.platform]
        if not isinstance(target, str) or target_os not in target or not data.startswith(magic):
            raise ValueError("installed canonical candidate differs from native OS")

    def verify(self):
        try:
            layout.check_installed(self.descriptor, self.variant, self.root, self.prefix)
        except layout.LayoutError as error:
            raise ValueError("installed package differs: " + str(error)) from error
        images = []
        for source, relative in self.executables:
            path = self.prefix / relative
            if path.resolve(strict=True) != path or file_hash(path) != file_hash(self.root / source):
                raise ValueError("migration executable escaped installed package")
            if not self.fixture and self.version == "0.11.0" and not hasattr(self, "baseline"):
                self.verify_candidate_identity(path)
            images.append({"path": str(path), "sha256": file_hash(path)})
        record = {"path": str(self.cli), "sha256": images[0]["sha256"],
                "installed_path": str(self.cli), "installed_sha256": images[0]["sha256"],
                "package_root": str(self.root), "prefix": str(self.prefix),
                "version": self.version, "wire": self.wire, "images": images,
                "metadata_sha256": file_hash(self.root / "share/rmux/artifact-metadata.json"),
                "commit": self.commit}
        if hasattr(self, "baseline") and record != self.baseline:
            raise ValueError("installed package changed since migration admission")
        return record

    def call(self, arguments, environment, timeout=15):
        origin = self.verify()
        argv = [str(self.cli), *map(str, arguments)]
        result = subprocess.run(argv, env=environment, stdin=subprocess.DEVNULL, capture_output=True, text=True,
                                timeout=timeout, cwd=self.prefix)
        if self.verify() != origin:
            raise ValueError("installed package changed during command")
        return {"client": origin, "command": argv, "returncode": result.returncode,
                "stdout": result.stdout, "stderr": result.stderr}

    def verify_process(self, pid):
        images = self.verify()["images"]
        path = native_image_path(pid)
        expected = next((image for image in images if Path(image["path"]) == path), None)
        if expected is None or file_hash(path) != expected["sha256"]:
            raise ValueError("running daemon image is outside its installed package")
        return {"pid": pid, **expected}
