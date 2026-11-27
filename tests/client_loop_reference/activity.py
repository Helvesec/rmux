"""Exact acknowledged same-second native discriminant frozen against tmux 3.7b."""
import math
import os
import time
from support import Client, Skip, stamp

EXPECTED = {"activity": {"normalized": "D=lo,E;hi,E;|T=hi,0;lo,1;|R=lo,0;hi,1;"}}

def wait_wall(target):
    while True:
        left = target - time.time()
        if left <= 0:
            return
        time.sleep(min(left, 0.01))

def parse_activity(raw):
    sections = dict(piece.split("=", 1) for piece in raw.split("|"))
    return {k: [row.split(",") for row in sections[k].split(";") if row] for k in ("D", "T", "R")}

def normalize_activity(parsed, names, epoch):
    parts = []
    for key in ("D", "T", "R"):
        parts.append(key + "=" + "".join(
            f"{names[name]},{'E' if key == 'D' and int(value) == epoch else value};"
            for name, value in parsed[key]))
    return "|".join(parts)

def activity(c):
    c.new("A")
    c.new("B")
    terminals = [Client(c, session) for session in ("A", "B")]
    rows = c.wait_clients(terminals)
    lo, hi = sorted(terminals, key=lambda client: client.tty)
    names = {lo.tty: "lo", hi.tty: "hi"}
    c.require(lo.tty != hi.tty, "distinct externally recorded OS slave paths")
    for client in terminals:
        row = next(row for row in rows if row[0] == str(client.p.pid))
        c.require(row[1] == client.tty and row[2] == client.tty,
                  "client name and tty equal independently captured OS path", actual=row, expected=client.tty)
    c.r["external_roles"] = {role: {"tty": tty, "pid": next(x.p.pid for x in terminals if x.tty == tty)}
                              for tty, role in names.items()}
    c.q("bind-key", "-n", "C-g", "set-option", "-g", "@lb_ack", "lo")
    c.q("bind-key", "-n", "C-k", "set-option", "-g", "@lb_ack", "hi")
    c.r["timing_attempts"] = []
    for attempt_number in range(1, 4):
        c.q("set-option", "-g", "@lb_ack", "pending")
        epoch = math.ceil(time.time()) + 1
        attempt = {"number": attempt_number, "epoch": epoch, "events": []}
        c.r["timing_attempts"].append(attempt)
        valid = True
        for role, client, data, offset, limit in (("lo", lo, b"\x07", 0.100, 0.300),
                                                 ("hi", hi, b"\x0b", 0.500, 0.800)):
            wait_wall(epoch + offset)
            event = {"role": role, "scheduled_wall": epoch + offset,
                     "ack_deadline": epoch + limit, "write_before": stamp()}
            event["write_bytes"] = os.write(client.master, data)
            event["write_after"] = stamp()
            attempt["events"].append(event)
            while time.time() < epoch + limit:
                ack = c.q("show-options", "-gqv", "@lb_ack", timeout=min(0.15, epoch + limit - time.time()))
                if ack == role:
                    event["ack"] = stamp()
                    break
                time.sleep(0.005)
            ack = event.get("ack")
            event["qualified"] = bool(ack and epoch + offset <= event["write_before"]["wall"] <=
                                      event["write_after"]["wall"] <= ack["wall"] < epoch + limit)
            valid = valid and event["qualified"]
        raw = c.q("display-message", "-p", "-t", "A",
                  "D=#{L/n:#{client_name},#{client_activity};}"
                  "|T=#{L/t:#{client_name},#{loop_last_flag};}"
                  "|R=#{L/tr:#{client_name},#{loop_last_flag};}")
        attempt["capture"] = raw
        parsed = parse_activity(raw)
        attempt["parsed"] = parsed
        same_second = len(parsed["D"]) == 2 and {name for name, _ in parsed["D"]} == set(names) and all(
            int(value) == epoch for _, value in parsed["D"])
        attempt["displayed_same_external_second"] = same_second
        attempt["qualified"] = valid and same_second
        if not attempt["qualified"]:
            attempt["reason"] = "timing precondition failed; no ordering verdict"
            continue
        # Once preconditions qualify, an ordering mismatch is final, never retried.
        normalized = normalize_activity(parsed, names, epoch)
        c.r["observations"] = {"raw": raw, "normalized": normalized, "epoch": epoch}
        c.r["qualified"] = True
        c.require(normalized == EXPECTED["activity"]["normalized"],
                  "full timestamp order and complete reverse comparator", actual=normalized,
                  expected=EXPECTED["activity"]["normalized"])
        c.handshake()
        return
    raise Skip("three attempts exhausted distinct acknowledged same-second timing preconditions")
