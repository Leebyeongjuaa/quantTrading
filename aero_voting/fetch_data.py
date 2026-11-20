"""Collect Aero (Base) allocation-reward data since the 2026-10-22 launch.

Outputs (data/):
  gauges.csv        one row per gauge: rewards contract, pool, tokens, type, current weight
  fee_events.csv    every NotifyFeesAmount release (fees credited to allocators), with the
                    allocation supply of that gauge at release time
  incentives.csv    every incentive program (streaming "bribes") per gauge
  supply_hourly.csv allocation supply (voting power) per gauge per hour
  prices.csv        hourly USD prices of every fee / incentive token (DefiLlama)

Run:  python -m aero_voting.fetch_data [--stage all|meta|fees|incentives|supply|prices]
"""
import argparse
import logging
import os
import sys
import time
from concurrent.futures import ThreadPoolExecutor

import numpy as np
import pandas as pd
import requests
from web3 import Web3

from .config import (AERO_TOKEN, ERC20_ABI, FACTORY_REGISTRY, LAUNCH_TS, LEAF_VOTER, LEAF_VOTER_ABI,
                     POOL_ABI, REGISTRY_ABI, REWARDS_ABI)
from .rpc import Rpc, encode_call

log = logging.getLogger("aero.fetch")
DATA = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "data")
LOG_SPAN = 50_000  # publicnode: max 50k blocks and 10 addresses per eth_getLogs
NOTIFY_FEES_TOPIC = "0x87e2847214b11ebf92b00367c6c1a37b67c5bab8ff351feec88d712e1775de3a"
# tokens DefiLlama cannot price by Base address -> proxy coin (new AERO converts 1:1 from old AERO)
PRICE_PROXY = {
    "0x3f5bf3edac59d650d8fdf9657a71f2f904a3a4ee": "base:0x940181a94A35A4569E4529A3CDfB74e38FD98631",  # new AERO -> old AERO
    "0xc1cba3fcea344f92d9239c08c0568f6f2f0ee452": "coingecko:wrapped-steth",   # wstETH
    "0x311935cd80b76769bf2ecc9d8ab7635b2139cf82": "coingecko:solana",          # SOL (bridged)
}


def cs(a: str) -> str:
    return Web3.to_checksum_address(a)


class Fetcher:
    def __init__(self):
        self.rpc = Rpc()
        self.w3 = self.rpc.w3
        self.head = self.rpc.block_number()
        self.head_ts = self.rpc.block_ts(self.head)
        self.launch_block = self.rpc.find_block_at(LAUNCH_TS)
        self.launch_block_ts = self.rpc.block_ts(self.launch_block)
        log.info("head=%d (%d) launch_block=%d (%d)", self.head, self.head_ts, self.launch_block, self.launch_block_ts)

    def block_to_ts(self, b):
        """Block -> timestamp by interpolating real block timestamps sampled every 2,000 blocks.
        Base's block time is not constant (2s at launch, faster since early November 2026), so a fixed
        block time misdates events."""
        if not hasattr(self, "_ts_grid"):
            grid = list(range(self.launch_block, self.head, 2000)) + [self.head]
            with ThreadPoolExecutor(max_workers=4) as ex:
                ts = list(ex.map(self.rpc.block_ts, grid))
            self._ts_grid = (np.array(grid, dtype=float), np.array(ts, dtype=float))
            log.info("sampled %d block timestamps for block->time interpolation", len(grid))
        xs, ys = self._ts_grid
        return np.rint(np.interp(np.asarray(b, dtype=float), xs, ys)).astype(np.int64)

    # ---------------------------------------------------------------- meta
    def fetch_meta(self) -> pd.DataFrame:
        reg = self.w3.eth.contract(address=cs(FACTORY_REGISTRY), abi=REGISTRY_ABI)
        factories = self.rpc.call(lambda w3: reg.functions.gaugeFactories().call())
        gauges, fac_of = [], {}
        for f in factories:
            for g in self.rpc.call(lambda w3: reg.functions.factoryToGauges(f).call()):
                if g not in fac_of:
                    gauges.append(g)
                    fac_of[g] = f
        n = len(gauges)
        log.info("%d gauges from %d factories", n, len(factories))
        enc = lambda abi, fn, args=(): encode_call(self.w3, abi, fn, args)
        calls, types = [], []
        for g in gauges:
            calls += [(FACTORY_REGISTRY, enc(REGISTRY_ABI, "gaugeToRewards", [g])),
                      (FACTORY_REGISTRY, enc(REGISTRY_ABI, "gaugeToTarget", [g])),
                      (FACTORY_REGISTRY, enc(REGISTRY_ABI, "emissionCap", [g])),
                      (LEAF_VOTER, enc(LEAF_VOTER_ABI, "gaugeStates", [g]))]
            types += [["address"], ["address"], ["uint128"],
                      ["uint128", "uint128", "uint48", "bool", "bool", "uint128", "uint256", "uint256", "(int128,int128,uint48,uint128)"]]
        res = self.rpc.multicall(calls, types, batch=200)
        rows = []
        for i, g in enumerate(gauges):
            rew, pool, cap, st = res[4 * i], res[4 * i + 1], res[4 * i + 2], res[4 * i + 3]
            bias, slope, ts, perm = st[8]
            decaying = max(0, bias - slope * max(0, self.head_ts - ts))
            rows.append(dict(gauge=g, factory=fac_of[g], rewards=rew[0], pool=pool[0], emission_cap_per_sec=cap[0] / 1e18,
                             is_activated=st[4], weight_now=(decaying + perm) / 1e18))
        df = pd.DataFrame(rows)
        # pool tokens / type
        calls, types = [], []
        for p in df.pool:
            calls += [(p, enc(POOL_ABI, "token0")), (p, enc(POOL_ABI, "token1")), (p, enc(POOL_ABI, "tickSpacing")), (p, enc(POOL_ABI, "symbol"))]
            types += [["address"], ["address"], ["int24"], ["string"]]
        res = self.rpc.multicall(calls, types, batch=200)
        df["token0"] = [res[4 * i][0] for i in range(n)]
        df["token1"] = [res[4 * i + 1][0] for i in range(n)]
        df["tick_spacing"] = [res[4 * i + 2][0] if res[4 * i + 2] else 0 for i in range(n)]
        psym = [res[4 * i + 3][0] if res[4 * i + 3] else "" for i in range(n)]
        df["pool_type"] = [f"CL{t}" if t > 0 else ("stable" if s.startswith("sAMM") else "volatile") for t, s in zip(df.tick_spacing, psym)]
        toks = sorted(set(df.token0) | set(df.token1))
        tmeta = self.token_meta(toks)
        df["symbol"] = [f"{tmeta[a]['symbol']}/{tmeta[b]['symbol']}" for a, b in zip(df.token0, df.token1)]
        df["label"] = df.symbol + " " + df.pool_type
        return df

    def token_meta(self, tokens: list[str]) -> dict:
        enc = lambda fn, t: encode_call(self.w3, ERC20_ABI, fn, [])
        calls, types = [], []
        for t in tokens:
            calls += [(t, enc("symbol", t)), (t, enc("decimals", t))]
            types += [["string"], ["uint8"]]
        res = self.rpc.multicall(calls, types, batch=200)
        out = {}
        for i, t in enumerate(tokens):
            out[t] = dict(symbol=res[2 * i][0] if res[2 * i] else t[:8], decimals=res[2 * i + 1][0] if res[2 * i + 1] else 18)
        return out

    # ---------------------------------------------------------------- fees
    def fetch_fee_events(self, meta: pd.DataFrame, from_block: int | None = None) -> pd.DataFrame:
        rewards = list(meta.rewards)
        rew2gauge = dict(zip(meta.rewards.str.lower(), meta.gauge))
        start = from_block if from_block is not None else self.launch_block
        jobs = []
        for a0 in range(0, len(rewards), 10):
            sub = rewards[a0:a0 + 10]
            f = start
            while f <= self.head:
                t = min(f + LOG_SPAN - 1, self.head)
                jobs.append((sub, f, t))
                f = t + 1
        log.info("fee log jobs: %d", len(jobs))
        rows = []

        def run(job):
            sub, f, t = job
            logs = self.rpc.call(lambda w3: w3.eth.get_logs({"address": [cs(a) for a in sub], "topics": [NOTIFY_FEES_TOPIC],
                                                             "fromBlock": f, "toBlock": t}))
            out = []
            for lg in logs:
                out.append(dict(block=lg["blockNumber"], tx=lg["transactionHash"].hex(), log_index=lg["logIndex"],
                                rewards=lg["address"].lower(), gauge=rew2gauge[lg["address"].lower()],
                                token="0x" + lg["topics"][2].hex()[-40:], amount=int(lg["data"].hex(), 16)))
            return out

        done = 0
        with ThreadPoolExecutor(max_workers=4) as ex:
            for out in ex.map(run, jobs):
                rows += out
                done += 1
                if done % 100 == 0:
                    log.info("  %d/%d jobs, %d events", done, len(jobs), len(rows))
        df = pd.DataFrame(rows)
        df["token"] = df.token.map(cs)
        df["ts"] = self.block_to_ts(df.block.values)
        df = df.sort_values(["block", "log_index"]).reset_index(drop=True)
        log.info("%d fee events", len(df))
        # supply (allocation voting power) of the gauge at the release timestamp
        calls = [(r, encode_call(self.w3, REWARDS_ABI, "supplyAt", [int(ts)])) for r, ts in zip(df.rewards, df.ts)]
        res = self.rpc.multicall(calls, [["uint256"]] * len(calls), batch=150)
        df["supply"] = [x[0] / 1e18 if x else float("nan") for x in res]
        tmeta = self.token_meta(sorted(set(df.token)))
        df["token_symbol"] = df.token.map(lambda t: tmeta[t]["symbol"])
        df["amount_f"] = [a / 10 ** tmeta[t]["decimals"] for a, t in zip(df.amount, df.token)]
        return df

    # ---------------------------------------------------------------- incentives
    def fetch_incentives(self, meta: pd.DataFrame) -> pd.DataFrame:
        calls = [(r, encode_call(self.w3, REWARDS_ABI, "incentiveCount", [])) for r in meta.rewards]
        counts = self.rpc.multicall(calls, [["uint256"]] * len(calls), batch=200)
        calls, keys = [], []
        for r, g, c in zip(meta.rewards, meta.gauge, counts):
            for pid in range(1, (c[0] if c else 0) + 1):
                calls.append((r, encode_call(self.w3, REWARDS_ABI, "incentive", [pid])))
                keys.append((r, g, pid))
        res = self.rpc.multicall(calls, [["(address,uint256,uint256,uint48,uint48,address)"]] * len(calls), batch=200)
        rows = []
        for (r, g, pid), x in zip(keys, res):
            token, amount, rate, start, end, creator = x[0]
            rows.append(dict(rewards=r, gauge=g, program_id=pid, token=cs(token), amount=amount, rate=rate, start=start, end=end, creator=creator))
        df = pd.DataFrame(rows)
        if len(df):
            tmeta = self.token_meta(sorted(set(df.token)))
            df["token_symbol"] = df.token.map(lambda t: tmeta[t]["symbol"])
            df["decimals"] = df.token.map(lambda t: tmeta[t]["decimals"])
            df["amount_f"] = df.amount / 10 ** df.decimals
            df["per_sec_f"] = df.rate / 1e18 / 10 ** df.decimals
        log.info("%d incentive programs", len(df))
        return df

    # ---------------------------------------------------------------- supply
    def repair_fee_ts(self, df: pd.DataFrame) -> pd.DataFrame:
        """Recompute ts (and the supply at that ts) for every fee event from real block timestamps."""
        new_ts = self.block_to_ts(df.block.values)
        bad = new_ts != df.ts.values
        log.info("repairing ts/supply for %d of %d fee events", bad.sum(), len(df))
        df.loc[bad, "ts"] = new_ts[bad]
        sub = df.loc[bad]
        calls = [(r, encode_call(self.w3, REWARDS_ABI, "supplyAt", [int(t)])) for r, t in zip(sub.rewards, sub.ts)]
        res = self.rpc.multicall(calls, [["uint256"]] * len(calls), batch=150)
        df.loc[bad, "supply"] = [x[0] / 1e18 if x else float("nan") for x in res]
        return df

    def fetch_supply_hourly(self, meta: pd.DataFrame) -> pd.DataFrame:
        hours = list(range(LAUNCH_TS, self.head_ts + 1, 3600))
        calls, keys = [], []
        for r, g in zip(meta.rewards, meta.gauge):
            for h in hours:
                calls.append((r, encode_call(self.w3, REWARDS_ABI, "supplyAt", [h])))
                keys.append((g, h))
        log.info("hourly supply calls: %d", len(calls))
        res = self.rpc.multicall(calls, [["uint256"]] * len(calls), batch=150)
        df = pd.DataFrame([dict(gauge=g, ts=h, supply=(x[0] / 1e18 if x else float("nan"))) for (g, h), x in zip(keys, res)])
        return df

    # ---------------------------------------------------------------- prices
    def fetch_prices(self, tokens: list[str]) -> pd.DataFrame:
        rows = []
        for t in tokens:
            key = PRICE_PROXY.get(t.lower(), f"base:{t}")
            meta = None
            # DefiLlama returns at most 500 points per request: walk the range in 500-hour chunks
            for start in range(LAUNCH_TS - 3600, self.head_ts + 3600, 500 * 3600):
                n = min(500, (self.head_ts + 3600 - start) // 3600 + 1)
                url = f"https://coins.llama.fi/chart/{key}?start={start}&span={n}&period=1h"
                j = None
                for attempt in range(4):
                    try:
                        j = requests.get(url, timeout=60).json()
                        break
                    except Exception as e:  # noqa: BLE001
                        log.warning("price fetch %s failed: %s", t, e)
                        time.sleep(3 * (attempt + 1))
                coin = (j or {}).get("coins", {}).get(key)
                if not coin:
                    continue
                meta = coin
                for p in coin["prices"]:
                    rows.append(dict(token=t, ts=p["timestamp"], price=p["price"], symbol=coin.get("symbol"), decimals=coin.get("decimals")))
                time.sleep(0.3)
            if meta is None:
                log.warning("no llama price for %s", t)
        df = pd.DataFrame(rows, columns=["token", "ts", "price", "symbol", "decimals"])
        df = df.drop_duplicates(["token", "ts"])
        return self.add_onchain(df, tokens)

    def add_onchain(self, llama: pd.DataFrame, tokens: list[str]) -> pd.DataFrame:
        """Replace missing / stale / patchy DefiLlama series with hourly on-chain pool prices."""
        llama = llama[llama.get("source", pd.Series("llama", index=llama.index)).fillna("llama").isin(["llama", ""])
                      ] if "source" in llama else llama
        bad = self.needs_onchain(llama, tokens)
        llama = llama.assign(source="llama")
        onchain = self.onchain_price_history(bad, llama[~llama.token.str.lower().isin({t.lower() for t in bad})])
        # replace a DefiLlama series only where an on-chain series exists; otherwise keep what DefiLlama has
        replaced = set(onchain.token.str.lower()) if len(onchain) else set()
        keep = llama[~llama.token.str.lower().isin(replaced)]
        log.info("prices: %d tokens from DefiLlama, %d replaced/added from on-chain pools, %d flagged without a pool",
                 keep.token.nunique(), len(replaced), len({t.lower() for t in bad} - replaced))
        return pd.concat([keep, onchain], ignore_index=True)

    def needs_onchain(self, llama: pd.DataFrame, tokens: list[str]) -> list[str]:
        """Tokens DefiLlama does not price reliably: no series, a series that stops more than 6h before head
        (DefiLlama stops updating some feeds; VEIL froze on 2026-11-09 at 6x its pool price), or a series that
        covers less than 70% of the hours since launch."""
        hours = (self.head_ts - LAUNCH_TS) / 3600
        st = llama.groupby(llama.token.str.lower()).ts.agg(["max", "count"])
        out = []
        for t in tokens:
            k = t.lower()
            if k not in st.index or st.at[k, "max"] < self.head_ts - 6 * 3600 or st.at[k, "count"] < 0.7 * hours:
                out.append(t)
        return out

    def onchain_price_history(self, tokens: list[str], llama: pd.DataFrame) -> pd.DataFrame:
        """Hourly USD price history from the deepest Aero pool pairing each token with a reliably priced token:
        pool price (CL slot0 or basic-pool reserves) at the block of every hour x the other token's DefiLlama
        price. A 3-hour rolling median removes single-block spikes."""
        gauges = pd.read_csv(os.path.join(DATA, "gauges.csv"))
        good = set(llama.token.str.lower()) - {t.lower() for t in tokens}
        grid = sorted(range(LAUNCH_TS, self.head_ts + 1, 3600))
        ref = llama.assign(token=llama.token.str.lower(), hour=(llama.ts // 3600) * 3600)
        ref = ref.groupby(["hour", "token"]).price.mean().unstack()
        ref = ref.reindex(sorted(set(ref.index) | set(grid))).ffill().bfill().loc[grid]
        bal_abi = [{"type": "function", "name": "balanceOf", "stateMutability": "view", "inputs": [{"type": "address"}], "outputs": [{"type": "uint256"}]}]
        slot0 = ["uint160", "int24", "uint16", "uint16", "uint16", "bool"]
        slot0_abi = [{"type": "function", "name": "slot0", "stateMutability": "view", "inputs": [], "outputs": [{"type": t} for t in slot0]}]
        res_abi = [{"type": "function", "name": "getReserves", "stateMutability": "view", "inputs": [], "outputs": [{"type": "uint256"}] * 3}]
        tmeta = self.token_meta(sorted({t for t in tokens} | {cs(t) for t in good if t in set(gauges.token0.str.lower()) | set(gauges.token1.str.lower())}))
        tmeta = {k.lower(): v for k, v in tmeta.items()}

        # pick the pool holding the most of the reliably priced side, measured now
        cands = []
        for t in tokens:
            tl = t.lower()
            for r in gauges.itertuples():
                t0, t1 = r.token0.lower(), r.token1.lower()
                if tl in (t0, t1):
                    other = t1 if tl == t0 else t0
                    if other in good:
                        cands.append((t, r.pool, other, tl == t0, int(r.tick_spacing)))
        bals = self.rpc.multicall([(o, encode_call(self.w3, bal_abi, "balanceOf", [cs(pl)])) for _, pl, o, _, _ in cands], [["uint256"]] * len(cands))
        best = {}
        for (t, pl, o, is0, ts), b in zip(cands, bals):
            usd = (b[0] if b else 0) / 10 ** tmeta[o]["decimals"] * ref[o].iloc[-1]
            if usd > best.get(t, (0,))[0]:
                best[t] = (usd, pl, o, is0, ts)
        chosen = [(t,) + v[1:] for t, v in best.items() if v[0] >= 1_000]   # ignore pools with < $1k of the priced side
        log.info("on-chain price history for %d of %d flagged tokens", len(chosen), len(tokens))

        self.block_to_ts(self.head)                                      # make sure the block/time grid exists
        xs, ys = self._ts_grid
        blocks = np.rint(np.interp(grid, ys, xs)).astype(int)
        calls = [(pl, encode_call(self.w3, slot0_abi if ts > 0 else res_abi, "slot0" if ts > 0 else "getReserves", []))
                 for _, pl, _, _, ts in chosen]
        types = [slot0 if ts > 0 else ["uint256"] * 3 for *_, ts in chosen]

        def at(b):
            return self.rpc.multicall(calls, types, block=int(b))
        with ThreadPoolExecutor(max_workers=4) as ex:
            snaps = list(ex.map(at, blocks))

        rows = []
        for j, (t, pl, o, is0, ts) in enumerate(chosen):
            d_t, d_o = tmeta[t.lower()]["decimals"], tmeta[o]["decimals"]
            ser = []
            for k, snap in enumerate(snaps):
                x = snap[j]
                px = np.nan
                if x:
                    if ts > 0:
                        p01 = (x[0] / 2 ** 96) ** 2                      # raw token1 per raw token0
                        raw = p01 if is0 else (1 / p01 if p01 > 0 else np.nan)
                    else:
                        r0, r1 = x[0], x[1]
                        raw = (r1 / r0 if is0 else r0 / r1) if r0 > 0 and r1 > 0 else np.nan
                    px = raw * 10 ** (d_t - d_o) * ref[o].iloc[k]
                ser.append(px)
            ser = pd.Series(ser, index=grid).where(lambda v: (v > 0) & (v < 1e7)).rolling(3, min_periods=1, center=True).median()
            for h, px in ser.dropna().items():
                rows.append(dict(token=t, ts=h, price=px, symbol=tmeta[t.lower()]["symbol"], decimals=d_t, source="onchain"))
        return pd.DataFrame(rows)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--stage", default="all")
    ap.add_argument("--incremental", action="store_true", help="fees: only fetch blocks after the last one in data/fee_events.csv")
    args = ap.parse_args()
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s", stream=sys.stderr)
    os.makedirs(DATA, exist_ok=True)
    f = Fetcher()
    st = args.stage
    mp = os.path.join(DATA, "gauges.csv")
    if st in ("all", "meta") or not os.path.exists(mp):
        meta = f.fetch_meta()
        meta.to_csv(mp, index=False)
    meta = pd.read_csv(mp)
    if st in ("all", "fees"):
        fp = os.path.join(DATA, "fee_events.csv")
        if args.incremental and os.path.exists(fp):
            old = pd.read_csv(fp)
            new = f.fetch_fee_events(meta, from_block=int(old.block.max()) + 1)
            df = pd.concat([old, new], ignore_index=True).drop_duplicates(["tx", "log_index"])
            log.info("appended %d new fee events (total %d)", len(new), len(df))
        else:
            df = f.fetch_fee_events(meta)
        df.to_csv(fp, index=False)
    if st == "onchain-prices":
        # rebuild on-chain series for unreliable tokens, keeping the stored DefiLlama data
        pp = os.path.join(DATA, "prices.csv")
        old = pd.read_csv(pp)
        toks = sorted(set(meta.token0) | set(meta.token1) | {AERO_TOKEN} | set(pd.read_csv(os.path.join(DATA, "incentives.csv")).token))
        f.add_onchain(old, toks).to_csv(pp, index=False)
    if st == "repair-ts":
        fp = os.path.join(DATA, "fee_events.csv")
        f.repair_fee_ts(pd.read_csv(fp)).to_csv(fp, index=False)
    if st in ("all", "incentives"):
        f.fetch_incentives(meta).to_csv(os.path.join(DATA, "incentives.csv"), index=False)
    if st in ("all", "supply"):
        f.fetch_supply_hourly(meta).to_csv(os.path.join(DATA, "supply_hourly.csv"), index=False)
    if st in ("all", "prices"):
        toks = set(meta.token0) | set(meta.token1) | {AERO_TOKEN}
        ip = os.path.join(DATA, "incentives.csv")
        if os.path.exists(ip):
            inc = pd.read_csv(ip)
            if len(inc):
                toks |= set(inc.token)
        f.fetch_prices(sorted(toks)).to_csv(os.path.join(DATA, "prices.csv"), index=False)


if __name__ == "__main__":
    main()
