"""D157 controls inherited by the synthetic, isolated pin revision fixture."""
import copy
import hashlib
import json


class PinActivationControls:
    def activate(self, name):
        self.git("-c", "core.hooksPath=/dev/null", "merge", "--no-edit", self.pin_commits[name])

    def reseal_proof(self):
        self.proof_path.write_text(json.dumps(self.evidence))
        checksum = hashlib.sha256(self.proof_path.read_bytes()).hexdigest()
        for row in self.document["entries"]:
            row["proof"]["sha256"] = checksum
        self.seal()

    def test_activation_requires_reachable_commit_not_merely_available_object(self):
        self.append("relative_or_string", activate=False)
        self.git("cat-file", "-e", self.pin_commits["relative_or_string"] + "^{commit}")
        self.assertEqual(self.decode(self.projection, current=True), self.original)
        for current in (False, True):
            with self.subTest(current=current), self.assertRaisesRegex(ValueError, "undeclared"):
                self.decode(self.revised("relative_or_string"), current=current)
        self.activate("relative_or_string")
        self.decode(self.revised("relative_or_string"), current=True)
        with self.assertRaisesRegex(ValueError, "stale"):
            self.decode(self.projection, current=True)

    def test_activation_tracks_each_pin_independently_of_chain_order(self):
        self.append("relative_or_string", activate=False)
        self.append("screen_cleanup", activate=False)
        self.assertEqual(self.decode(self.projection, current=True), self.original)
        self.activate("screen_cleanup")
        self.decode(self.revised("screen_cleanup"), current=True)
        with self.assertRaises(ValueError):
            self.decode(self.revised("relative_or_string"), current=True)
        self.activate("relative_or_string")
        self.decode(self.revised("relative_or_string", "screen_cleanup"), current=True)
        for snapshot in (self.projection, self.revised("relative_or_string")):
            self.assertEqual(self.decode(snapshot), self.original)

    def test_activation_commit_must_be_exact_not_a_cherry_picked_equivalent(self):
        self.append("relative_or_string", activate=False)
        self.git("-c", "core.hooksPath=/dev/null", "cherry-pick", "--no-commit", self.pin_commits["relative_or_string"])
        self.git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "different synthetic pin identity")
        self.assertEqual(self.decode(self.projection, current=True), self.original)
        with self.assertRaises(ValueError):
            self.decode(self.revised("relative_or_string"), current=True)

    def test_activation_without_declaration_rejected_after_pin_commit(self):
        self.activate("relative_or_string")
        with self.assertRaisesRegex(ValueError, "undeclared"):
            self.decode(self.revised("relative_or_string"), current=True)

    def test_pin_commit_missing_invalid_or_omitted_in_proof_fails_closed(self):
        self.append("relative_or_string", activate=False)
        good = copy.deepcopy(self.evidence)
        for value in (None, 1, "HEAD", "f" * 39, "F" * 40, "f" * 41):
            with self.subTest(commit=value):
                self.evidence = copy.deepcopy(good)
                self.evidence["revisions"][0]["pin_commit"] = value
                self.reseal_proof()
                with self.assertRaises(ValueError):
                    self.decode(self.projection, current=True)
        self.evidence = copy.deepcopy(good)
        del self.evidence["revisions"][0]["pin_commit"]
        self.reseal_proof()
        with self.assertRaises(ValueError):
            self.decode(self.projection, current=True)

    def test_unknown_commit_cannot_activate_even_resealed(self):
        self.append("relative_or_string", activate=False)
        self.evidence["revisions"][0]["pin_commit"] = "f" * 40
        self.reseal_proof()
        self.assertEqual(self.decode(self.projection, current=True), self.original)
        with self.assertRaises(ValueError):
            self.decode(self.revised("relative_or_string"), current=True)

    def test_inactive_chain_still_rejects_committed_withdrawal(self):
        self.append("relative_or_string", activate=False)
        self.commit()
        self.document["entries"].clear()
        self.write_chain()
        self.assert_history_rejected(self.projection)
