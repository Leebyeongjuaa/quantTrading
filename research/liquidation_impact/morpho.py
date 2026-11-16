"""Morpho Blue (Ethereum): large ETH/BTC-collateral, USD-loan positions and their liquidations.

  python3 morpho.py    downloads
      data/morpho_markets.csv       markets with ETH- or BTC-like collateral and a USD loan asset
      data/morpho_liquidations.csv  Liquidate events (pos = "<marketId>:<borrower>")
      data/morpho_snapshots.csv     daily positions with >= MIN_DEBT_USD of debt and the price of
                                    ETH/BTC at which they become liquidatable
      data/oracle_base.csv          Aave oracle ETH and BTC prices at every snapshot block
A position is liquidatable when borrowed > collateral * oraclePrice * LLTV, so its health
factor scales one-for-one with the collateral's USD price.
"""
import collections
import csv
import sys
from concurrent.futures import ThreadPoolExecutor

import pandas as pd
import requests
from eth_abi import decode, encode

import rpc
from snapshots import ORACLE, WETH, multicall

MORPHO = "0xbbbbbbbbbb9cc5e90e3b3af64bdaf62c37eeffcb"
WBTC = "0x2260fac5e5542a773aa44fbcfedf7c193bc2c599"
BORROW = "0x570954540bed6b1304a87dfe815a5eda4a648f7097a16240dcd85c9b5fd42a43"
LIQUIDATE = "0xa4946ede45d0c6f06a0f5ce92c9ad3b4751452d2fe0e25010783bcab57a67e41"
ETH_SYMBOLS = {"WETH", "wstETH", "weETH", "rETH", "cbETH", "osETH", "rsETH", "ETHx", "ezETH", "pufETH", "rswETH"}
BTC_SYMBOLS = {"WBTC", "cbBTC", "tBTC", "LBTC", "eBTC", "FBTC"}
USD_SYMBOLS = {"USDC", "USDT", "DAI", "PYUSD", "USDS", "GHO", "USDe", "RLUSD", "crvUSD", "USDA", "eUSD", "AUSD", "USDtb"}
MIN_CUMULATIVE_BORROW_USD = 1_000_000
MIN_DEBT_USD = 1_000_000
BASE_ASSET = {"ETH": WETH, "BTC": WBTC}


def fetch_markets():
    query = """{ markets(first: 1000, where: {chainId_in: [1]}) { items {
        marketId lltv creationBlockNumber oracle { address }
        loanAsset { address symbol decimals } collateralAsset { address symbol decimals } } } }"""
    items = requests.post("https://api.morpho.org/graphql", json={"query": query}, timeout=60).json()["data"]["markets"]["items"]
    rows = []
    for m in items:
        c, l = m["collateralAsset"], m["loanAsset"]
        if not c or not m["oracle"] or l["symbol"] not in USD_SYMBOLS:
            continue
        cls = "ETH" if c["symbol"] in ETH_SYMBOLS else "BTC" if c["symbol"] in BTC_SYMBOLS else None
        if cls:
            rows.append({"id": m["marketId"], "cls": cls, "collateral": c["symbol"], "loan": l["symbol"],
                         "lltv": int(m["lltv"]) / 1e18, "oracle": m["oracle"]["address"].lower(),
                         "loan_decimals": l["decimals"], "collateral_decimals": c["decimals"],
                         "created": m["creationBlockNumber"]})
    pd.DataFrame(rows).to_csv("data/morpho_markets.csv", index=False)
    return pd.DataFrame(rows)


def load_markets():
    return pd.read_csv("data/morpho_markets.csv").set_index("id")


def position_calls(market, user):
    """Calls needed to value one position: base-asset price, market totals, oracle price, position."""
    mid = bytes.fromhex(market.name[2:])
    return [
        (ORACLE, bytes.fromhex("b3596f07") + encode(["address"], [BASE_ASSET[market.cls]])),
        (MORPHO, bytes.fromhex("5c60e39a") + mid),
        (market.oracle, bytes.fromhex("a035b1fe")),
        (MORPHO, bytes.fromhex("93c52062") + mid + encode(["address"], [user])),
    ]


def value_position(market, out):
    """out: results of position_calls. Returns (liquidation price of the base asset, debt in USD)."""
    if not all(ok and len(ret) >= 32 for ok, ret in out):
        return None
    base_price = int.from_bytes(out[0][1], "big") / 1e8
    _, _, total_borrow_assets, total_borrow_shares, _, _ = decode(["uint128"] * 6, out[1][1])
    price = int.from_bytes(out[2][1], "big")
    _, borrow_shares, collateral = decode(["uint256", "uint128", "uint128"], out[3][1])
    if borrow_shares == 0 or collateral == 0 or price == 0:
        return None
    borrowed = borrow_shares * (total_borrow_assets + 1) / (total_borrow_shares + 10**6)
    hf = collateral * price / 1e36 * market.lltv / borrowed
    return [base_price / hf, borrowed / 10**market.loan_decimals]


def live(markets, pos, block, worker=0):
    market_id, user = pos.split(":")
    market = markets.loc[market_id]
    return value_position(market, multicall(position_calls(market, user), block, worker))


def main():
    markets = fetch_markets().set_index("id")
    print(len(markets), "markets", file=sys.stderr)
    head = int(rpc.call("eth_blockNumber", [], prefer=1), 16)
    ids = list(markets.index)

    cumulative = collections.Counter()
    for lo in range(int(markets.created.min()), head + 1, 50_000):
        for l in rpc.get_logs(MORPHO, [BORROW, ids], lo, min(lo + 49_999, head), prefer=1):
            mid = l["topics"][1]
            cumulative[(mid, "0x" + l["topics"][2][26:])] += int(l["data"][2:66], 16) / 10 ** markets.loan_decimals[mid]
    universe = [k for k, v in cumulative.items() if v >= MIN_CUMULATIVE_BORROW_USD]
    print(len(cumulative), "borrow positions,", len(universe), "large", file=sys.stderr)

    blocks = list(pd.read_csv("data/oracle_eth.csv").block)
    with open("data/morpho_liquidations.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["block", "tx", "pos", "cls", "repaid_usd", "liquidator"])
        for lo in range(blocks[0], head + 1, 50_000):
            for l in rpc.get_logs(MORPHO, [LIQUIDATE, ids], lo, min(lo + 49_999, head), prefer=1):
                mid = l["topics"][1]
                repaid = int(l["data"][2:66], 16) / 10 ** markets.loan_decimals[mid]
                w.writerow([int(l["blockNumber"], 16), l["transactionHash"], f"{mid}:0x{l['topics'][3][26:]}",
                            markets.cls[mid], round(repaid, 2), "0x" + l["topics"][2][26:]])

    def snapshot(args):
        block, worker = args
        rows, base = [], {}
        for i in range(0, len(universe), 300):
            chunk = universe[i : i + 300]
            calls = [c for mid, user in chunk for c in position_calls(markets.loc[mid], user)]
            out = multicall(calls, block, worker)
            for n, (mid, user) in enumerate(chunk):
                part = out[4 * n : 4 * n + 4]
                if part[0][0] and len(part[0][1]) >= 32:
                    base[markets.cls[mid]] = int.from_bytes(part[0][1], "big") / 1e8
                v = value_position(markets.loc[mid], part)
                if v and v[1] >= MIN_DEBT_USD:
                    rows.append([block, f"{mid}:{user}", markets.cls[mid], round(v[1]), v[0]])
        return block, base, rows

    oracle = {}
    with open("data/morpho_snapshots.csv", "w", newline="") as f, ThreadPoolExecutor(6) as pool:
        w = csv.writer(f)
        w.writerow(["block", "pos", "cls", "debt_usd", "p_est"])
        for n, (block, base, rows) in enumerate(pool.map(snapshot, [(b, i) for i, b in enumerate(blocks)])):
            oracle[block] = base
            w.writerows(rows)
            if n % 30 == 0:
                print(n, len(blocks), file=sys.stderr, flush=True)
    pd.DataFrame.from_dict(oracle, orient="index").rename_axis("block").sort_index().to_csv("data/oracle_base.csv")


if __name__ == "__main__":
    main()
