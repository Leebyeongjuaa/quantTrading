"""Daily Aave v3 account snapshots for large stablecoin borrowers -> data/snapshots.csv.

Environment (all optional): MIN_CUMULATIVE_BORROW_USD (who is tracked, default $2M),
SNAPSHOT_MIN_DEBT_USD (rows kept, default $1M), BLOCKS="start,end" (explicit grid),
SNAPSHOT_INTERVAL (blocks between snapshots), PAIRS=file.csv (only these block,user pairs).
A cheap way to track many small accounts: weekly snapshots for everyone, then PAIRS for the
days of the weeks in which an account started close to liquidation.

For every candidate in data/borrowers.csv and every 7200th block of the study period,
reads Pool.getUserAccountData at that historical block (through Multicall3) and keeps
rows with at least MIN_DEBT_USD of debt. Also records the Aave oracle ETH price at each
snapshot block in data/oracle_eth.csv. Only data available at that block is used, so the
implied liquidation price (oracle price / health factor) is known ex ante.
"""
import csv
import os
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor

import pandas as pd
import requests
from eth_abi import decode, encode

POOL = "0x87870bca3f3fd6335c3f4ce8392d69350b4fa4e2"
ORACLE = "0x54586be62e3c3580375ae3723c145253060ca0c2"
WETH = "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
MULTICALL3 = "0xca11bde05977b3631167028862be2a173976ca11"
ENDPOINTS = [
    "https://rpc.mevblocker.io",
    "https://gateway.tenderly.co/public/mainnet",
    "https://eth.drpc.org",
]
MIN_CUMULATIVE_BORROW_USD = float(os.environ.get("MIN_CUMULATIVE_BORROW_USD", 2_000_000))  # who is tracked
MIN_DEBT_USD = float(os.environ.get("SNAPSHOT_MIN_DEBT_USD", 1_000_000))  # rows kept
INTERVAL = int(os.environ.get("SNAPSHOT_INTERVAL", 7200))  # blocks; 7200 is about one day
CHUNK = 400
_local = threading.local()


def multicall(calls, block, worker, overrides=None):
    """calls: list of (target, calldata bytes). Returns list of (success, returndata).
    overrides: optional eth_call state override, e.g. {address: {"code": "0x..."}}."""
    data = "0x82ad56cb" + encode(["(address,bool,bytes)[]"], [[(t, True, d) for t, d in calls]]).hex()
    payload = {"jsonrpc": "2.0", "id": 1, "method": "eth_call",
               "params": [{"to": MULTICALL3, "data": data}, hex(block)] + ([overrides] if overrides else [])}
    if not hasattr(_local, "s"):
        _local.s = requests.Session()
    last = None
    for attempt in range(8):
        url = ENDPOINTS[(worker + attempt) % len(ENDPOINTS)]
        try:
            j = _local.s.post(url, json=payload, timeout=120).json()
            if "result" in j:
                return decode(["(bool,bytes)[]"], bytes.fromhex(j["result"][2:]))[0]
            last = j.get("error")
        except Exception as e:
            last = e
        time.sleep(1 + attempt)
    raise RuntimeError(f"multicall failed at {block}: {last}")


def account_calldata(user):
    return bytes.fromhex("bf92857c") + encode(["address"], [user])  # getUserAccountData(address)


def snapshot(args):
    block, users, worker = args
    calls = [(POOL, account_calldata(u)) for u in users]
    calls.append((ORACLE, bytes.fromhex("b3596f07") + encode(["address"], [WETH])))  # getAssetPrice
    out = multicall(calls, block, worker)
    eth_price = int.from_bytes(out[-1][1], "big") / 1e8
    rows = []
    for u, (ok, ret) in zip(users, out[:-1]):
        if not ok or len(ret) < 192:
            continue
        coll, debt, _, liq_threshold, _, hf = decode(["uint256"] * 6, ret)
        if debt / 1e8 >= MIN_DEBT_USD:
            rows.append([block, u, round(coll / 1e8), round(debt / 1e8), liq_threshold, hf / 1e18])
    return block, eth_price, rows


def main():
    b = pd.read_csv("data/borrowers.csv")
    users = list(b[b.cumulative_usd >= MIN_CUMULATIVE_BORROW_USD].user)
    liq = pd.read_csv("data/liquidations.csv", usecols=["block"])
    blocks = range(int(liq.block.min()) - INTERVAL, int(liq.block.max()), INTERVAL)
    if os.environ.get("BLOCKS"):  # "start,end": explicit snapshot grid, e.g. to extend an existing one backwards
        lo, hi = map(int, os.environ["BLOCKS"].split(","))
        blocks = range(lo, hi, INTERVAL)
    tasks = []
    if os.environ.get("PAIRS"):  # csv with columns block,user: snapshot only these account-blocks
        for blk, grp in pd.read_csv(os.environ["PAIRS"]).groupby("block"):
            who = list(grp.user)
            for i in range(0, len(who), CHUNK):
                tasks.append((int(blk), who[i : i + CHUNK], len(tasks)))
    else:
        for blk in blocks:
            for i in range(0, len(users), CHUNK):
                tasks.append((blk, users[i : i + CHUNK], len(tasks)))
    print(f"{len(users)} users, {len(tasks)} multicalls", file=sys.stderr)
    oracle = {}
    with open("data/snapshots.csv", "w", newline="") as f, ThreadPoolExecutor(6) as pool:
        w = csv.writer(f)
        w.writerow(["block", "user", "collateral_usd", "debt_usd", "liq_threshold_bps", "hf"])
        for n, (blk, eth_price, rows) in enumerate(pool.map(snapshot, tasks)):
            oracle[blk] = eth_price
            w.writerows(rows)
            if n % 200 == 0:
                print(n, len(tasks), file=sys.stderr, flush=True)
    pd.Series(oracle, name="eth_usd").rename_axis("block").sort_index().to_csv("data/oracle_eth.csv")


if __name__ == "__main__":
    main()
