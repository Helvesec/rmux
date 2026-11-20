"""Prospective Web admission comparisons on supplied data, never a Web runner."""
import base64
import binascii
import hashlib
import json
import math
import re


def require(condition, message):
    if not condition:
        raise ValueError(message)


def finite(value):
    return type(value) in (int, float) and math.isfinite(value)


def validate_profile(profile):
    require(profile["schema"] == 1 and profile["status"] == "PROSPECTIVE_NOT_MEASURED", "Web profile identity")
    require(profile["failure_classes"] == ["token", "origin", "proof", "pin", "capability"],
            "failure class omitted/reordered")
    require(profile["topologies"] == ["direct-loopback", "loopback-relay"], "topology omitted")
    require(profile["backoff_states"] == ["fresh", "same-escalation", "locked"], "backoff omitted")
    require(profile["milestone"] == "first-upgrade-byte-to-observed-close", "milestone differs")
    require(profile["stages"] == ["HTTP upgrade/origin", "hello/token/capability",
                                   "cryptographic proof", "authenticated PIN/admission"], "admission stage omitted")
    require(type(profile["clock_resolution_max_ns"]) is int and
            0 < profile["clock_resolution_max_ns"] <= 100000, "clock resolution")
    for field in ("topology_contract", "backoff_contract", "stage_rule", "sample_order",
                  "canonical_contract", "crypto_contract", "pin_exception"):
        require(isinstance(profile[field], str) and profile[field].strip(), "missing Web contract: " + field)
    require(profile["warmup_per_stratum"] == 50 and profile["retain_all_attempts"] is True, "warmup/attempt policy")
    stats = profile["statistics"]
    require(stats["method"] == "two-sample-CDF-DKW-upper-bound", "statistic changed")
    require(type(stats["minimum_per_class_stratum"]) is int and stats["minimum_per_class_stratum"] >= 50000,
            "sample floor")
    require(stats["family_comparisons"] == 60, "all pairwise strata must be compared")
    require(stats["family_alpha"] == 0.01 and stats["beta_equal_distribution"] == 0.01 and
            stats["cdf_margin"] == 0.05, "frozen error levels/margin changed")
    require(profile["ordinary_close"] == {"code": 4000, "reason": "handshake_rejected"}, "ordinary close")
    require(profile["authenticated_missing_pin_close"] == {"code": 4008, "reason": "pin_required"}, "PIN exception")
    require(profile["random_fields"] == {"server_nonce": 16, "server_public": 32, "server_ml_kem_ct": 1088},
            "random field format/length")
    require(profile["canonical_fields"] == ["direction", "opcode", "wire_bytes", "deterministic_fields", "close"],
            "canonical projection")
    require(profile["canaries"] == ["deterministic-reason", "crypto-reuse", "close-code", "time-shift"],
            "missing Web falsifier")
    crypto = profile["crypto_statistics"]
    require(crypto["method"] == "simultaneous-bit-marginal-equivalence"
            and crypto["minimum_per_class_stratum"] == 100000 and crypto["comparisons"] == 60
            and crypto["bits"] == 9088 and crypto["family_alpha"] == 0.01 and crypto["margin"] == 0.05
            and crypto["beta_equal_distribution"] == 0.01
            and crypto["overall_two_statistical_families_error_bound"] == 0.02,
            "crypto distribution profile")


def decode(value, length):
    require(isinstance(value, str) and re.fullmatch(r"[A-Za-z0-9_-]+", value), "noncanonical base64url")
    try:
        raw = base64.b64decode(value + "=" * (-len(value) % 4), altchars=b"-_", validate=True)
    except (ValueError, binascii.Error) as error:
        raise ValueError("invalid base64url") from error
    require(len(raw) == length and base64.urlsafe_b64encode(raw).decode().rstrip("=") == value,
            "random field length/encoding")
    return raw


def no_duplicate_fields(pairs):
    require(len({key for key, _ in pairs}) == len(pairs), "duplicate JSON field")
    return dict(pairs)


def transcript(profile, sample, seen_random):
    """Project actual supplied server-to-peer frames, retaining unknown fields.

    Client stimuli are separately pinned; equality of attacker-chosen inputs is
    not required. Removing arbitrary server fields would hide reason leakage.
    """
    require(sample["failure_class"] in profile["failure_classes"] or sample["failure_class"] == "missing-pin",
            "unknown admission class")
    require(type(sample["token_proof_verified"]) is bool and type(sample["pin_absent"]) is bool,
            "PIN exception prerequisites not attested")
    pin_exception = sample["token_proof_verified"] and sample["pin_absent"]
    require((sample["failure_class"] == "missing-pin") == pin_exception, "unauthenticated PIN exception")
    expected = profile["authenticated_missing_pin_close"] if pin_exception else profile["ordinary_close"]
    require(sample["close"] == expected, "close code/reason diverges")
    require(isinstance(sample["frames"], list), "frames absent")
    frames = []
    current_random = set()
    for frame in sample["frames"]:
        require(frame["direction"] == "server-to-peer" and frame["opcode"] in ("text", "binary"), "frame direction/opcode")
        require(isinstance(frame["payload_hex"], str) and re.fullmatch("(?:[0-9a-f]{2})*", frame["payload_hex"]),
                "frame bytes not canonical hex")
        raw = bytes.fromhex(frame["payload_hex"])
        require(len(raw) == frame["wire_bytes"], "frame length falsified")
        if frame["opcode"] == "text":
            fields = json.loads(raw.decode(), object_pairs_hook=no_duplicate_fields)
            require(isinstance(fields, dict), "handshake fields")
            if fields.get("type") == "challenge":
                require(set(profile["random_fields"]) <= set(fields), "challenge random field missing")
            for name, size in profile["random_fields"].items():
                if name in fields:
                    value = decode(fields[name], size)
                    identity = (name, hashlib.sha256(value).hexdigest())
                    require(identity not in seen_random and identity not in current_random, "cryptographic value reused")
                    current_random.add(identity)
                    fields[name] = {"decoded_bytes": size, "encoding": "base64url-no-pad"}
        else:
            # Encrypted rejection records still expose opcode and exact length;
            # their authenticity/format is a required collector observation.
            require(frame.get("authenticated") is True, "unverified encrypted frame")
            plaintext = bytes.fromhex(frame["plaintext_hex"])
            fields = json.loads(plaintext.decode(), object_pairs_hook=no_duplicate_fields)
            require(isinstance(fields, dict), "decrypted deterministic fields missing")
        frames.append({"direction": frame["direction"], "opcode": frame["opcode"],
                       "wire_bytes": len(raw), "deterministic_fields": fields})
    seen_random.update(current_random)
    return {"frames": frames, "close": sample["close"]}


def transcript_family(profile, samples):
    """Canonical comparison for a supplied five-class block, not a native verdict."""
    require(len(samples) == len(profile["failure_classes"]) and
            {s["failure_class"] for s in samples} == set(profile["failure_classes"]),
            "admission class absent/extra")
    seen = set()
    projected = [transcript(profile, sample, seen) for sample in samples]
    require(all(value == projected[0] for value in projected), "deterministic transcript differs")
    return {"canonical_equivalence": True, "native_execution_proven": False}


def cdf_distance(left, right):
    require(left and right and all(finite(x) and x >= 0 for x in (*left, *right)), "nonfinite/negative/empty timing")
    a, b = sorted(left), sorted(right)
    i = j = 0
    distance = 0.0
    for value in sorted(set(a + b)):
        while i < len(a) and a[i] <= value:
            i += 1
        while j < len(b) and b[j] <= value:
            j += 1
        distance = max(distance, abs(i / len(a) - j / len(b)))
    return distance


def timing(profile, left, right):
    stats = profile["statistics"]
    n_min = stats["minimum_per_class_stratum"]
    require(len(left) >= n_min and len(right) >= n_min, "insufficient samples")
    alpha = stats["family_alpha"] / stats["family_comparisons"]
    uncertainty = sum(math.sqrt(math.log(4 / alpha) / (2 * n)) for n in (len(left), len(right)))
    upper = cdf_distance(left, right) + uncertainty
    require(upper <= stats["cdf_margin"], "temporal equivalence not demonstrated")
    return {"cdf_upper_bound": upper, "margin": stats["cdf_margin"], "alpha_per_comparison": alpha}


def random_distribution(profile, left, right):
    """Simultaneous bit-marginal equivalence; not a cryptographic RNG proof.

    Collector must additionally check generation freshness and the protocol's
    cryptographic validators. Constant or repeated values fail freshness first.
    Inputs are {n, ones:[count per decoded bit]} summaries bound to raw samples.
    """
    validate_profile(profile)
    stats = profile["crypto_statistics"]
    comparisons, alpha = stats["comparisons"], stats["family_alpha"]
    minimum, margin = stats["minimum_per_class_stratum"], stats["margin"]
    require(all(type(x["n"]) is int and x["n"] >= minimum for x in (left, right)), "random sample floor")
    require(len(left["ones"]) == len(right["ones"]) == stats["bits"], "random bit population")
    width = len(left["ones"])
    error = alpha / (comparisons * width)
    uncertainty = sum(math.sqrt(math.log(4 / error) / (2 * x["n"])) for x in (left, right))
    for a, b in zip(left["ones"], right["ones"]):
        require(type(a) is int and 0 <= a <= left["n"] and type(b) is int and 0 <= b <= right["n"],
                "invalid random bit count")
        require(abs(a / left["n"] - b / right["n"]) + uncertainty <= margin,
                "cryptographic field distribution differs")
    return {"bit_marginal_equivalence": True, "cryptographic_security_proven": False}
