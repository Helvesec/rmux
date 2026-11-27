"""Release HTTP acceptance contract; gh is inert and never reaches GitHub."""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SHA = "a" * 40
TAG_SHA = "b" * 40
TAG = "v0.11.0"
BODY = {"tag_name": TAG, "target_commitish": SHA}
GH_STUB = r'''
import json, os, sys
from pathlib import Path

args = sys.argv[1:]
endpoint = args[-1]
config = json.loads(os.environ["RELEASE_REF_FIXTURE"])
with Path(os.environ["RELEASE_REF_TRACE"]).open("a") as trace:
    trace.write(json.dumps(args) + "\n")
if endpoint.endswith("/git/ref/tags/" + config["tag"]):
    print(json.dumps(config["ref"]))
elif "/git/tags/" in endpoint:
    key = endpoint.rsplit("/", 1)[1]
    if key not in config["tags"]:
        sys.exit(99)
    print(json.dumps(config["tags"][key]))
elif endpoint.endswith("/releases/tags/" + config["tag"]):
    response = config["release"]
    if "--include" in args or "-i" in args:
        sys.stdout.write(response["headers"])
    sys.stdout.write(response["body"])
    sys.stderr.write(response.get("stderr", ""))
    sys.exit(response["exit"])
else:
    sys.exit(99)
'''


def response(status=200, body=BODY, exit_code=0, headers=None, stderr=""):
    return {
        "headers": headers if headers is not None else
        f"HTTP/2.0 {status} Fixture\r\nContent-Type: application/json\r\n\r\n",
        "body": json.dumps(body) if not isinstance(body, str) else body,
        "exit": exit_code,
        "stderr": stderr,
    }


class ReleaseRefHTTPTests(unittest.TestCase):
    def setUp(self):
        audit = ROOT / ".test-artifacts" / "verify-release-ref-http"
        audit.mkdir(parents=True, exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(dir=audit)
        self.addCleanup(self.directory.cleanup)
        self.fixture = Path(self.directory.name)
        stub = self.fixture / "gh"
        stub.write_text(f"#!{sys.executable}\n" + GH_STUB, encoding="utf-8")
        stub.chmod(0o755)

    def verify(self, release=None, ref=None, tags=None, tag=TAG, expected=SHA):
        trace = self.fixture / "calls.jsonl"
        trace.unlink(missing_ok=True)
        config = {
            "tag": tag,
            "ref": ref or {"object": {"type": "tag", "sha": TAG_SHA}},
            "tags": tags if tags is not None else {
                TAG_SHA: {"object": {"type": "commit", "sha": SHA},
                          "verification": {"verified": True}},
            },
            "release": release if release is not None else response(),
        }
        env = dict(os.environ, PATH=str(self.fixture) + os.pathsep + os.environ["PATH"],
                   RELEASE_REF_FIXTURE=json.dumps(config), RELEASE_REF_TRACE=str(trace),
                   TMPDIR=str(self.fixture))
        result = subprocess.run(
            ["/bin/bash", str(ROOT / "scripts/verify-release-ref.sh"),
             "fixture/repository", tag, expected],
            cwd=ROOT, env=env, capture_output=True, text=True, timeout=10,
        )
        calls = [json.loads(line) for line in trace.read_text().splitlines()]
        return result, calls

    def assertAccepted(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("release-ref-ok", result.stdout)

    def assertRejected(self, result):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("release-ref-ok", result.stdout)

    def test_valid_200_and_explicit_404(self):
        for release in (response(), response(404, {"message": "Not Found"}, 1),
                        response(headers="HTTP/1.1 200 OK\nContent-Type: application/json\n\n")):
            with self.subTest(release=release):
                self.assertAccepted(self.verify(release)[0])

    def test_http_errors_fail_closed(self):
        for status in (401, 403, 429, 500, 503):
            with self.subTest(status=status):
                self.assertRejected(self.verify(response(status, {"message": "error"}, 1))[0])

    def test_transport_authentication_and_timeout_fail_closed(self):
        for release in (
            response(body="", exit_code=1, headers="", stderr="transport failed\n"),
            response(body="", exit_code=124, headers="", stderr="timeout\n"),
            response(body="", exit_code=4, headers="", stderr="authentication required\n"),
            response(body="", exit_code=1, headers="", stderr="gh: HTTP 404\n"),
            response(404, body="", exit_code=124, stderr="timeout\n"),
            response(200, exit_code=1, stderr="truncated transfer\n"),
        ):
            with self.subTest(release=release):
                self.assertRejected(self.verify(release)[0])

    def test_malformed_http_response_fails_closed(self):
        for release in (
            response(headers=""),
            response(headers="garbage 404\n\n"),
            response(headers="HTTP/2.0 200 OK\n"),
            response(503, exit_code=0),
            response(404, headers="HTTP/2.0 4040 Invalid\r\n\r\n", exit_code=1),
        ):
            with self.subTest(release=release):
                self.assertRejected(self.verify(release)[0])

    def test_200_json_and_release_identity_remain_required(self):
        for body in ("{broken", "", [], {}, None,
                     {"tag_name": TAG, "target_commitish": "main"},
                     {"tag_name": "v0.10.0", "target_commitish": SHA},
                     {"tag_name": TAG, "target_commitish": "c" * 40}):
            with self.subTest(body=body):
                self.assertRejected(self.verify(response(body=body))[0])

    def test_signed_nested_tag_and_case_normalization_remain_valid(self):
        tags = {
            TAG_SHA: {"object": {"type": "tag", "sha": "c" * 40},
                      "verification": {"verified": True}},
            "c" * 40: {"object": {"type": "commit", "sha": SHA.upper()},
                       "verification": {"verified": True}},
        }
        result, calls = self.verify(response(body={"tag_name": TAG,
                                                   "target_commitish": SHA.upper()}),
                                    tags=tags, expected=SHA.upper())
        self.assertAccepted(result)
        self.assertEqual(sum("/git/tags/" in call[-1] for call in calls), 2)

    def test_tag_signature_peeling_and_sha_fail_before_release_lookup(self):
        for ref, tags in (
            ({"object": {"type": "commit", "sha": SHA}}, {}),
            (None, {TAG_SHA: {"object": {"type": "commit", "sha": SHA},
                             "verification": {"verified": False, "reason": "unsigned"}}}),
            (None, {TAG_SHA: {"object": {"type": "commit", "sha": SHA}}}),
            (None, {TAG_SHA: {"object": {"type": "tag", "sha": TAG_SHA},
                             "verification": {"verified": True}}}),
            (None, {TAG_SHA: {"object": {"type": "tree", "sha": SHA},
                             "verification": {"verified": True}}}),
            (None, {TAG_SHA: {"object": {"type": "commit", "sha": "d" * 40},
                             "verification": {"verified": True}}}),
        ):
            with self.subTest(ref=ref, tags=tags):
                result, calls = self.verify(ref=ref, tags=tags)
                self.assertRejected(result)
                self.assertFalse(any("/releases/tags/" in call[-1] for call in calls))

    def test_tag_peel_depth_remains_bounded(self):
        keys = [f"{n:040x}" for n in range(17)]
        tags = {key: {"object": {"type": "tag", "sha": keys[(i + 1) % len(keys)]},
                      "verification": {"verified": True}} for i, key in enumerate(keys)}
        result, calls = self.verify(ref={"object": {"type": "tag", "sha": keys[0]}}, tags=tags)
        self.assertRejected(result)
        self.assertEqual(sum("/git/tags/" in call[-1] for call in calls), 16)
        self.assertFalse(any("/releases/tags/" in call[-1] for call in calls))


if __name__ == "__main__":
    unittest.main()
