#!/usr/bin/env python3
"""Write a two-week SIMULATED ledger for previewing the dashboard.

    scripts/demo-ledger.py data/demo-ledger.jsonl
    cargo run -- report -l data/demo-ledger.jsonl --html data/dashboard.html

The trades are random, not results from any real strategy.
"""
import json
import random
import sys
from datetime import datetime, timedelta, timezone

random.seed(7)
out = open(sys.argv[1] if len(sys.argv) > 1 else "data/demo-ledger.jsonl", "w")
t = datetime(2026, 10, 14, tzinfo=timezone.utc)
end = t + timedelta(days=14)
cash, open_pos, n = 1000.0, {}, 0
tokens = ["SOL", "BONK", "JUP", "WIF"]


def emit(ev):
    out.write(json.dumps(ev) + "\n")


def equity():
    return cash + sum(p["value"] for p in open_pos.values())


while t < end:
    t += timedelta(minutes=5)
    for sym, p in list(open_pos.items()):
        p["value"] *= 1 + random.gauss(0.0001, 0.012)
        pnl_pct = (p["value"] - p["cost"]) / p["cost"] * 100
        reason = None
        if pnl_pct <= -8:
            reason = "stop_loss"
        elif pnl_pct >= 20:
            reason = "take_profit"
        elif random.random() < 0.006:
            reason = "signal"
        if reason:
            proceeds = p["value"] - 0.01
            pnl = proceeds - p["cost"]
            cash += proceeds
            emit({"event": "close", "ts": t.isoformat(), "symbol": sym, "mint": sym, "reason": reason,
                  "qty": 1.0, "proceeds": proceeds, "fee": 0.01, "cost": p["cost"], "pnl": pnl,
                  "pnl_pct": pnl / p["cost"] * 100, "price": proceeds, "price_impact_pct": 0.05,
                  "held_secs": int((t - p["opened"]).total_seconds()), "route": "Demo"})
            del open_pos[sym]
    if random.random() < 0.02:
        n += 1
        sym = random.choice(tokens)
        sid = f"demo-{n}"
        if sym in open_pos:
            emit({"event": "reject", "ts": t.isoformat(), "signal_id": sid, "token": sym, "side": "buy",
                  "reason": "already holding this token"})
        elif len(open_pos) >= 3:
            emit({"event": "reject", "ts": t.isoformat(), "signal_id": sid, "token": sym, "side": "buy",
                  "reason": "max open positions reached"})
        else:
            cash -= 100.01
            open_pos[sym] = {"cost": 100.01, "value": 100.0, "opened": t}
            emit({"event": "open", "ts": t.isoformat(), "signal_id": sid, "symbol": sym, "mint": sym,
                  "spend": 100.0, "fee": 0.01, "qty": 1.0, "price": 100.0, "price_impact_pct": 0.05,
                  "route": "Demo"})
    emit({"event": "equity", "ts": t.isoformat(), "cash": cash, "equity": equity(),
          "day_pnl": 0.0, "open_positions": len(open_pos)})
