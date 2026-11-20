"""Byte witnesses owned by one qualification, never by an executable consumer."""
import hashlib
from pathlib import Path
from types import SimpleNamespace


class Qualification:
    def __init__(self, g):
        self.g = g
        self.files = {}
        self.inputs = {}

    def witness(self, path, keep=False):
        path = Path(path)
        resolved = path.resolve(strict=True)
        before = self.g.stamp(path.stat())
        previous = self.files.get(path)
        if previous:
            if previous[0:2] != (resolved, before):
                raise self.g.IdentityError(f'linked unit bytes changed during qualification: {path}')
            return previous
        data = getattr(self.g, 'read_bytes', Path.read_bytes)(path) if keep else None
        sha = hashlib.sha256(data).hexdigest() if keep else self.g.file_hash(path)
        if (path.resolve(strict=True) != resolved
                or self.g.stamp(path.stat()) != before):
            raise self.g.IdentityError(f'linked unit bytes changed during qualification: {path}')
        result = (resolved, before, sha, data)
        self.files[path] = result
        return result

    def file_hash(self, path, keep=False):
        return self.witness(path, keep)[2]

    def tracked_input(self, path, source, head):
        # A transitive graph repeatedly visits the same workspace manifests.
        # Bind their Git proof to this qualification's byte witness; consumers
        # never inherit this table. Recheck bytes as well as stamps: successive
        # same-size writes can share a native clock tick.
        witness = self.witness(path)
        key = (Path(path), Path(source), head)
        if key not in self.inputs:
            blob = self.g.tracked_input(path, source, head)
            self.inputs[key] = blob
        if (self.g.file_hash(path) != witness[2]
                or self.witness(path)[:3] != witness[:3]):
            raise self.g.IdentityError('pinned input changed during qualification')
        return self.inputs[key]

    def read_bytes(self, path):
        result = self.witness(path, keep=True)
        if result[3] is None:
            # A hash-only witness needs bytes only if a later marker/body check
            # requests them. It must still match the earlier exact stamp/hash.
            data = getattr(self.g, 'read_bytes', Path.read_bytes)(Path(path))
            self.validate()
            if hashlib.sha256(data).hexdigest() != result[2]:
                raise self.g.IdentityError(f'linked unit bytes changed during qualification: {path}')
            result = (*result[:3], data)
            self.files[Path(path)] = result
        return result[3]

    def release(self, path):
        path = Path(path)
        if path in self.files:
            self.files[path] = (*self.files[path][:3], None)

    def validate(self):
        for path, (resolved, stamp, _, _) in self.files.items():
            if (path.resolve(strict=True) != resolved
                    or self.g.stamp(path.stat()) != stamp):
                raise self.g.IdentityError(f'linked unit bytes changed during qualification: {path}')


def scope(g):
    if getattr(g, '_qualification', None) is not None:
        return g
    q = Qualification(g)
    qualified = SimpleNamespace(**vars(g))
    qualified._qualification = q
    qualified.file_hash = q.file_hash
    qualified.read_bytes = q.read_bytes
    qualified.tracked_input = q.tracked_input
    qualified.verify = lambda *args: g.verify(*args, _qualification=q)
    qualified.verify_built = lambda *args: g.support('candidate_identity_retention').verify_built(
        qualified, *args)
    return qualified
