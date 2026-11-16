"""Download ETH/USDC swap ticks around ETH-collateral liquidations into data/swaps.csv.

Reference pools:
  v4  ETH/USDC 1bp  (PoolManager, pool id below)  - tightest fee, main price series
  v3  USDC/WETH 5bp (0x88e6...)                   - robustness check
"""
import csv
import sys

import pandas as pd

import rpc

WINDOW = 110  # blocks fetched on each side of a liquidation block

V4_MANAGER = "0x000000000004444c5dc75cb358380d2e3de08a90"
V4_SWAP = "0x40e9cecb9f5f1f1c5b9c97dec2917b7ee92e57ba5563708daca94dd84ad7112f"
V4_ETH_USDC = "0x00b9edc1583bf6ef09ff3a09f6c23ecb57fd7d0bb75625717ec81eed181e22d7"
V3_SWAP = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"
V3_USDC_WETH = "0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640"

ETH_LIKE = {
    "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",  # WETH
    "0x7f39c581f595b53c5cb19bd0b3f8da6c935e2ca0",  # wstETH
    "0xcd5fe23c85820f7b72d0926fc9b05b43e359b7ee",  # weETH
    "0xae78736cd615f374d3085123a210448e74fc6393",  # rETH
    "0xbe9895146f7af43049ca1c1ae358b0541ea49704",  # cbETH
}
USD_DEBT = {  # address -> decimals
    "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48": 6,  # USDC
    "0xdac17f958d2ee523a2206206994597c13d831ec7": 6,  # USDT
    "0x6c3ea9036406852006290770bedfcaba0e23a0e8": 6,  # PYUSD
    "0x6b175474e89094c44da98b954eedeac495271d0f": 18,  # DAI
    "0x40d16fc0246ad3160ccc09b8d0d3a2cd28ae6c2f": 18,  # GHO
    "0x4c9edd5852cd905f086c759e8383e09bff1e68b3": 18,  # USDe
}


def signed(word):
    v = int(word, 16)
    return v - (1 << 256) if v >> 255 else v


def eth_long_liquidations():
    """Liquidations where ETH-like collateral is seized against USD debt (ETH gets sold)."""
    df = pd.read_csv("data/liquidations.csv")
    df = df[df.collateral.isin(ETH_LIKE) & df.debt.isin(USD_DEBT)].copy()
    df["usd"] = [int(a) / 10 ** USD_DEBT[d] for a, d in zip(df.debt_to_cover, df.debt)]
    return df


def merged_windows(blocks):
    spans = []
    for b in sorted(set(blocks)):
        lo, hi = b - WINDOW, b + WINDOW
        if spans and lo <= spans[-1][1] + 1:
            spans[-1][1] = max(spans[-1][1], hi)
        else:
            spans.append([lo, hi])
    return spans


def main():
    spans = merged_windows(eth_long_liquidations().block)
    total = sum(hi - lo + 1 for lo, hi in spans)
    print(f"{len(spans)} windows, {total} blocks", file=sys.stderr)
    with open("data/swaps.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["pool", "block", "tx_index", "log_index", "tx", "price", "usd"])
        for i, (lo, hi) in enumerate(spans):
            for l in rpc.get_logs(V4_MANAGER, [V4_SWAP, V4_ETH_USDC], lo, hi, step=5000):
                d = l["data"][2:]
                sq = int(d[128:192], 16)
                price = (sq / 2**96) ** 2 * 1e12  # USDC per ETH
                usd = abs(signed(d[64:128])) / 1e6
                w.writerow(["v4", int(l["blockNumber"], 16), int(l["transactionIndex"], 16),
                            int(l["logIndex"], 16), l["transactionHash"], price, usd])
            for l in rpc.get_logs(V3_USDC_WETH, [V3_SWAP], lo, hi, step=5000):
                d = l["data"][2:]
                sq = int(d[128:192], 16)
                price = 1e12 / (sq / 2**96) ** 2
                usd = abs(signed(d[0:64])) / 1e6
                w.writerow(["v3", int(l["blockNumber"], 16), int(l["transactionIndex"], 16),
                            int(l["logIndex"], 16), l["transactionHash"], price, usd])
            if i % 50 == 0:
                print(i, len(spans), file=sys.stderr)


if __name__ == "__main__":
    main()
