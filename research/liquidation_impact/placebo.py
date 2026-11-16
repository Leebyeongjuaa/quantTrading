"""Is the edge about liquidations, or just about shorting a falling market?

Re-runs the Aave / ETH-collateral backtest with the same accounts and days, but with the entry
level moved to a price where nothing gets liquidated: P_liq * (1 + shift). The "liquidation"
exit is replaced by the moment the Aave oracle price crosses that level, which is when a real
liquidation would have fired. shift = 0 keeps the real level and checks that the oracle-cross
exit reproduces the real liquidation exits.

  python3 placebo.py            run and print          (trades -> data/placebo_trades.csv)
  python3 placebo.py --report   re-print from the saved trades
"""
import json
import os
import sys

import numpy as np
import pandas as pd
import requests
from eth_abi import encode

import backtest as bt
from snapshots import ORACLE, WETH

SHIFTS = [0.0, 0.01, 0.02, 0.03, 0.05]
CELLS = [(0.001, 0.005), (0.002, 0.0075), (0.003, 0.005)]
ORACLE_TOLERANCE = 0.0002  # accrued interest moves the level by a hair between entry and exit
ORACLE_CACHE = "data/oracle_blocks.json"
URLS = ["https://rpc.mevblocker.io", "https://gateway.tenderly.co/public/mainnet"]


class BlockOracle:
    """Aave oracle ETH price at the end of any block, fetched in batches and cached on disk."""

    def __init__(self):
        raw = json.load(open(ORACLE_CACHE)) if os.path.exists(ORACLE_CACHE) else {}
        self.price = {int(k): v for k, v in raw.items()}
        self.data = "0xb3596f07" + encode(["address"], [WETH]).hex()  # getAssetPrice(WETH)

    def save(self):
        json.dump(self.price, open(ORACLE_CACHE, "w"))

    def fetch(self, blocks):
        missing = [b for b in blocks if b not in self.price]
        for i in range(0, len(missing), 100):
            chunk = missing[i : i + 100]
            payload = [{"jsonrpc": "2.0", "id": b, "method": "eth_call",
                        "params": [{"to": ORACLE, "data": self.data}, hex(b)]} for b in chunk]
            for attempt in range(8):
                try:
                    out = requests.post(URLS[attempt % len(URLS)], json=payload, timeout=120).json()
                    got = {r["id"]: int(r["result"], 16) / 1e8 for r in out if isinstance(r, dict) and r.get("result")}
                    if len(got) == len(chunk):
                        self.price.update(got)
                        break
                except Exception:
                    pass
            else:
                raise RuntimeError("oracle batch failed")

    def first_cross(self, level, after, until):
        """First block in (after, until] whose oracle price is at or below `level`."""
        for lo in range(after + 1, until + 1, 200):
            blocks = list(range(lo, min(lo + 200, until + 1)))
            self.fetch(blocks)
            for b in blocks:
                if self.price[b] <= level * (1 + ORACLE_TOLERANCE):
                    return b
        return None


class ShiftedAave:
    """Aave ETH-collateral accounts with every level moved by `shift` and an oracle-cross exit."""

    exact_daily = False

    def __init__(self, base, shift, oracle):
        self.base, self.shift, self.oracle = base, shift, oracle
        self.name = f"shift {shift:+.0%}"
        self.daily = base.daily[base.daily.cls == "ETH"].assign(p_est=lambda d: d.p_est * (1 + shift))
        self.level = {}

    def prefetch(self, keys):
        self.base.prefetch(keys)

    def live(self, pos, cls, block):
        state = self.base.live(pos, cls, block)
        if state is None:
            return None
        self.level[pos] = state[0] * (1 + self.shift)  # the engine simulates right after this call
        return [self.level[pos], state[1]]

    def liquidation(self, pos, after, until):
        return self.oracle.first_cross(self.level[pos], after, until)


def run():
    oracle = BlockOracle()
    ctx = bt.context(["aave"])
    parts = []
    for shift in SHIFTS:
        source = ShiftedAave(ctx["sources"]["aave"], shift, oracle)
        ctx["sources"][source.name] = source
        for bt.ENTRY_BUFFER, bt.STOP in CELLS:
            tr = bt.run([source.name]).assign(shift=shift, entry_buffer=bt.ENTRY_BUFFER, stop=bt.STOP)
            parts.append(tr)
            print(f"placebo: shift {shift:+.0%} entry {bt.ENTRY_BUFFER:.2%} stop {bt.STOP:.2%} -> {len(tr)} trades",
                  file=sys.stderr, flush=True)
            pd.concat(parts).to_csv("data/placebo_trades.csv", index=False)
            oracle.save()


def crowded(placebo, real):
    """Placebo trades whose level sits within 0.5% of a real traded level around the same time."""
    out = []
    for r in placebo.itertuples():
        near = real[(real.signal_block - r.signal_block).abs() <= 600]
        out.append(bool(((near.p_liq / r.p_liq - 1).abs() <= 0.005).any()))
    return np.array(out)


def report():
    first, last, _ = bt.load_prices()
    days = (last - first) * 12 / 86400
    pl = pd.read_csv("data/placebo_trades.csv")
    grid = pd.read_csv("data/grid_trades.csv")
    real_all = grid[grid.cls == "ETH"]
    rows = []
    for entry, stop in CELLS:
        real = grid[(grid.entry_buffer == entry) & (grid.stop == stop) & (grid.source == "aave") & (grid.cls == "ETH")]
        sets = [("real liquidations", real)]
        for shift in SHIFTS:
            d = pl[(pl.entry_buffer == entry) & (pl.stop == stop) & (pl["shift"] == shift)]
            label = "real level, oracle-cross exit" if shift == 0 else f"fake level {shift:+.0%}"
            sets.append((label, d))
            if shift:
                same = real_all[(real_all.entry_buffer == entry) & (real_all.stop == stop)]
                sets.append((label + ", no real level nearby", d[~crowded(d, same)]))
        for label, d in sets:
            if len(d) == 0:
                continue
            one = bt.one_at_a_time(d.assign(net_bps=d.gross_bps - bt.ROUND_TRIP_FEE_BPS))
            x = one.net_bps
            hit = one.reason == "liquidated"
            rows.append({"cell": f"{entry * 100:g}/{stop * 100:g}", "levels": label, "trades": len(one),
                         "hit %": 100 * hit.mean(), "bps if hit": x[hit].mean(), "bps if stopped": x[~hit].mean(),
                         "net bps": x.mean(), "t": x.mean() / (x.std(ddof=1) / np.sqrt(len(x))) if len(x) > 2 else np.nan,
                         "APR %": x.sum() / 100 * 365 / days})
    rows = pd.DataFrame(rows)
    for cell, d in rows.groupby("cell", sort=False):
        print(f"\n== entry/stop {cell} % (Aave, ETH collateral, one position at a time) ==")
        print(d.drop(columns="cell").round(1).to_string(index=False))
    print("\n== average over the three sets ==")
    print(rows.drop(columns="cell").groupby("levels", sort=False).mean().round(1).to_string())


if __name__ == "__main__":
    if "--report" not in sys.argv:
        run()
    report()
