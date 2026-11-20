"""Validate an explicitly requested persistence version transition before setup.
Image provenance and native process observation remain the caller's obligations.
"""
import hashlib
import pathlib
import re
import subprocess
import sys


def verify_images(images):
    """Compare consumers to their initial pins; never refresh a qualified digest."""
    for image in images:
        try:
            digest = hashlib.sha256(pathlib.Path(image["path"]).read_bytes()).hexdigest()
        except OSError as exc:
            raise ValueError("persistence transition image changed: " + image["path"]) from exc
        if digest != image["sha256"]:
            raise ValueError("persistence transition image changed: " + image["path"])


def verify_transition(transition):
    if transition is not None:
        verify_images([transition["saving"], transition["restoring"]])


def image_version(image):
    text = subprocess.check_output([str(image), "--version"], text=True, timeout=30).strip()
    match = re.fullmatch(r"rmux (0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", text)
    if match is None:
        raise ValueError("unqualified rmux version: " + text)
    return tuple(map(int, match.groups()))


def validate_transition(saving_image, restoring_image, transition):
    if transition is None:
        return None
    if transition not in ("same", "upgrade", "rollback"):
        raise ValueError("unknown persistence transition: " + transition)
    paths = [pathlib.Path(p).resolve(strict=True) for p in (saving_image, restoring_image)]
    digests = [hashlib.sha256(p.read_bytes()).hexdigest() for p in paths]
    if transition != "same" and digests[0] == digests[1]:
        raise ValueError(transition + " requires distinct images")
    versions = [image_version(p) for p in paths]
    if digests != [hashlib.sha256(p.read_bytes()).hexdigest() for p in paths]:
        raise ValueError("persistence transition image changed during qualification")
    valid = {"same": versions[0] == versions[1],
             "upgrade": versions[0] < versions[1],
             "rollback": versions[0] > versions[1]}[transition]
    if not valid:
        raise ValueError(transition + " contradicts effective image versions")
    return {"kind": transition,
            "saving": {"path": str(paths[0]), "sha256": digests[0], "version": versions[0]},
            "restoring": {"path": str(paths[1]), "sha256": digests[1], "version": versions[1]}}


if __name__ == "__main__":
    # The generated plugin wrapper checks the same initial pins before each exec.
    verify_images([{"path": sys.argv[1], "sha256": sys.argv[2]},
                   {"path": sys.argv[3], "sha256": sys.argv[4]}])
