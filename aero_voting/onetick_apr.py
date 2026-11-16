"""One-tick emission APR for the highest-volume Aero Slipstream pools on Base.

"One tick" = the narrowest CL range a position can take: one tickSpacing wide, around the current price.
Method (exact, no reward-rate assumption):
  pool.rewardGrowthGlobalX128 accrues AERO per unit of *staked in-range* liquidity (Q128). Its change over a
  window is what a staked position that stayed in range the whole time earned per unit liquidity. Dividing
  by the USD value of one unit of liquidity spread over one tick spacing at the current price gives the
  marginal in-range emission APR of a one-tick position (what the app shows as the max APR for the pool).

Volume ranking: GeckoTerminal (dexes aero-slipstream-base / aero-volatile-base / aero-stable-base).
Run:  python -m aero_voting.onetick_apr [--top 10]
"""
import argparse
import math
import time

import pandas as pd
import requests
from web3 import Web3

from .config import AERO_TOKEN, ERC20_ABI
from .rpc import Rpc, encode_call

Q128 = 2 ** 128
U256 = 2 ** 256
YEAR = 365 * 86400
OLD_AERO = "0x940181a94A35A4569E4529A3CDfB74e38FD98631"  # pre-migration AERO, 1:1 price proxy on DefiLlama
GT = "https://api.geckoterminal.com/api/v2/networks/base"
POOL_ABI = [
    {"type": "function", "name": n, "stateMutability": "view", "inputs": [], "outputs": [{"type": t}]}
    for n, t in [("rewardGrowthGlobalX128", "uint256"), ("stakedLiquidity", "uint128"), ("liquidity", "uint128"),
                 ("tickSpacing", "int24"), ("token0", "address"), ("token1", "address"), ("fee", "uint24")]
] + [{"type": "function", "name": "slot0", "stateMutability": "view", "inputs": [], "outputs": [
    {"type": "uint160"}, {"type": "int24"}, {"type": "uint16"}, {"type": "uint16"}, {"type": "uint16"}, {"type": "bool"}]}]


def top_pools(n: int) -> pd.DataFrame:
    rows = []
    for dex in ["aero-slipstream-base", "aero-volatile-base", "aero-stable-base"]:
        j = requests.get(f"{GT}/dexes/{dex}/pools", params={"sort": "h24_volume_usd_desc", "page": 1}, timeout=60).json()
        for p in j.get("data", []):
            a = p["attributes"]
            rows.append(dict(pool=Web3.to_checksum_address(a["address"]), name=a["name"], dex=dex,
                             vol24=float(a["volume_usd"]["h24"] or 0), tvl=float(a["reserve_in_usd"] or 0)))
        time.sleep(1.5)
    return pd.DataFrame(rows).sort_values("vol24", ascending=False).head(n).reset_index(drop=True)


def llama_prices(tokens: list[str]) -> dict:
    keys = ",".join(f"base:{t}" for t in tokens)
    j = requests.get(f"https://coins.llama.fi/prices/current/{keys}", timeout=60).json()["coins"]
    return {k.split(":")[1].lower(): v["price"] for k, v in j.items()}


def usd_per_unit_liquidity(sqrtp: float, tick: int, ts: int, d0: int, d1: int, px0: float, px1: float):
    """USD value of L=1 spread over the tick-spacing bucket that contains the current tick."""
    lo = math.floor(tick / ts) * ts
    sa, sb = 1.0001 ** (lo / 2), 1.0001 ** ((lo + ts) / 2)
    a0 = (1 / sqrtp - 1 / sb)          # raw token0 per unit L
    a1 = (sqrtp - sa)                  # raw token1 per unit L
    return a0 / 10 ** d0 * px0 + a1 / 10 ** d1 * px1, lo


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--top", type=int, default=10)
    args = ap.parse_args()

    pools = top_pools(args.top)
    rpc = Rpc(["https://base-rpc.publicnode.com"])
    w3 = rpc.w3
    head = rpc.block_number()
    now = rpc.block_ts(head)
    windows = {"24h": rpc.find_block_at(now - 86400), "7d": rpc.find_block_at(now - 7 * 86400)}

    enc = lambda fn: encode_call(w3, POOL_ABI, fn, [])
    fns = ["rewardGrowthGlobalX128", "stakedLiquidity", "liquidity", "tickSpacing", "token0", "token1", "slot0", "fee"]
    types = [["uint256"], ["uint128"], ["uint128"], ["int24"], ["address"], ["address"],
             ["uint160", "int24", "uint16", "uint16", "uint16", "bool"], ["uint24"]]
    cur = rpc.multicall([(p, enc(f)) for p in pools.pool for f in fns], types * len(pools))
    past = {k: rpc.multicall([(p, enc(f)) for p in pools.pool for f in fns[:2]], types[:2] * len(pools), block=b)
            for k, b in windows.items()}
    past_ts = {k: rpc.block_ts(b) for k, b in windows.items()}

    toks = sorted({cur[i * len(fns) + 4][0] for i in range(len(pools))} | {cur[i * len(fns) + 5][0] for i in range(len(pools))})
    dec = rpc.multicall([(t, encode_call(w3, ERC20_ABI, "decimals", [])) for t in toks], [["uint8"]] * len(toks))
    dec = {t.lower(): d[0] for t, d in zip(toks, dec)}
    px = llama_prices(toks + [OLD_AERO])
    aero_px = px[OLD_AERO.lower()]

    rows = []
    for i, p in enumerate(pools.itertuples()):
        g, staked, liq, ts, t0, t1, s0, fee = cur[i * len(fns):(i + 1) * len(fns)]
        t0, t1, ts = t0[0].lower(), t1[0].lower(), ts[0]
        d0, d1 = dec[t0], dec[t1]
        sqrtp = s0[0] / 2 ** 96
        p01 = sqrtp ** 2 * 10 ** (d0 - d1)  # token1 per token0
        p0, p1 = px.get(t0), px.get(t1)
        if p0 is None and p1 is not None:
            p0 = p01 * p1
        if p1 is None and p0 is not None:
            p1 = p0 / p01
        usd_l, lo = usd_per_unit_liquidity(sqrtp, s0[1], ts, d0, d1, p0, p1)
        r = dict(pool=p.name, vol24_musd=p.vol24 / 1e6, tvl_musd=p.tvl / 1e6, tick_spacing=ts,
                 fee_pct=fee[0] / 1e4, range_pct=(1.0001 ** ts - 1) * 100,
                 staked_share=staked[0] / liq[0] if liq[0] else float("nan"),
                 intick_staked_kusd=staked[0] * usd_l / 1e3)  # $ already staked in the active bucket (dilution)
        for k in windows:
            g0, _ = past[k][2 * i], past[k][2 * i + 1]
            dg = (g[0] - g0[0]) % U256
            aero_per_l_per_s = dg / Q128 / 1e18 / (now - past_ts[k])
            r[f"apr_{k}"] = aero_per_l_per_s * YEAR * aero_px / usd_l * 100
        rows.append(r)
    df = pd.DataFrame(rows)

    # How long does price stay in one tick bucket? Sample slot0 every 5 min over the last 24h
    # (blocks interpolated linearly between the 24h-ago block and head; Base block time is constant within it).
    b0 = windows["24h"]
    n = 288
    sample_blocks = [b0 + round((head - b0) * k / n) for k in range(n + 1)]
    ticks = [[] for _ in range(len(pools))]
    for b in sample_blocks:
        res = rpc.multicall([(p, enc("slot0")) for p in pools.pool], [types[6]] * len(pools), block=b)
        for i, x in enumerate(res):
            ticks[i].append(x[1])
    for i in range(len(pools)):
        ts = df.at[i, "tick_spacing"]
        bk = [math.floor(t / ts) for t in ticks[i]]
        runs, cur_run = [], 1
        for a, b in zip(bk, bk[1:]):
            if a == b:
                cur_run += 1
            else:
                runs.append(cur_run)
                cur_run = 1
        runs.append(cur_run)
        df.at[i, "avg_dwell_min"] = 5 * sum(r * r for r in runs) / sum(runs)   # time-weighted mean stay, >= 5
        df.at[i, "static_in_range_24h"] = sum(1 for b in bk if b == bk[0]) / len(bk) * 100
        df.at[i, "apr_static_24h"] = df.at[i, "apr_24h"] * df.at[i, "static_in_range_24h"] / 100
    print(f"block {head}  {pd.to_datetime(now, unit='s')} UTC   AERO ${aero_px:.4f}")
    print(df.to_string(index=False, float_format=lambda x: f"{x:,.1f}"))
    return df


if __name__ == "__main__":
    main()
