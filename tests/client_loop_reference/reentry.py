"""Exact seeded explicit-E native discriminant, frozen against tmux 3.7b."""
import time
from support import Client, Failure, identity, stamp

EXPECTED = {"reentry": {"expanded": "AUTO=LBseed|NAME=LBseed|END", "name": "LBseed"}}

def reentry(c):
    c.new("N", name="LBseed")
    control = Client(c, "N", control=True)
    c.wait_clients([control])
    c.q("set-option", "-w", "-t", "N:0", "automatic-rename-format", "LBseed")
    c.q("set-option", "-w", "-t", "N:0", "automatic-rename", "on")
    seed = c.q("display-message", "-p", "-t", "N:0", "#{window_name}")
    enabled = c.q("show-options", "-wqv", "-t", "N:0", "automatic-rename")
    c.require(seed == "LBseed" and enabled == "on", "seeded fixed point with automatic rename enabled",
              name=seed, enabled=enabled)
    c.handshake()
    c.r["qualified"] = True
    c.r["observations"] = {"seed": seed, "enabled": enabled, "before_trigger": stamp()}
    trigger_start = time.monotonic()
    def trigger(*args):
        left = 3 - (time.monotonic() - trigger_start)
        if left <= 0:
            raise Failure("automatic-name trigger exceeded three seconds")
        return c.q(*args, timeout=left)
    try:
        trigger("set-option", "-w", "-t", "N:0", "automatic-rename-format", "#{L:#{window_name}}")
        trigger("set-option", "-w", "-t", "N:0", "automatic-rename", "on")
        expanded = trigger("display-message", "-p", "-t", "N:0",
                           "AUTO=#{E:automatic-rename-format}|NAME=#{window_name}|END")
        c.r["observations"]["expanded"] = expanded
        c.require(expanded == EXPECTED["reentry"]["expanded"], "explicit E expansion returns seeded fixed point",
                  actual=expanded, expected=EXPECTED["reentry"]["expanded"])
        time.sleep(0.750)
        c.r["observations"]["after_750ms"] = stamp()
        c.handshake()
        name = c.q("display-message", "-p", "-t", "N:0", "#{window_name}")
        c.r["observations"]["name_after_750ms"] = name
        c.require(name == "LBseed", "name remains seeded fixed point after 750 ms", actual=name)
    finally:
        c.r["observations"]["trigger_elapsed"] = time.monotonic() - trigger_start
        c.r["observations"]["server_before_cleanup"] = identity(c.server["pid"])
