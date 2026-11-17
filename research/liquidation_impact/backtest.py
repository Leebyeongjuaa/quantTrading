"""Backtest: short ETH/BTC just above a large lending position's liquidation price.

Rule (all decisions use information available at that block):
  * Universe: positions with >= MIN_DEBT_USD of USD-stablecoin debt against ETH-like or BTC-like
    collateral on Aave v3, Morpho Blue and Fluid (Ethereum mainnet), so their health factor
    moves one-for-one with the ETH (or BTC) price.
  * Liquidation price P_liq = price at which the health factor reaches 1.
  * Entry:  DEX price closes within ENTRY_BUFFER above P_liq -> short, filled one block later.
  * Exit:   (a) the position is liquidated           -> cover one block after the liquidation
            (b) price closes STOP above the entry    -> cover one block later
            (c) MAX_HOLD blocks pass                 -> cover
          optional:
            MAX_ENTRY_SLIP: skip the entry when the fill would be that far below the signal price
            (d) TAKE_PROFIT: price closes that far below the entry -> cover one block later
            (e) DEFEND_EXIT: the borrower adds margin / repays so that P_liq drops by more than
                that fraction (or the position stops qualifying)   -> cover one block later
Prices: data/prices.csv (ETH) and data/prices_btc.csv (BTC), last Uniswap v3 trade per block.

  python3 backtest.py [aave] [morpho] [fluid]     run the listed sources (default: all)
  python3 backtest.py --report-only               re-print the report from data/trades.csv
  python3 backtest.py --grid                      sweep entry buffer x stop (GRID_ENTRY, GRID_STOP)
  python3 backtest.py --grid-report               re-print the sweep from data/grid_trades.csv
  python3 backtest.py --exits                     compare exit rules (take-profit, margin-add exit)
  python3 backtest.py --exits-report              re-print that comparison from data/exit_trades.csv
"""
import json
import os
import sys
from concurrent.futures import ThreadPoolExecutor

import numpy as np
import pandas as pd
from eth_abi import decode, encode

import morpho
from snapshots import ORACLE, POOL, WETH, account_calldata, multicall

ENTRY_BUFFER = float(os.environ.get("ENTRY_BUFFER", 0.003))
STOP = float(os.environ.get("STOP", 0.005))
MAX_HOLD = int(os.environ.get("MAX_HOLD", 7200))
TAKE_PROFIT = float(os.environ.get("TAKE_PROFIT", 0))  # 0 = off
DEFEND_EXIT = float(os.environ.get("DEFEND_EXIT", 0))  # 0 = off
MAX_ENTRY_SLIP = float(os.environ.get("MAX_ENTRY_SLIP", 0))  # 0 = off; entry is a limit order
MIN_DEBT_USD = float(os.environ.get("MIN_DEBT_USD", 2_000_000))
SIGNAL_MEDIAN_BLOCKS = 3
REARM = 0.002  # price must first sit this far above the entry level before a (re-)entry
ROUGH_SLACK = 0.03  # Aave daily estimates assume pure collateral; check on-chain when this close
INTRADAY_SLACK = 0.005  # re-check on-chain state when price is this close to the last known level
MIN_COLLATERAL_SHARE = 0.9
MAX_SAME_ASSET_DEBT_SHARE = 0.05
ROUND_TRIP_FEE_BPS = 10.0  # two swaps in a 0.05% pool
DAY = 7200
CACHE_PATH = os.environ.get("CACHE_PATH", "data/live_cache.json")

AAVE_ASSETS = {
    "ETH": [
        WETH,
        "0x7f39c581f595b53c5cb19bd0b3f8da6c935e2ca0",  # wstETH
        "0xcd5fe23c85820f7b72d0926fc9b05b43e359b7ee",  # weETH
        "0xae78736cd615f374d3085123a210448e74fc6393",  # rETH
        "0xbe9895146f7af43049ca1c1ae358b0541ea49704",  # cbETH
        "0xf1c9acdc66974dfb6decb12aa385b9cd01190e38",  # osETH
        "0xa1290d69c65a6fe4df752f95823fae25cb99e5a7",  # rsETH
        "0xa35b1b31ce002fbf2058d22f30f95d405200a15b",  # ETHx
        "0xbf5495efe5db9ce00f80364c8b423567e58d2110",  # ezETH
    ],
    "BTC": [
        morpho.WBTC,
        "0xcbb7c0000ab88b473b1f5afd9ef808440eed33bf",  # cbBTC
        "0x18084fba666a33d37592fa2633fd49a74dd93a88",  # tBTC
        "0x8236a87084f8b84306f72007f36f2618a5634494",  # LBTC
        "0x657e8c867d8b37dcc18fa4caead9c45eb088c642",  # eBTC
        "0xc96de26018a54d51c097160568752c4e3bd6c364",  # FBTC
    ],
}


class Cache:
    """Disk cache for on-chain lookups at historical blocks."""

    def __init__(self):
        self.data = json.load(open(CACHE_PATH)) if os.path.exists(CACHE_PATH) else {}
        self.misses = 0

    def get(self, key, fetch):
        if key not in self.data:
            self.data[key] = fetch()
            self.misses += 1
            if self.misses % 200 == 0:
                self.save()
        return self.data[key]

    def save(self):
        json.dump(self.data, open(CACHE_PATH, "w"))


class Aave:
    """pos = account address. An account qualifies for a class (ETH or BTC) when >= 90% of its
    collateral and <= 5% of its debt is in that class."""

    name = "aave"
    exact_daily = False

    def __init__(self, cache, last_block):
        self.cache = cache
        oracle = pd.read_csv("data/oracle_base.csv").set_index("block")
        snaps = pd.read_csv("data/snapshots.csv")
        snaps = snaps[snaps.debt_usd >= MIN_DEBT_USD].rename(columns={"user": "pos"})
        self.daily = pd.concat(
            [snaps.assign(cls=c, p_est=snaps.block.map(oracle[c]) / snaps.hf) for c in ("ETH", "BTC")]
        )[["block", "pos", "cls", "debt_usd", "p_est"]]
        liq = pd.read_csv("data/liquidations.csv")
        self.liq_blocks = liq.groupby("user").block.apply(lambda s: np.sort(s.unique())).to_dict()
        self.tokens = self._reserve_tokens(last_block)
        self.per_user = 1 + 3 * len(self.tokens)

    def _reserve_tokens(self, block):
        assets = [(c, a) for c, lst in AAVE_ASSETS.items() for a in lst]
        calls = [(POOL, bytes.fromhex("35ea6a75") + encode(["address"], [a])) for _, a in assets]  # getReserveData
        out = []
        for (cls, asset), (ok, ret) in zip(assets, multicall(calls, block, 0)):
            a_token, debt_token = "0x" + ret[8 * 32 + 12 : 9 * 32].hex(), "0x" + ret[10 * 32 + 12 : 11 * 32].hex()
            if ok and int(a_token, 16):
                out.append((cls, asset, a_token, debt_token))
        return out

    def _calls(self, user):
        bal = bytes.fromhex("70a08231") + encode(["address"], [user])
        calls = [(POOL, account_calldata(user))]
        for _, asset, a_token, debt_token in self.tokens:
            calls += [(ORACLE, bytes.fromhex("b3596f07") + encode(["address"], [asset])), (a_token, bal), (debt_token, bal)]
        return calls

    def _parse(self, out):
        """-> {cls: [p_liq, debt_usd] or None}"""
        res = {"ETH": None, "BTC": None}
        if not out[0][0] or len(out[0][1]) < 192:
            return res
        coll, debt, _, _, _, hf = decode(["uint256"] * 6, out[0][1])
        if debt == 0 or coll == 0:
            return res
        word = lambda i: int.from_bytes(out[i][1], "big") if out[i][0] and out[i][1] else 0
        held, owed, base = {"ETH": 0.0, "BTC": 0.0}, {"ETH": 0.0, "BTC": 0.0}, {}
        for n, (cls, asset, _, _) in enumerate(self.tokens):
            price = word(1 + 3 * n)
            if asset in (WETH, morpho.WBTC):
                base[cls] = price / 1e8
            decimals = 8 if cls == "BTC" and asset != "0x18084fba666a33d37592fa2633fd49a74dd93a88" else 18
            held[cls] += word(2 + 3 * n) / 10**decimals * price
            owed[cls] += word(3 + 3 * n) / 10**decimals * price
        for cls in res:
            share = min(1.0, held[cls] / coll)
            if share >= MIN_COLLATERAL_SHARE and owed[cls] / debt <= MAX_SAME_ASSET_DEBT_SHARE:
                # health factor scales with the asset price on that share of the collateral
                res[cls] = [base[cls] * (1 - (1 - 1e18 / hf) / share), debt / 1e8]
        return res

    def prefetch(self, keys):
        """keys: (pos, cls, block). Batches accounts that share a block."""
        by_block = {}
        for user, block in {(u, b) for u, c, b in keys if f"aave:{u}:{c}:{b}" not in self.cache.data}:
            by_block.setdefault(block, []).append(user)
        tasks = [(b, us[i : i + 40], n) for n, (b, us) in enumerate(by_block.items()) for i in range(0, len(us), 40)]

        def work(task):
            block, users, worker = task
            out = multicall([c for u in users for c in self._calls(u)], block, worker)
            res = {}
            for i, u in enumerate(users):
                for cls, v in self._parse(out[i * self.per_user : (i + 1) * self.per_user]).items():
                    res[f"aave:{u}:{cls}:{block}"] = v
            return res

        with ThreadPoolExecutor(6) as pool:
            for n, res in enumerate(pool.map(work, tasks)):
                self.cache.data.update(res)
                if n % 100 == 0:
                    print("aave prefetch", n, len(tasks), file=sys.stderr, flush=True)
        self.cache.save()

    def live(self, pos, cls, block):
        def fetch():
            res = self._parse(multicall(self._calls(pos), block, 0))
            for c, v in res.items():
                self.cache.data[f"aave:{pos}:{c}:{block}"] = v
            return res[cls]

        return self.cache.get(f"aave:{pos}:{cls}:{block}", fetch)

    def liquidation(self, pos, after, until):
        blocks = self.liq_blocks.get(pos, [])
        i = np.searchsorted(blocks, after, side="right")
        return int(blocks[i]) if i < len(blocks) and blocks[i] <= until else None


class Morpho:
    """pos = "<marketId>:<borrower>". Daily snapshots are already exact."""

    name = "morpho"
    exact_daily = True

    def __init__(self, cache, last_block):
        self.cache = cache
        self.markets = morpho.load_markets()
        snaps = pd.read_csv("data/morpho_snapshots.csv")
        self.daily = snaps[snaps.debt_usd >= MIN_DEBT_USD]
        liq = pd.read_csv("data/morpho_liquidations.csv")
        self.liq_blocks = liq.groupby("pos").block.apply(lambda s: np.sort(s.unique())).to_dict()

    def prefetch(self, keys):
        pass

    def live(self, pos, cls, block):
        return self.cache.get(f"morpho:{pos}:{block}", lambda: morpho.live(self.markets, pos, block))

    liquidation = Aave.liquidation


SOURCES = {"aave": Aave, "morpho": Morpho}


def load_prices():
    eth = pd.read_csv("data/prices.csv", usecols=["block", "close"]).set_index("block").close
    btc = pd.read_csv("data/prices_btc.csv").set_index("block").close
    first, last = int(eth.index.min()), int(eth.index.max())
    out = {}
    for cls, s in (("ETH", eth), ("BTC", btc)):
        dense = s.reindex(range(first, last + 1)).ffill().bfill()
        # Signals use a 3-block median: a single block can close several percent away from the
        # market when someone dumps into this one pool, and reverts in the next block.
        out[cls] = (dense.rolling(SIGNAL_MEDIAN_BLOCKS, min_periods=1).median().to_numpy(), dense.to_numpy())
    return first, last, out


_ctx = {}


def context(source_names):
    """Prices, cache and sources, loaded once per process."""
    if not _ctx:
        _ctx.update(prices=load_prices(), cache=Cache(), sources={}, day_min={})
    for n in source_names:
        if n not in _ctx["sources"]:
            _ctx["sources"][n] = SOURCES[n](_ctx["cache"], _ctx["prices"][1])
    return _ctx


def run(source_names):
    ctx = context(source_names)
    first, last, prices = ctx["prices"]
    cache = ctx["cache"]
    trades = []
    for source in [ctx["sources"][n] for n in source_names]:
        daily = source.daily[source.daily.block >= first]
        slack0 = INTRADAY_SLACK if source.exact_daily else ROUGH_SLACK

        def day_end(block):
            return min(block + DAY, last - 2)

        def day_min(cls, block):
            if (cls, block) not in ctx["day_min"]:
                seg = prices[cls][0][block - first + 1 : day_end(block) - first + 1]
                ctx["day_min"][(cls, block)] = seg.min() if len(seg) else np.inf
            return ctx["day_min"][(cls, block)]

        near = {(r.pos, r.cls, int(r.block)) for r in daily.itertuples()
                if day_min(r.cls, int(r.block)) <= r.p_est * (1 + ENTRY_BUFFER) * (1 + slack0)}
        print(f"{source.name}: {len(near)} position-days near a level", file=sys.stderr)
        source.prefetch(near)

        groups = daily.sort_values("block").groupby(["pos", "cls"], sort=False)
        for gi, ((pos, cls), rows) in enumerate(groups):
            signal, fill = prices[cls]
            at = lambda b: signal[b - first]
            fill_at = lambda b: fill[b - first]
            armed, open_until = False, -1
            for s in rows.itertuples():
                b0 = int(s.block)
                b1 = day_end(b0)
                if (pos, cls, b0) not in near:
                    armed = True  # price stayed clear of the level all day
                    continue
                state = [s.p_est, s.debt_usd] if source.exact_daily else source.live(pos, cls, b0)
                if state is None or state[1] < MIN_DEBT_USD:
                    continue  # not a single-asset-collateral / USD-debt position
                p_est, slack = state[0], INTRADAY_SLACK
                if day_min(cls, b0) > p_est * (1 + ENTRY_BUFFER) * (1 + slack):
                    armed = True
                    continue
                t = b0 + 1
                while t <= b1:
                    level = p_est * (1 + ENTRY_BUFFER)
                    window = signal[t - first : b1 - first + 1]
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
                    state = source.live(pos, cls, t)
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
                    trade = simulate(source, pos, cls, t, p_est, state[1], at, fill_at, last)
                    if trade:
                        trades.append(trade)
                        open_until = trade["exit_block"]
                        t = open_until + 1
                    else:
                        t += 1
            if gi % 100 == 0:
                print(source.name, gi, len(groups), len(trades), cache.misses, file=sys.stderr, flush=True)
        cache.save()
    return pd.DataFrame(trades)


def simulate(source, pos, cls, t, p_liq, debt, at, fill_at, last):
    """Signal at close of block t (median-filtered price); fills one block later at the pool price."""
    if t + 2 > last:
        return None
    entry = fill_at(t + 1)
    if MAX_ENTRY_SLIP and entry < at(t) * (1 - MAX_ENTRY_SLIP):
        # The pool closed far below the price we acted on (someone dumped into it in that block).
        # A limit order would not have filled there, so there is no trade.
        return None
    end = min(t + 1 + MAX_HOLD, last - 1)
    reason, sig = "timeout", end
    for b in range(t + 2, end + 1):
        price = at(b)
        if price >= entry * (1 + STOP):
            reason, sig = "stop", b
            break
        if TAKE_PROFIT and price <= entry * (1 - TAKE_PROFIT):
            reason, sig = "take_profit", b
            break
    liq_block = source.liquidation(pos, t, sig)
    if liq_block is not None and liq_block <= t + 1:
        return None  # liquidated before we could get in
    if liq_block is not None:
        reason, sig = "liquidated", liq_block
    if DEFEND_EXIT:
        # Did the borrower pull the liquidation price away before our exit? Look at the state just
        # before the exit, then bisect for the first block where it had moved.
        def defended(block):
            state = source.live(pos, cls, block)
            return state is None or state[0] < p_liq * (1 - DEFEND_EXIT)

        check = sig - 1 if reason == "liquidated" else sig
        if check > t + 1 and defended(check):
            lo, hi = t + 1, check
            while lo < hi:
                mid = (lo + hi) // 2
                lo, hi = (lo, mid) if defended(mid) else (mid + 1, hi)
            reason, sig = "defended", lo
    exit_price = fill_at(sig + 1)
    return {
        "source": source.name, "cls": cls, "pos": pos, "debt_usd": debt, "p_liq": p_liq, "signal_block": t,
        "entry": entry, "exit_block": sig + 1, "reason": reason, "exit": exit_price,
        "gross_bps": 1e4 * np.log(entry / exit_price), "blocks_held": sig - t,
    }


def one_at_a_time(tr):
    """Skip signals that arrive while a trade is open."""
    taken, busy_until = [], -1
    for r in tr.sort_values("signal_block").itertuples():
        if r.signal_block > busy_until:
            taken.append(r)
            busy_until = r.exit_block
    return pd.DataFrame(taken)


def summary(name, d, days):
    one = one_at_a_time(d)
    best_day = one.groupby(one.signal_block // DAY).net_bps.sum().max()
    x = d.net_bps
    return {
        "group": name, "trades": len(d), "days": (d.signal_block // DAY).nunique(),
        "liquidated%": 100 * (d.reason == "liquidated").mean(), "stop%": 100 * (d.reason == "stop").mean(),
        "net_bps": x.mean(), "t": x.mean() / (x.std(ddof=1) / np.sqrt(len(x))) if len(x) > 1 else np.nan,
        "1pos_trades": len(one), "1pos_APR%": one.net_bps.sum() / 100 * 365 / days,
        "APR_ex_best_day%": (one.net_bps.sum() - best_day) / 100 * 365 / days,
    }


def report(tr):
    first, last, _ = load_prices()
    days = (last - first) * 12 / 86400
    tr["net_bps"] = tr.gross_bps - ROUND_TRIP_FEE_BPS
    print(f"entry {ENTRY_BUFFER:.2%} above liquidation price, stop {STOP:.2%}, max hold {MAX_HOLD} blocks, "
          f"debt >= ${MIN_DEBT_USD / 1e6:.0f}M, {days:.0f} days")
    rows = [summary("ALL", tr, days)]
    for cls in sorted(tr.cls.unique()):
        rows.append(summary(cls, tr[tr.cls == cls], days))
    for (src, cls), d in tr.groupby(["source", "cls"]):
        rows.append(summary(f"{src} {cls}", d, days))
    print(pd.DataFrame(rows).round(1).to_string(index=False))
    print("\nby outcome:")
    print(tr.groupby("reason").agg(n=("net_bps", "size"), net_bps=("net_bps", "mean"), median=("net_bps", "median"),
                                    median_blocks=("blocks_held", "median")).round(1).to_string())
    print("\nby position size (debt):")
    size = pd.cut(tr.debt_usd, [0, 5e6, 2e7, 1e12], labels=["$2-5M", "$5-20M", ">$20M"])
    print(tr.groupby(size, observed=True).agg(n=("net_bps", "size"), liquidated=("reason", lambda s: 100 * (s == "liquidated").mean()),
                                               net_bps=("net_bps", "mean")).round(1).to_string())


# Entry buffer may be zero or negative: enter once the DEX price is at / below the liquidation price
# while the oracle has not caught up yet (e.g. GRID_ENTRY=0,-0.001,-0.002).
GRID_ENTRY = [float(x) for x in os.environ.get("GRID_ENTRY", "0.0005,0.001,0.002,0.003,0.005,0.01").split(",")]
GRID_STOP = [float(x) for x in os.environ.get("GRID_STOP", "0.003,0.005,0.0075,0.01,0.02").split(",")]
GRID_OUT = os.environ.get("GRID_OUT", "data/grid_trades.csv")


def grid(source_names):
    """Run every (entry buffer, stop) combination and save all trades to data/grid_trades.csv."""
    global ENTRY_BUFFER, STOP
    parts = []
    for entry in GRID_ENTRY:
        for stop in GRID_STOP:
            ENTRY_BUFFER, STOP = entry, stop
            parts.append(run(source_names).assign(entry_buffer=entry, stop=stop))
            print(f"grid: entry {entry:.2%} stop {stop:.2%} -> {len(parts[-1])} trades", file=sys.stderr, flush=True)
            pd.concat(parts).to_csv(GRID_OUT, index=False)


def grid_report(tr, label):
    """One table per metric, rows = entry buffer, columns = stop. APR is one position at a time."""
    first, last, _ = load_prices()
    days = (last - first) * 12 / 86400
    middle = (first + last) // 2
    tr = tr.assign(net_bps=tr.gross_bps - ROUND_TRIP_FEE_BPS)
    cells = []
    for (entry, stop), d in tr.groupby(["entry_buffer", "stop"]):
        one = one_at_a_time(d)
        by_day = one.groupby(one.signal_block // DAY).net_bps.sum()
        x = one.net_bps
        cells.append({
            "entry%": entry * 100, "stop%": stop * 100,
            "trades": len(one),
            "liquidated %": 100 * (one.reason == "liquidated").mean(),
            "net bps / trade": x.mean(),
            "t-stat": x.mean() / (x.std(ddof=1) / np.sqrt(len(x))),
            "APR %": x.sum() / 100 * 365 / days,
            "APR % without best day": (x.sum() - by_day.max()) / 100 * 365 / days,
            "APR % first half": one[one.signal_block < middle].net_bps.sum() / 100 * 365 / (days / 2),
            "APR % second half": one[one.signal_block >= middle].net_bps.sum() / 100 * 365 / (days / 2),
            "winning days %": 100 * (by_day > 0).mean(),
        })
    cells = pd.DataFrame(cells)
    print(f"===== {label}: {tr.groupby(['entry_buffer', 'stop']).ngroups} parameter sets =====")
    for metric in cells.columns[2:]:
        print(f"\n{metric}  (rows: entry buffer %, columns: stop %)")
        print(cells.pivot(index="entry%", columns="stop%", values=metric).round(1).to_string())


EXIT_VARIANTS = [  # (label, take profit, margin-add exit threshold)
    ("base", 0, 0),
    ("defend", 0, 0.003),
    ("tp 0.5%", 0.005, 0),
    ("tp 1%", 0.01, 0),
    ("tp 2%", 0.02, 0),
    ("tp 1% + defend", 0.01, 0.003),
    ("tp 2% + defend", 0.02, 0.003),
]
EXIT_CELLS = [(e, s) for e in (0.001, 0.002, 0.003) for s in (0.005, 0.0075, 0.01)]


def exits(source_names):
    """Run each exit variant on the nine entry/stop sets of the robust region."""
    global ENTRY_BUFFER, STOP, TAKE_PROFIT, DEFEND_EXIT
    parts = []
    for label, TAKE_PROFIT, DEFEND_EXIT in EXIT_VARIANTS:
        for ENTRY_BUFFER, STOP in EXIT_CELLS:
            parts.append(run(source_names).assign(variant=label, entry_buffer=ENTRY_BUFFER, stop=STOP))
            print(f"exits: {label} entry {ENTRY_BUFFER:.2%} stop {STOP:.2%} -> {len(parts[-1])} trades",
                  file=sys.stderr, flush=True)
            pd.concat(parts).to_csv("data/exit_trades.csv", index=False)


def exits_report(tr, label):
    first, last, _ = load_prices()
    days = (last - first) * 12 / 86400
    tr = tr.assign(net_bps=tr.gross_bps - ROUND_TRIP_FEE_BPS)
    rows = []
    for (variant, entry, stop), d in tr.groupby(["variant", "entry_buffer", "stop"], sort=False):
        one = one_at_a_time(d)
        by_day = one.groupby(one.signal_block // DAY).net_bps.sum()
        eq = one.net_bps.cumsum() / 100
        counts = one.reason.value_counts()
        rows.append({"variant": variant, "cell": f"{entry * 100:g}/{stop * 100:g}", "trades": len(one),
                     "APR": one.net_bps.sum() / 100 * 365 / days,
                     "APR ex best day": (one.net_bps.sum() - by_day.max()) / 100 * 365 / days,
                     "max drawdown": (eq.cummax().clip(lower=0) - eq).max(),
                     "net bps": one.net_bps.mean(),
                     **{f"n {r}": counts.get(r, 0) for r in ("liquidated", "stop", "take_profit", "defended")},
                     **{f"bps {r}": one[one.reason == r].net_bps.mean() for r in ("take_profit", "defended")}})
    rows = pd.DataFrame(rows)
    order = [v[0] for v in EXIT_VARIANTS]
    print(f"===== {label}: one position at a time, {days:.0f} days =====")
    print("\nAPR % by entry/stop set (columns) and exit rule (rows)")
    print(rows.pivot(index="variant", columns="cell", values="APR").reindex(order).round(1).to_string())
    print("\naverage over the nine sets")
    print(rows.drop(columns="cell").groupby("variant").mean().reindex(order).round(1).to_string())


if __name__ == "__main__":
    out = os.environ.get("OUT", "data/trades.csv")
    names = [a for a in sys.argv[1:] if not a.startswith("--")] or ["aave", "morpho", "fluid"]
    if "--exits" in sys.argv or "--exits-report" in sys.argv:
        if "--exits" in sys.argv:
            if "fluid" in names:
                from fluid import Fluid

                SOURCES["fluid"] = Fluid
            exits(names)
        x = pd.read_csv("data/exit_trades.csv")
        exits_report(x, "all protocols, ETH + BTC")
        exits_report(x[(x.source == "aave") & (x.cls == "ETH")], "Aave, ETH collateral only")
        sys.exit()
    if "--grid" in sys.argv or "--grid-report" in sys.argv:
        if "--grid" in sys.argv:
            if "fluid" in names:
                from fluid import Fluid

                SOURCES["fluid"] = Fluid
            grid(names)
        g = pd.read_csv(GRID_OUT)
        grid_report(g, "all protocols, ETH + BTC")
        grid_report(g[(g.source == "aave") & (g.cls == "ETH")], "Aave, ETH collateral only")
        grid_report(g[g.debt_usd >= 5e6], "positions with >= $5M debt")
        sys.exit()
    if "--report-only" in sys.argv:
        trades = pd.read_csv(out)
    else:
        if "fluid" in names:
            from fluid import Fluid

            SOURCES["fluid"] = Fluid
        trades = run(names)
        trades.to_csv(out, index=False)
    report(trades)
