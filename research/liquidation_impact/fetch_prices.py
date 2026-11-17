"""Per-block DEX prices in USD.

  python3 fetch_prices.py        ETH: open/low/high/close from the Uniswap v3 USDC/WETH 0.05%
                                 pool -> data/prices.csv
  python3 fetch_prices.py btc    BTC: last trade per block from the WBTC/USDC 0.05% pool and
                                 the WBTC/WETH 0.05% pool (times the ETH price of that block)
                                 -> data/prices_btc.csv (needs data/prices.csv)
Blocks without a swap are omitted; forward-fill the close when a dense series is needed.
"""
import csv
import sys

import pandas as pd

import rpc
from fetch_swaps import V3_SWAP, V3_USDC_WETH

STEP = 4000


def main():
    liq = pd.read_csv("data/liquidations.csv", usecols=["block"])
    head = int(rpc.call("eth_blockNumber", []), 16)
    start, end = int(liq.block.min()) - 7200, min(head, int(liq.block.max()) + 7200)
    with open("data/prices.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["block", "open", "low", "high", "close", "n"])
        for lo in range(start, end + 1, STEP):
            cur, o, l, h, c, n = None, 0, 0, 0, 0, 0
            for log in rpc.get_logs(V3_USDC_WETH, [V3_SWAP], lo, min(lo + STEP - 1, end), step=STEP):
                b = int(log["blockNumber"], 16)
                p = 1e12 / (int(log["data"][2:][128:192], 16) / 2**96) ** 2
                if b != cur:
                    if cur is not None:
                        w.writerow([cur, o, l, h, c, n])
                    cur, o, l, h, n = b, p, p, p, 0
                l, h, c, n = min(l, p), max(h, p), p, n + 1
            if cur is not None:
                w.writerow([cur, o, l, h, c, n])
            if (lo - start) // STEP % 50 == 0:
                print(lo, file=sys.stderr, flush=True)


V3_WBTC_USDC = "0x9a772018fbd77fcd2d25657e5c547baff3fd7d16"  # token0 WBTC (8 dec), token1 USDC (6 dec)
V3_WBTC_WETH = "0x4585fe77225b41b697c938b018e2ac67ac5a20c0"  # token0 WBTC (8 dec), token1 WETH (18 dec)


def btc():
    eth = pd.read_csv("data/prices.csv", usecols=["block", "close"]).set_index("block").close
    start, end = int(eth.index.min()), int(eth.index.max())
    eth = eth.reindex(range(start, end + 1)).ffill()
    ticks = {}  # (block, log_index) -> BTC price in USD
    for pool in (V3_WBTC_USDC, V3_WBTC_WETH):
        for lo in range(start, end + 1, 50_000):
            for log in rpc.get_logs(pool, [V3_SWAP], lo, min(lo + 49_999, end)):
                b = int(log["blockNumber"], 16)
                raw = (int(log["data"][2:][128:192], 16) / 2**96) ** 2  # token1 per token0, raw units
                price = raw * 100 if pool == V3_WBTC_USDC else raw / 1e10 * eth[b]
                ticks[(b, int(log["logIndex"], 16))] = price
            print(pool[:8], lo, len(ticks), file=sys.stderr, flush=True)
    last = {}
    for (b, _), price in sorted(ticks.items()):
        last[b] = price
    pd.Series(last, name="close").rename_axis("block").to_csv("data/prices_btc.csv")


if __name__ == "__main__":
    btc() if sys.argv[1:] == ["btc"] else main()
