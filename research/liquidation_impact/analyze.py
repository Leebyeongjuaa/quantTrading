"""Event study: ETH DEX price around Aave v3 liquidations of ETH collateral.

All returns are log returns in basis points. Negative = ETH got cheaper.
  pre_K   end of block B-1-K -> end of block B-1          (run-up into the liquidation)
  leg1    end of block B-1   -> just before the first liquidation tx in block B
  leg2    just before the first liquidation tx -> just after the last one (the sale itself)
  post_K  just after the last liquidation tx -> end of block B+K
"""
import sys

import numpy as np
import pandas as pd

from fetch_swaps import eth_long_liquidations

HORIZONS = [0, 1, 2, 5, 10, 25, 50, 100]
POOL = sys.argv[1] if len(sys.argv) > 1 else "v4"
ROUND_TRIP_FEE_BPS = {"v4": 2.5, "v3": 10.0}[POOL]

swaps = pd.read_csv("data/swaps.csv")
swaps = swaps[swaps.pool == POOL].sort_values(["block", "log_index"]).reset_index(drop=True)
blk = swaps.block.to_numpy()
txi = swaps.tx_index.to_numpy()
px = swaps.price.to_numpy()
order_key = blk * 100_000 + txi  # sortable (block, tx_index)


def price_at_end(block):
    i = np.searchsorted(blk, block, side="right") - 1
    return px[i] if i >= 0 else np.nan


def price_before_tx(block, tx_index):
    i = np.searchsorted(order_key, block * 100_000 + tx_index, side="left") - 1
    return px[i] if i >= 0 else np.nan


def price_after_tx(block, tx_index):
    i = np.searchsorted(order_key, block * 100_000 + tx_index, side="right") - 1
    return px[i] if i >= 0 else np.nan


def bps(a, b):
    return 1e4 * np.log(b / a)


liq = eth_long_liquidations()
ref_txs = set(swaps.tx)
rows = []
for b, g in liq.groupby("block"):
    first_tx, last_tx = g.tx_index.min(), g.tx_index.max()
    p_prev = price_at_end(b - 1)
    p_pre = price_before_tx(b, first_tx)
    p_post = price_after_tx(b, last_tx)
    row = {
        "block": b,
        "usd": g.usd.sum(),
        "n_liq": len(g),
        "touched_ref_pool": bool(set(g.tx) & ref_txs),
        "leg1": bps(p_prev, p_pre),
        "leg2": bps(p_pre, p_post),
    }
    for k in (5, 25, 100):
        row[f"pre_{k}"] = bps(price_at_end(b - 1 - k), p_prev)
    for k in HORIZONS:
        row[f"post_{k}"] = bps(p_post, price_at_end(b + k))
    rows.append(row)
ev = pd.DataFrame(rows).sort_values("block").reset_index(drop=True)
ev["head"] = ev.block.diff().fillna(1e9) > 100  # first liquidation block after 100 quiet blocks
ev.to_csv(f"data/events_{POOL}.csv", index=False)


def summarize(df, cols):
    out = []
    for c in cols:
        x = df[c].dropna()
        if len(x) < 2:
            continue
        out.append(
            {
                "metric": c,
                "n": len(x),
                "mean": x.mean(),
                "median": x.median(),
                "t": x.mean() / (x.std(ddof=1) / np.sqrt(len(x))),
                "pct_down": 100 * (x < 0).mean(),
            }
        )
    return pd.DataFrame(out).round(2).to_string(index=False)


path = ["pre_100", "pre_25", "pre_5", "leg1", "leg2"] + [f"post_{k}" for k in HORIZONS]
print(f"=== pool {POOL}: {len(ev)} liquidation blocks, ${ev.usd.sum() / 1e6:.0f}M repaid, "
      f"{ev['head'].sum()} cluster heads, {100 * ev.touched_ref_pool.mean():.0f}% touched this pool ===")
print("\n-- price path, all liquidation blocks --")
print(summarize(ev, path))
print("\n-- price path, cluster heads only --")
print(summarize(ev[ev["head"]], path))

buckets = [(0, 1e4, "<$10k"), (1e4, 1e5, "$10k-100k"), (1e5, 1e6, "$100k-1M"), (1e6, 1e12, ">$1M")]
print("\n-- by size of liquidations in the block (mean bps) --")
tbl = []
for lo, hi, name in buckets:
    d = ev[(ev.usd >= lo) & (ev.usd < hi)]
    tbl.append({"size": name, "n": len(d), **{c: d[c].mean() for c in ["pre_25", "leg1", "leg2", "post_0", "post_1", "post_5", "post_25"]}})
print(pd.DataFrame(tbl).round(2).to_string(index=False))

# --- strategies, gross and net of the pool's round-trip swap fee -------------
ev["short_prev_to_end"] = -(ev.leg1 + ev.leg2 + ev.post_0)  # short at end of B-1, cover end of B
ev["short_prev_to_5"] = -(ev.leg1 + ev.leg2 + ev.post_5)
ev["short_pre_to_end"] = -(ev.leg2 + ev.post_0)  # short just ahead of the liquidation tx
ev["short_pre_to_1"] = -(ev.leg2 + ev.post_1)
ev["short_pre_to_5"] = -(ev.leg2 + ev.post_5)
ev["long_end_to_5"] = ev.post_5 - ev.post_0  # buy the dip at end of B
ev["long_end_to_25"] = ev.post_25 - ev.post_0
strat = [c for c in ev.columns if c.startswith(("short_", "long_"))]
print(f"\n-- strategy gross P&L per trade (bps); round-trip fee on this pool = {ROUND_TRIP_FEE_BPS} bps --")
print(summarize(ev, strat))
big = ev[ev.usd >= 1e6]
print("\n-- same, blocks with >= $1M liquidated --")
print(summarize(big, strat))

# --- placebo: big down-blocks with no liquidation nearby ---------------------
last_px = swaps.groupby("block").price.last()
full = last_px.reindex(range(blk.min(), blk.max() + 1)).ffill()
ret1 = 1e4 * np.log(full / full.shift(1))
liq_blocks = ev.block.to_numpy()
cand = ret1.index.to_numpy()
pos = np.searchsorted(liq_blocks, cand)
dist = np.minimum(
    np.abs(cand - liq_blocks[np.clip(pos, 0, len(liq_blocks) - 1)]),
    np.abs(cand - liq_blocks[np.clip(pos - 1, 0, len(liq_blocks) - 1)]),
)
block_move = ev.leg1 + ev.leg2 + ev.post_0
print("\n-- drift after a down-block: liquidation blocks vs. quiet blocks (>=30 blocks from any liquidation) --")
tbl = []
for lo, hi in [(-10, -3), (-25, -10), (-1000, -25)]:
    e = ev[(block_move > lo) & (block_move <= hi)] if lo > -1000 else ev[block_move <= hi]
    c = cand[(dist >= 30) & (ret1.to_numpy() > lo) & (ret1.to_numpy() <= hi)]
    c = c[c + 25 <= full.index.max()]
    fwd = lambda k: 1e4 * np.log(full.reindex(c + k).to_numpy() / full.reindex(c).to_numpy())
    tbl.append({"block move (bps)": f"({lo},{hi}]", "n_liq": len(e),
                "liq +5": (e.post_5 - e.post_0).mean(), "liq +25": (e.post_25 - e.post_0).mean(),
                "n_quiet": len(c), "quiet +5": np.nanmean(fwd(5)) if len(c) else np.nan,
                "quiet +25": np.nanmean(fwd(25)) if len(c) else np.nan})
print(pd.DataFrame(tbl).round(2).to_string(index=False))

print("\n-- 12 largest liquidation blocks --")
print(ev.nlargest(12, "usd")[["block", "usd", "n_liq", "pre_25", "leg1", "leg2", "post_0", "post_1", "post_5", "post_25"]].round(1).to_string(index=False))
