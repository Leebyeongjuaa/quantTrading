"""Fluid vaults (Ethereum): large ETH/BTC-collateral, USD-debt positions and vault liquidations.

  python3 fluid.py    downloads
      data/fluid_liquidations.csv   LogLiquidate events of the vaults below
      data/fluid_snapshots.csv      daily positions (pos = "<vault>:<nftId>") with >= MIN_DEBT_USD
                                    of debt and the ETH/BTC price at which they become liquidatable
Fluid liquidates per tick, not per position: a position is liquidatable once
debt / (collateral * price) exceeds the vault's liquidation threshold, so
P_liq = debt / (collateral * threshold * collateral-to-base ratio). Whether a given position
was actually hit by a vault liquidation is read from its balance before and after.
"""
import collections
import csv
import sys
from concurrent.futures import ThreadPoolExecutor

import numpy as np
import pandas as pd
from eth_abi import decode, encode

import rpc
from snapshots import ORACLE, WETH, multicall

POSITIONS_RESOLVER = "0xaa21a86030eaa16546a759d2d10fd3bf9d053bc7"
LOG_OPERATE = "0xfef64760e30a41b9d5ba7dd65ff7236a61d89ed8b44c67a29e84db1a67513a1c"
LOG_LIQUIDATE = "0x80fd9cc6b1821f4a510e45ffce6852ea3404807b5d3d833ffa85664408afcb66"
WEETH = "0xcd5fe23c85820f7b72d0926fc9b05b43e359b7ee"
FIRST_BLOCK = 19_500_000  # before the first of these vaults was deployed
MIN_CUMULATIVE_BORROW_USD = 1_000_000
MIN_DEBT_USD = 1_000_000
# The T1 vaults (plain collateral, plain debt) that carry almost all ETH/BTC-vs-USD liquidation volume.
# address: (class, collateral decimals, debt decimals, collateral priced via Aave oracle asset or None)
VAULTS = {
    "0x0c8c77b7ff4c2af7f6cebbe67350a490e3dd6cb3": ("ETH", 18, 6, None),  # 11 ETH / USDC
    "0xe16a6f5359abb1f61ce71e25dd0932e3e00b00eb": ("ETH", 18, 6, None),  # 12 ETH / USDT
    "0xf2c8f54447cbd591c396b0dd7ac15faf552d0fa4": ("ETH", 18, 6, WEETH),  # 19 weETH / USDC
    "0x6f72895cf6904489bcd862c941c3d02a3ee4f03e": ("BTC", 8, 6, None),  # 21 WBTC / USDC
    "0x3a0b7c8840d74d39552ef53f586dd8c3d1234c40": ("BTC", 8, 6, None),  # 22 WBTC / USDT
    "0x01c7c1c41dea58b043e700efb23dc077f12a125e": ("BTC", 8, 6, None),  # 29 cbBTC / USDC
    "0xe6b5d1cdc4935295c84772c4700932b4bfc93274": ("BTC", 8, 6, None),  # 30 cbBTC / USDT
}


def signed(word):
    v = int(word, 16)
    return v - (1 << 256) if v >> 255 else v


def vault_calls(vault, nft_ids):
    """Threshold word, collateral ratio prices, then one call per position (a batched position
    query reverts as a whole when any of its NFTs does not exist yet at that block)."""
    priced_via = VAULTS[vault][3] or WETH
    return [
        (vault, bytes.fromhex("b5c736e4") + (1).to_bytes(32, "big")),  # readFromStorage(vaultVariables2)
        (ORACLE, bytes.fromhex("b3596f07") + encode(["address"], [priced_via])),
        (ORACLE, bytes.fromhex("b3596f07") + encode(["address"], [WETH])),
    ] + [(POSITIONS_RESOLVER, bytes.fromhex("6970693c") + encode(["uint256[]", "address"], [[n], vault])) for n in nft_ids]


def value_positions(vault, out):
    """-> {nftId: (p_liq of the base asset, debt_usd, collateral units)} from vault_calls results."""
    if not all(ok and ret for ok, ret in out[:3]):
        return {}
    cls, col_dec, debt_dec, priced_via = VAULTS[vault]
    threshold = ((int.from_bytes(out[0][1], "big") >> 42) & 1023) / 1000
    ratio = int.from_bytes(out[1][1], "big") / int.from_bytes(out[2][1], "big") if priced_via else 1.0
    res = {}
    for ok, ret in out[3:]:
        if not ok or not ret:
            continue
        for nft, _, supply, borrow in decode(["(uint256,address,uint256,uint256)[]"], ret)[0]:
            if supply and borrow and threshold:
                collateral, debt = supply / 10**col_dec, borrow / 10**debt_dec
                res[nft] = (debt / (collateral * threshold * ratio), debt, collateral)
    return res


def state(pos, block, worker=0):
    vault, nft = pos.split(":")
    v = value_positions(vault, multicall(vault_calls(vault, [int(nft)]), block, worker)).get(int(nft))
    return list(v) if v else None


def main():
    head = int(rpc.call("eth_blockNumber", []), 16)
    vaults = list(VAULTS)
    cumulative = collections.Counter()
    for lo in range(FIRST_BLOCK, head + 1, 50_000):
        for l in rpc.get_logs(vaults, [LOG_OPERATE], lo, min(lo + 49_999, head)):
            d = l["data"][2:]
            debt = signed(d[192:256])
            if debt > 0:
                cumulative[(l["address"], int(d[64:128], 16))] += debt / 10 ** VAULTS[l["address"]][2]
        print("operate", lo, len(cumulative), file=sys.stderr, flush=True)
    universe = collections.defaultdict(list)
    for (vault, nft), usd in cumulative.items():
        if usd >= MIN_CUMULATIVE_BORROW_USD:
            universe[vault].append(nft)
    print({v[:8]: len(n) for v, n in universe.items()}, file=sys.stderr)

    blocks = list(pd.read_csv("data/oracle_base.csv").block)
    with open("data/fluid_liquidations.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["block", "tx", "vault", "cls", "debt_usd"])
        for lo in range(blocks[0], head + 1, 50_000):
            for l in rpc.get_logs(vaults, [LOG_LIQUIDATE], lo, min(lo + 49_999, head)):
                cls, _, debt_dec, _ = VAULTS[l["address"]]
                w.writerow([int(l["blockNumber"], 16), l["transactionHash"], l["address"], cls,
                            round(int(l["data"][2:][128:192], 16) / 10**debt_dec, 2)])

    def snapshot(args):
        block, worker = args
        rows = []
        for vault, nfts in universe.items():
            for i in range(0, len(nfts), 150):
                out = multicall(vault_calls(vault, nfts[i : i + 150]), block, worker)
                for nft, (p_liq, debt, _) in value_positions(vault, out).items():
                    if debt >= MIN_DEBT_USD:
                        rows.append([block, f"{vault}:{nft}", VAULTS[vault][0], round(debt), p_liq])
        return rows

    with open("data/fluid_snapshots.csv", "w", newline="") as f, ThreadPoolExecutor(6) as pool:
        w = csv.writer(f)
        w.writerow(["block", "pos", "cls", "debt_usd", "p_est"])
        for n, rows in enumerate(pool.map(snapshot, [(b, i) for i, b in enumerate(blocks)])):
            w.writerows(rows)
            if n % 30 == 0:
                print("snapshot", n, len(blocks), file=sys.stderr, flush=True)


class Fluid:
    """Backtest source. Daily snapshots are exact; liquidation hits are detected on demand."""

    name = "fluid"
    exact_daily = True

    def __init__(self, cache, last_block):
        self.cache = cache
        snaps = pd.read_csv("data/fluid_snapshots.csv")
        self.daily = snaps[snaps.debt_usd >= float(__import__("os").environ.get("MIN_DEBT_USD", 2_000_000))]
        liq = pd.read_csv("data/fluid_liquidations.csv")
        self.vault_liq_blocks = liq.groupby("vault").block.apply(lambda s: np.sort(s.unique())).to_dict()

    def prefetch(self, keys):
        pass

    def _state(self, pos, block):
        return self.cache.get(f"fluid:{pos}:{block}", lambda: state(pos, block))

    def live(self, pos, cls, block):
        s = self._state(pos, block)
        return s[:2] if s else None

    def liquidation(self, pos, after, until):
        """First vault liquidation in (after, until] that reduced this position's debt."""
        blocks = self.vault_liq_blocks.get(pos.split(":")[0], [])
        for b in blocks[np.searchsorted(blocks, after, side="right") :]:
            if b > until:
                break
            before, now = self._state(pos, int(b) - 1), self._state(pos, int(b))
            if before and (now is None or (now[1] < before[1] * 0.995 and now[2] < before[2])):
                return int(b)
        return None


if __name__ == "__main__":
    main()
