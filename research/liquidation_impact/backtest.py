"""Backtest: short ETH just above a large Aave position's liquidation price.

Rule (all decisions use information available at that block):
  * Universe: Aave v3 accounts with >= MIN_DEBT_USD of debt whose collateral is ETH-like
    and whose debt is not, so their health factor moves one-for-one with the ETH price.
  * Liquidation price P_liq = ETH price at which the health factor reaches 1.
  * Entry:  DEX price closes within ENTRY_BUFFER above P_liq -> short, filled one block later.
  * Exit:   (a) the account is liquidated            -> cover one block after the liquidation
            (b) price closes STOP above the entry    -> cover one block later
            (c) MAX_HOLD blocks pass                 -> cover
Prices are end-of-block prices of the Uniswap v3 USDC/WETH 0.05% pool (data/prices.csv).
"""
import json
import os
import sys
from concurrent.futures import ThreadPoolExecutor

import numpy as np
import pandas as pd
from eth_abi import decode, encode

from snapshots import ORACLE, POOL, WETH, account_calldata, multicall

ENTRY_BUFFER = float(os.environ.get("ENTRY_BUFFER", 0.003))
STOP = float(os.environ.get("STOP", 0.005))
MAX_HOLD = int(os.environ.get("MAX_HOLD", 7200))
MIN_DEBT_USD = float(os.environ.get("MIN_DEBT_USD", 2_000_000))
REARM = 0.002  # price must first sit this far above the entry level before a (re-)entry
PREFILTER_SLACK = 0.03  # daily snapshots within this distance of the level get an exact on-chain check
INTRADAY_SLACK = 0.005  # re-check on-chain state when price is this close to the last known level
MIN_ETH_COLLATERAL_SHARE = 0.9
MAX_ETH_DEBT_SHARE = 0.05
ROUND_TRIP_FEE_BPS = 10.0  # two swaps in the 0.05% pool

ETH_LIKE = [
    WETH,
    "0x7f39c581f595b53c5cb19bd0b3f8da6c935e2ca0",  # wstETH
    "0xcd5fe23c85820f7b72d0926fc9b05b43e359b7ee",  # weETH
    "0xae78736cd615f374d3085123a210448e74fc6393",  # rETH
    "0xbe9895146f7af43049ca1c1ae358b0541ea49704",  # cbETH
    "0xf1c9acdc66974dfb6decb12aa385b9cd01190e38",  # osETH
    "0xa1290d69c65a6fe4df752f95823fae25cb99e5a7",  # rsETH
    "0xa35b1b31ce002fbf2058d22f30f95d405200a15b",  # ETHx
    "0xbf5495efe5db9ce00f80364c8b423567e58d2110",  # ezETH
]
CACHE_PATH = "data/live_cache.json"


def reserve_tokens(block):
    """(aToken, variableDebtToken) for each listed ETH-like reserve."""
    calls = [(POOL, bytes.fromhex("35ea6a75") + encode(["address"], [a])) for a in ETH_LIKE]  # getReserveData
    out = []
    for asset, (ok, ret) in zip(ETH_LIKE, multicall(calls, block, 0)):
        a_token = "0x" + ret[8 * 32 + 12 : 9 * 32].hex()
        debt_token = "0x" + ret[10 * 32 + 12 : 11 * 32].hex()
        if ok and int(a_token, 16):
            out.append((asset, a_token, debt_token))
    return out


class LiveState:
    """On-chain account state at an arbitrary historical block, cached on disk."""

    def __init__(self, tokens):
        self.tokens = tokens
        self.cache = json.load(open(CACHE_PATH)) if os.path.exists(CACHE_PATH) else {}
        self.calls = 0

    def save(self):
        json.dump(self.cache, open(CACHE_PATH, "w"))

    def get(self, user, block):
        """Returns (liquidation price of ETH, debt in USD) or None if not an ETH-vs-USD position."""
        key = f"{user}:{block}"
        if key not in self.cache:
            self.cache[key] = self._fetch(user, block)
            self.calls += 1
            if self.calls % 200 == 0:
                self.save()
        return self.cache[key]

    def _calls(self, user):
        bal = bytes.fromhex("70a08231") + encode(["address"], [user])
        calls = [(POOL, account_calldata(user))]
        for asset, a_token, debt_token in self.tokens:
            calls += [(ORACLE, bytes.fromhex("b3596f07") + encode(["address"], [asset])), (a_token, bal), (debt_token, bal)]
        return calls

    def _parse(self, out):
        if not out[0][0] or len(out[0][1]) < 192:
            return None
        coll, debt, _, _, _, hf = decode(["uint256"] * 6, out[0][1])
        if debt == 0 or coll == 0:
            return None
        word = lambda i: int.from_bytes(out[i][1], "big") if out[i][0] and out[i][1] else 0
        eth_coll = eth_debt = 0.0
        for n in range(len(self.tokens)):
            price = word(1 + 3 * n)
            eth_coll += word(2 + 3 * n) / 1e18 * price
            eth_debt += word(3 + 3 * n) / 1e18 * price
        share = min(1.0, eth_coll / coll)
        if share < MIN_ETH_COLLATERAL_SHARE or eth_debt / debt > MAX_ETH_DEBT_SHARE:
            return None
        eth_price = word(1) / 1e8  # Aave oracle price of WETH
        hf = hf / 1e18
        # health factor scales with the ETH price on the ETH share of collateral
        p_liq = eth_price * (1 - (1 - 1 / hf) / share)
        return [p_liq, debt / 1e8]

    def _fetch(self, user, block):
        return self._parse(multicall(self._calls(user), block, 0))

    def prefetch(self, pairs, chunk=60):
        """Fill the cache for many (user, block) pairs, batching users that share a block."""
        by_block = {}
        for user, block in pairs:
            if f"{user}:{block}" not in self.cache:
                by_block.setdefault(block, []).append(user)
        tasks = [(b, us[i : i + chunk], n) for n, (b, us) in enumerate(by_block.items()) for i in range(0, len(us), chunk)]
        per_user = 1 + 3 * len(self.tokens)

        def work(task):
            block, users, worker = task
            out = multicall([c for u in users for c in self._calls(u)], block, worker)
            return {f"{u}:{block}": self._parse(out[i * per_user : (i + 1) * per_user]) for i, u in enumerate(users)}

        with ThreadPoolExecutor(6) as pool:
            for n, res in enumerate(pool.map(work, tasks)):
                self.cache.update(res)
                if n % 100 == 0:
                    print("prefetch", n, len(tasks), file=sys.stderr, flush=True)
        self.save()


def run():
    px = pd.read_csv("data/prices.csv", usecols=["block", "close"]).set_index("block").close
    first, last = int(px.index.min()), int(px.index.max())
    close = px.reindex(range(first, last + 1)).ffill().to_numpy()
    at = lambda b: close[b - first]

    snaps = pd.read_csv("data/snapshots.csv")
    oracle = pd.read_csv("data/oracle_eth.csv").set_index("block").eth_usd
    snaps = snaps[(snaps.debt_usd >= MIN_DEBT_USD) & (snaps.block >= first)]
    snaps["p_est"] = snaps.block.map(oracle) / snaps.hf  # exact only for pure ETH-vs-USD accounts
    liq = pd.read_csv("data/liquidations.csv")
    liq_blocks = liq.groupby("user").block.apply(lambda s: np.sort(s.unique())).to_dict()

    interval = int(np.diff(np.sort(snaps.block.unique())).min())
    live = LiveState(reserve_tokens(last))

    def day_segment(block):
        b1 = min(block + interval, last - 2)
        return b1, close[block - first + 1 : b1 - first + 1]

    # exact state at the daily snapshot for every account-day that comes near its rough level
    near = [(s.user, int(s.block)) for s in snaps.itertuples()
            if len(day_segment(int(s.block))[1]) and day_segment(int(s.block))[1].min() <= s.p_est * (1 + ENTRY_BUFFER) * (1 + PREFILTER_SLACK)]
    print(f"{len(near)} account-days near a rough level", file=sys.stderr)
    live.prefetch(near)
    near = set(near)

    trades = []
    users = snaps.user.unique()
    for ui, (user, rows) in enumerate(snaps.sort_values("block").groupby("user", sort=False)):
        armed, open_until = False, -1
        for s in rows.itertuples():
            b0 = int(s.block)
            b1, seg = day_segment(b0)
            if (user, b0) not in near:
                armed = True  # price stayed clear of the level all day
                continue
            state = live.get(user, b0)
            if state is None or state[1] < MIN_DEBT_USD:
                continue  # not an ETH-collateral / USD-debt position
            p_est, slack = state[0], INTRADAY_SLACK
            if seg.min() > p_est * (1 + ENTRY_BUFFER) * (1 + slack):
                armed = True
                continue
            t = b0 + 1
            while t <= b1:
                level = p_est * (1 + ENTRY_BUFFER)
                window = close[t - first : b1 - first + 1]
                if not armed:
                    up = np.nonzero(window > level * (1 + slack) * (1 + REARM))[0]
                    if len(up) == 0:
                        break
                    armed, t = True, t + int(up[0]) + 1
                    continue
                hit = np.nonzero(window <= level * (1 + slack))[0]
                if len(hit) == 0:
                    break
                t += int(hit[0])
                state = live.get(user, t)
                if state is None or state[1] < MIN_DEBT_USD:
                    break  # not (or no longer) a qualifying position today
                p_est, slack = state[0], 0.0
                if at(t) > p_est * (1 + ENTRY_BUFFER):
                    t += 1
                    continue  # estimate was stale; keep scanning with the exact level
                armed = False
                if t <= open_until:
                    t = open_until + 1
                    continue
                trade = simulate(user, t, p_est, state[1], at, last, liq_blocks.get(user, []))
                if trade:
                    trades.append(trade)
                    open_until = trade["exit_block"]
                    t = open_until + 1
                else:
                    t += 1
        if ui % 50 == 0:
            print(ui, len(users), len(trades), live.calls, file=sys.stderr, flush=True)
    live.save()
    return pd.DataFrame(trades)


def simulate(user, t, p_liq, debt, at, last, user_liq_blocks):
    """Decision at close of block t; every fill happens one block after its signal."""
    if t + 2 > last:
        return None
    later = [b for b in user_liq_blocks if b > t]
    if later and later[0] <= t + 1:
        return None  # liquidated before we could get in
    entry = at(t + 1)
    end = min(t + 1 + MAX_HOLD, last - 1)
    liq_block = later[0] if later and later[0] <= end else None
    stop_block = None
    for b in range(t + 2, (liq_block or end) + 1):
        if at(b) >= entry * (1 + STOP):
            stop_block = b
            break
    if stop_block:
        reason, signal = "stop", stop_block
    elif liq_block:
        reason, signal = "liquidated", liq_block
    else:
        reason, signal = "timeout", end
    exit_price = at(signal + 1)
    return {
        "user": user, "debt_usd": debt, "p_liq": p_liq, "signal_block": t, "entry": entry,
        "exit_block": signal + 1, "reason": reason, "exit": exit_price,
        "gross_bps": 1e4 * np.log(entry / exit_price),
        # optimistic variant: cover in the liquidation block itself
        "gross_bps_same_block": 1e4 * np.log(entry / at(signal)) if reason == "liquidated" else 1e4 * np.log(entry / exit_price),
        "blocks_held": signal - t,
    }


def report(tr):
    tr["net_bps"] = tr.gross_bps - ROUND_TRIP_FEE_BPS
    print(f"entry {ENTRY_BUFFER:.2%} above liquidation price, stop {STOP:.2%}, max hold {MAX_HOLD} blocks, "
          f"debt >= ${MIN_DEBT_USD / 1e6:.0f}M")
    print(f"{len(tr)} trades on {tr.user.nunique()} accounts, {(tr.signal_block // 7200).nunique()} distinct days\n")
    rows = []
    for name, d in [("all", tr)] + [(r, tr[tr.reason == r]) for r in ["liquidated", "stop", "timeout"]]:
        if len(d):
            rows.append({"outcome": name, "n": len(d), "share%": 100 * len(d) / len(tr),
                         "gross_bps": d.gross_bps.mean(), "median": d.gross_bps.median(),
                         "net_bps": d.net_bps.mean(), "win%": 100 * (d.net_bps > 0).mean(),
                         "median_blocks": d.blocks_held.median()})
    print(pd.DataFrame(rows).round(1).to_string(index=False))
    x = tr.net_bps
    print(f"\nnet per trade: mean {x.mean():.1f} bps, t-stat {x.mean() / (x.std(ddof=1) / np.sqrt(len(x))):.2f}, "
          f"sum {x.sum():.0f} bps")
    print(f"if covered in the liquidation block itself: mean gross {tr.gross_bps_same_block.mean():.1f} bps")

    # one position at a time: skip signals that arrive while a trade is open
    taken, busy_until = [], -1
    for _, r in tr.sort_values("signal_block").iterrows():
        if r.signal_block > busy_until:
            taken.append(r)
            busy_until = r.exit_block
    one = pd.DataFrame(taken)
    print(f"\none position at a time: {len(one)} trades, net mean {one.net_bps.mean():.1f} bps, "
          f"sum {one.net_bps.sum():.0f} bps, win {100 * (one.net_bps > 0).mean():.0f}%, "
          f"liquidated {100 * (one.reason == 'liquidated').mean():.0f}% / stop {100 * (one.reason == 'stop').mean():.0f}%")

    print("\nby position size (debt):")
    rows = []
    for lo, hi in [(2e6, 5e6), (5e6, 2e7), (2e7, 1e12)]:
        d = tr[(tr.debt_usd >= lo) & (tr.debt_usd < hi)]
        if len(d):
            rows.append({"debt": f"${lo / 1e6:.0f}M-{hi / 1e6:.0f}M" if hi < 1e11 else f">${lo / 1e6:.0f}M", "n": len(d),
                         "liquidated%": 100 * (d.reason == "liquidated").mean(), "stop%": 100 * (d.reason == "stop").mean(),
                         "net_bps": d.net_bps.mean(), "win%": 100 * (d.net_bps > 0).mean()})
    print(pd.DataFrame(rows).round(1).to_string(index=False))


if __name__ == "__main__":
    out = "data/trades.csv"
    if "--report-only" in sys.argv:
        trades = pd.read_csv(out)
    else:
        trades = run()
        trades.to_csv(out, index=False)
    report(trades)
