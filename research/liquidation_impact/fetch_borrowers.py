"""Find every address that ever borrowed a meaningful amount of USD stablecoins on Aave v3.

Scans Borrow events since the pool's deployment and writes data/borrowers.csv
(one row per borrower with cumulative and largest single borrow in USD).
"""
import collections
import csv
import os
import sys

import rpc

AAVE_V3_POOL = "0x87870bca3f3fd6335c3f4ce8392d69350b4fa4e2"
POOL_DEPLOY_BLOCK = 16_291_127
BORROW = "0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0"
STABLES = {  # address -> decimals
    "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48": 6,  # USDC
    "0xdac17f958d2ee523a2206206994597c13d831ec7": 6,  # USDT
    "0x6c3ea9036406852006290770bedfcaba0e23a0e8": 6,  # PYUSD
    "0x6b175474e89094c44da98b954eedeac495271d0f": 18,  # DAI
    "0x40d16fc0246ad3160ccc09b8d0d3a2cd28ae6c2f": 18,  # GHO
    "0x4c9edd5852cd905f086c759e8383e09bff1e68b3": 18,  # USDe
    "0xdc035d45d973e3ec169d2276ddab16f1e407384f": 18,  # USDS
    "0x8292bb45bf1ee4d140127049757c2e0ff06317ed": 18,  # RLUSD
    "0xf939e0a03fb07f59a73314e73794be0e57ac1b4e": 18,  # crvUSD
    "0x5f98805a4e8be255a32880fdec7f6728c6568ba0": 18,  # LUSD
}
MIN_CUMULATIVE_USD = float(os.environ.get("MIN_CUMULATIVE_USD", 500_000))


def main():
    head = int(rpc.call("eth_blockNumber", [], prefer=1), 16)
    reserve_topics = ["0x" + a[2:].rjust(64, "0") for a in STABLES]
    total = collections.Counter()
    largest = collections.Counter()
    last_block = {}
    for lo in range(POOL_DEPLOY_BLOCK, head + 1, 50_000):
        logs = rpc.get_logs(AAVE_V3_POOL, [BORROW, reserve_topics], lo, min(lo + 49_999, head), prefer=1)
        for l in logs:
            user = "0x" + l["topics"][2][26:]
            usd = int(l["data"][2:][64:128], 16) / 10 ** STABLES["0x" + l["topics"][1][26:]]
            total[user] += usd
            largest[user] = max(largest[user], usd)
            last_block[user] = int(l["blockNumber"], 16)
        print(lo, len(total), file=sys.stderr, flush=True)
    keep = [u for u, v in total.items() if v >= MIN_CUMULATIVE_USD]
    with open("data/borrowers.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["user", "cumulative_usd", "largest_usd", "last_borrow_block"])
        for u in sorted(keep, key=lambda u: -total[u]):
            w.writerow([u, round(total[u]), round(largest[u]), last_block[u]])
    print(f"{len(total)} stablecoin borrowers, {len(keep)} with >= ${MIN_CUMULATIVE_USD:,} cumulative")


if __name__ == "__main__":
    main()
