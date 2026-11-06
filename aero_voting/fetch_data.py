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

import pandas as pd
import requests
from web3 import Web3

from .config import (AERO_TOKEN, ERC20_ABI, FACTORY_REGISTRY, LAUNCH_TS, LEAF_VOTER, LEAF_VOTER_ABI,
                     POOL_ABI, REGISTRY_ABI, REWARDS_ABI)
from .rpc import Rpc, encode_call

log = logging.getLogger("aero.fetch")
DATA = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "data")
NOTIFY_FEES_TOPIC = "0x87e2847214b11ebf92b00367c6c1a37b67c5bab8ff351feec88d712e1775de3a"
BLOCK_SEC = 2
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

    def block_to_ts(self, b: int) -> int:
        # Base has a fixed 2s block time; verified exact between launch and head.
        return self.launch_block_ts + (b - self.launch_block) * BLOCK_SEC

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
                t = min(f + 10_000 - 1, self.head)
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
        df["ts"] = df.block.map(self.block_to_ts)
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
        span = (self.head_ts - LAUNCH_TS) // 3600 + 2
        for t in tokens:
            key = PRICE_PROXY.get(t.lower(), f"base:{t}")
            url = f"https://coins.llama.fi/chart/{key}?start={LAUNCH_TS - 3600}&span={span}&period=1h"
            for attempt in range(4):
                try:
                    j = requests.get(url, timeout=60).json()
                    break
                except Exception as e:  # noqa: BLE001
                    log.warning("price fetch %s failed: %s", t, e)
                    time.sleep(3 * (attempt + 1))
            else:
                continue
            coin = j.get("coins", {}).get(key)
            if not coin:
                log.warning("no llama price for %s", t)
                continue
            for p in coin["prices"]:
                rows.append(dict(token=t, ts=p["timestamp"], price=p["price"], symbol=coin.get("symbol"), decimals=coin.get("decimals")))
            time.sleep(0.3)
        df = pd.DataFrame(rows)
        missing = [t for t in tokens if t not in set(df.token)]
        spot = self.pool_spot_prices(missing, df)
        if len(spot):
            df = pd.concat([df, spot], ignore_index=True)
        return df

    def pool_spot_prices(self, missing: list[str], priced: pd.DataFrame) -> pd.DataFrame:
        """Fallback: price a token from the current spot of an Aero pool pairing it with a priced token.
        One constant price (stamped at launch) - acceptable for tokenized stocks / long-tail over a ~2 week window."""
        gauges = pd.read_csv(os.path.join(DATA, "gauges.csv"))
        last_px = priced.sort_values("ts").groupby("token").price.last().to_dict()
        tmeta = self.token_meta(sorted(set(missing) | set(last_px)))
        rows, w3 = [], self.w3
        slot0_abi = [{"type": "function", "name": "slot0", "stateMutability": "view", "inputs": [], "outputs": [
            {"type": "uint160"}, {"type": "int24"}, {"type": "uint16"}, {"type": "uint16"}, {"type": "uint16"}, {"type": "bool"}]}]
        res_abi = [{"type": "function", "name": "getReserves", "stateMutability": "view", "inputs": [], "outputs": [
            {"type": "uint256"}, {"type": "uint256"}, {"type": "uint256"}]}]
        for t in missing:
            cands = gauges[((gauges.token0.str.lower() == t.lower()) & gauges.token1.isin(last_px)) |
                           ((gauges.token1.str.lower() == t.lower()) & gauges.token0.isin(last_px))]
            cands = cands.sort_values("weight_now", ascending=False)
            for pool in cands.itertuples():
                try:
                    is0 = pool.token0.lower() == t.lower()
                    other = pool.token1 if is0 else pool.token0
                    d_t, d_o = tmeta[t]["decimals"], tmeta[other]["decimals"]
                    if pool.tick_spacing > 0:
                        c = w3.eth.contract(address=cs(pool.pool), abi=slot0_abi)
                        sqrt_p = self.rpc.call(lambda w3: c.functions.slot0().call())[0]
                        p01 = (sqrt_p / 2 ** 96) ** 2  # raw token1 per raw token0
                        px_t_in_other = (p01 if is0 else 1 / p01) * 10 ** ((d_t - d_o) if is0 else (d_t - d_o))
                    else:
                        c = w3.eth.contract(address=cs(pool.pool), abi=res_abi)
                        r0, r1 = self.rpc.call(lambda w3: c.functions.getReserves().call())[:2]
                        if r0 == 0 or r1 == 0:
                            continue
                        px_t_in_other = (r1 / 10 ** d_o) / (r0 / 10 ** d_t) if is0 else (r0 / 10 ** d_o) / (r1 / 10 ** d_t)
                    px = px_t_in_other * last_px[other]
                    if 0 < px < 1e7:  # absurd spot = empty / broken pool
                        rows.append(dict(token=t, ts=LAUNCH_TS, price=px, symbol=tmeta[t]["symbol"], decimals=d_t, source="pool_spot"))
                        break
                except Exception as e:  # noqa: BLE001
                    log.debug("spot price %s via %s failed: %s", t, pool.pool, e)
        log.info("pool-spot priced %d of %d tokens missing from DefiLlama", len(rows), len(missing))
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
