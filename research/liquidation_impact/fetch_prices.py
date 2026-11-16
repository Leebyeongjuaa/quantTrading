"""Per-block ETH price (open/low/high/close) from the Uniswap v3 USDC/WETH 0.05% pool.

Writes data/prices.csv covering the same block range as data/liquidations.csv.
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
    start, end = int(liq.block.min()) - 7200, int(rpc.call("eth_blockNumber", []), 16)
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


if __name__ == "__main__":
    main()
