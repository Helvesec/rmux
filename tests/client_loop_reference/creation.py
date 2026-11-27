"""Exact control-first / first-OSC discriminant frozen against tmux 3.7b."""
from support import Client, stamp

EXPECTED = {"creation": {"title": "LB1|C,1,0;P,0,1;|END", "index": "C,0;P,1;"}}

def creation(c):
    c.new("C")
    c.new("P", rows=30, cols=100)
    c.q("set-option", "-g", "set-titles-string",
        "LB1|#{L:#{client_session},#{client_control_mode},#{loop_last_flag};}|END")
    c.q("set-option", "-g", "set-titles", "on")
    control = Client(c, "C", control=True)
    c.wait_clients([control])
    c.r["control_only_verified"] = stamp()
    terminal = Client(c, "P", rows=30, cols=100)
    first = c.until(lambda: terminal.drains[0].first, "first complete OSC 0/2 title", 3)
    # Freeze the first complete record, including every preceding byte. Never
    # look for an expected marker or permit a subsequent record to repair it.
    prefix = bytes(terminal.drains[0].raw[:first["end"]])
    (c.out / "first-title-prefix.bin").write_bytes(prefix)
    c.r["observations"] = {"first_title": first, "prefix_path": str(c.out / "first-title-prefix.bin")}
    title_ok = first["payload"] == EXPECTED["creation"]["title"]
    c.r["assertions"].append({"description": "first title canonical creation order",
                               "passed": title_ok, "expected": EXPECTED["creation"]["title"],
                               "actual": first["payload"]})
    c.wait_clients([control, terminal])
    index = c.q("display-message", "-p", "-t", "P", "#{L/i:#{client_session},#{loop_last_flag};}")
    c.r["observations"]["index"] = index
    index_ok = index == EXPECTED["creation"]["index"]
    c.r["assertions"].append({"description": "explicit /i canonical creation order",
                               "passed": index_ok, "expected": EXPECTED["creation"]["index"], "actual": index})
    c.r["qualified"] = True
    c.require(title_ok and index_ok, "both creation-order assertions pass")
    c.handshake()
