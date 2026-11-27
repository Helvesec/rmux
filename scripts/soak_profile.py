"""Version 1 workload; brief runs never certify the long qualification."""
import math


def soak_profile(mode="long", duration=None):
    if mode not in {"brief", "long"}:
        raise ValueError("unknown soak mode")
    duration = (10 if mode == "brief" else 86400) if duration is None else duration
    if (type(duration) not in (int, float) or not math.isfinite(duration)
            or duration < (1 if mode == "brief" else 86400)
            or duration != int(duration)):
        raise ValueError("soak duration must be whole seconds; long requires at least 24h")
    duration = int(duration)
    rates = {"input": 2, "capture": 2, "churn": 1}
    return {"schema": 1, "id": "soak/v1", "mode": mode, "duration_seconds": duration,
            "topology": {"sessions": 2, "windows_per_session": 2,
                         "panes_per_window": 2, "clients": 2},
            "concurrency": 2, "rate_scope": "per-worker", "rates_per_second": rates,
            "minimum_operations": {name: rate * duration * 2 for name, rate in rates.items()},
            "maximum_skips": 0, "command_timeout_seconds": 10,
            "postconditions": ["topology", "roundtrip", "clients", "cleanup"]}
