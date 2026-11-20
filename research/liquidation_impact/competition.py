"""How contested are Aave v3 liquidations, by size? -> data/competition.csv (one row per tx)

For every liquidation transaction in data/liquidations.csv:
  * debt repaid and collateral seized in USD at the Aave oracle price of that block, and the
    liquidator's bonus net of the protocol's cut,
  * gas paid, the priority-fee part of it and (for a sample, from traces) direct payments to
    the block builder: what the liquidator gave up to win the block,
  * how long the account had been liquidatable: the health factor at the end of the blocks
    before the liquidation. Not liquidatable one block earlier = taken in the first block.

  python3 competition.py            fetch and print
  python3 competition.py --report   re-print from the saved file
Environment: TRACE_PER_BUCKET (transactions traced per size bucket, default 300).
"""
import os
import sys

import numpy as np
import pandas as pd
import requests
from eth_abi import decode, encode

from liquidator_flows import receipts
from snapshots import ORACLE, POOL, WETH

URLS = ["https://rpc.mevblocker.io", "https://gateway.tenderly.co/public/mainnet", "https://eth.drpc.org"]
TRACE_URL = "https://eth.drpc.org"
LADDER = [1, 2, 3, 5, 10, 25, 100, 300, 1800, 7200]  # blocks before the liquidation
FOLLOW_UP = 50  # same account liquidated this recently: the delay is not a fresh opportunity
TRACE_PER_BUCKET = int(os.environ.get("TRACE_PER_BUCKET", 300))
EDGES = [0, 1e2, 1e3, 1e4, 1e5, 1e6, 1e12]
LABELS = ["<$100", "$100-1k", "$1k-10k", "$10k-100k", "$100k-1M", ">=$1M"]


def batch(calls, size=100, label=""):
    """calls: list of (method, params). Returns the results in order (None where a call reverted)."""
    out = [None] * len(calls)
    for i in range(0, len(calls), size):
        chunk = calls[i : i + size]
        payload = [{"jsonrpc": "2.0", "id": n, "method": m, "params": p} for n, (m, p) in enumerate(chunk)]
        for attempt in range(9):
            try:
                res = requests.post(URLS[attempt % len(URLS)], json=payload, timeout=120).json()
                got = {r["id"]: r for r in res if isinstance(r, dict)}
                # a revert is a real answer; anything else missing means the provider failed
                bad = [n for n in range(len(chunk)) if n not in got
                       or ("result" not in got[n] and got[n].get("error", {}).get("code") != 3)]
                if not bad:
                    break
            except Exception:
                pass
        else:
            raise RuntimeError(f"batch failed: {label}")
        for n in range(len(chunk)):
            out[i + n] = got[n].get("result")
        if label and i % 2000 == 0:
            print(label, i, len(calls), file=sys.stderr, flush=True)
    return out


def eth_call(to, data, block="latest"):
    return ("eth_call", [{"to": to, "data": data}, block if block == "latest" else hex(block)])


def selector(signature):
    return batch([("web3_sha3", ["0x" + signature.encode().hex()])])[0][:10]


def word(address):
    return encode(["address"], [address]).hex()


def health_before(liq):
    """Smallest step of LADDER at which the account was not yet liquidatable (0 = always was)."""
    sel = selector("getUserAccountData(address)")
    steps = pd.Series(0, index=liq.index)
    todo = liq.index
    for step in LADDER:
        rows = liq.loc[todo]
        res = batch([eth_call(POOL, sel + word(u), b - step) for u, b in zip(rows.user, rows.block)], label=f"hf -{step}")
        hf = np.array([decode(["uint256"] * 6, bytes.fromhex(r[2:]))[5] / 1e18 if r and len(r) > 2 else np.inf for r in res])
        steps[todo[hf >= 1]] = step
        todo = todo[hf < 1]
        if len(todo) == 0:
            break
    return steps


def fetch():
    liq = pd.read_csv("data/liquidations.csv")
    for c in ("debt_to_cover", "collateral_amount"):
        liq[c] = liq[c].astype(float)

    assets = sorted(set(liq.collateral) | set(liq.debt))
    decimals = {a: int(r, 16) for a, r in zip(assets, batch([eth_call(a, "0x313ce567") for a in assets]))}

    pairs = sorted(set(zip(liq.collateral, liq.block)) | set(zip(liq.debt, liq.block)) | {(WETH, b) for b in liq.block})
    res = batch([eth_call(ORACLE, "0xb3596f07" + word(a), b) for a, b in pairs], label="prices")  # getAssetPrice
    price = {k: int(r, 16) / 1e8 for k, r in zip(pairs, res)}
    liq["debt_usd"] = [d / 10 ** decimals[a] * price[(a, b)] for a, b, d in zip(liq.debt, liq.block, liq.debt_to_cover)]
    liq["collateral_usd"] = [c / 10 ** decimals[a] * price[(a, b)] for a, b, c in zip(liq.collateral, liq.block, liq.collateral_amount)]
    # the protocol keeps part of the bonus: bits 152-167 of the collateral's reserve configuration
    config = selector("getConfiguration(address)")
    seized = sorted(set(zip(liq.collateral, liq.block)))
    res = batch([eth_call(POOL, config + word(a), b) for a, b in seized], label="protocol fee")
    fee = {k: ((int(r, 16) >> 152) & 0xFFFF) / 1e4 for k, r in zip(seized, res)}
    liq["bonus_usd"] = (liq.collateral_usd - liq.debt_usd) * (1 - pd.Series([fee[k] for k in zip(liq.collateral, liq.block)]))

    liq = liq.sort_values(["block", "tx_index", "log_index"]).reset_index(drop=True)
    previous = liq.groupby("user").block.shift()
    liq["follow_up"] = ((liq.block - previous) <= FOLLOW_UP) & ((liq.block - previous) > 0)
    liq["step"] = health_before(liq)

    blocks = sorted(set(liq.block))
    head = dict(zip(blocks, batch([("eth_getBlockByNumber", [hex(b), False]) for b in blocks], label="blocks")))
    rows = []
    groups = {tx: g for tx, g in liq.groupby("tx")}
    for rc in receipts(list(groups)):
        g = groups[rc["transactionHash"]]
        top = g.loc[g.debt_usd.idxmax()]
        block = int(top.block)
        eth = price[(WETH, block)]
        gas, paid = int(rc["gasUsed"], 16), int(rc["effectiveGasPrice"], 16)
        base = int(head[block]["baseFeePerGas"], 16)
        rows.append({
            "tx": rc["transactionHash"], "block": block, "tx_index": int(rc["transactionIndex"], 16),
            "sender": rc["from"], "liquidator": top.liquidator, "builder": head[block]["miner"],
            "liquidations": len(g), "debt_usd": g.debt_usd.sum(), "bonus_usd": g.bonus_usd.sum(),
            "gas_usd": gas * paid / 1e18 * eth, "priority_usd": gas * (paid - base) / 1e18 * eth,
            "gas_used": gas, "base_fee_gwei": base / 1e9, "eth_usd": eth,
            "step": int(top.step), "follow_up": bool(top.follow_up),
        })
    tx = pd.DataFrame(rows)
    tx["bucket"] = pd.cut(tx.debt_usd, EDGES, labels=LABELS, right=False)

    # direct payments to the builder, from traces, for a sample of each size bucket
    sample = tx.groupby("bucket", observed=True, group_keys=False).apply(
        lambda d: d.sample(min(len(d), TRACE_PER_BUCKET), random_state=0))
    old = pd.read_csv("data/competition.csv") if os.path.exists("data/competition.csv") else pd.DataFrame(columns=["tx", "coinbase_usd"])
    tx["coinbase_usd"] = tx.tx.map(old.dropna(subset=["coinbase_usd"]).set_index("tx").coinbase_usd)  # traced earlier
    for n, r in enumerate(sample[sample.tx.map(tx.set_index("tx").coinbase_usd).isna()].itertuples()):
        for attempt in range(6):
            try:
                out = requests.post(TRACE_URL, timeout=120, json={
                    "jsonrpc": "2.0", "id": 1, "method": "trace_transaction", "params": [r.tx]}).json()["result"]
                break
            except Exception:
                out = None
        if out is None:
            continue
        wei = sum(int(t["action"].get("value", "0x0"), 16) for t in out
                  if t["type"] == "call" and t["action"].get("to") == r.builder and not t.get("error"))
        tx.loc[r.Index, "coinbase_usd"] = wei / 1e18 * r.eth_usd
        if n % 200 == 0:
            print("traces", n, len(sample), file=sys.stderr, flush=True)
            tx.to_csv("data/competition.csv", index=False)
    tx.to_csv("data/competition.csv", index=False)


def report():
    tx = pd.read_csv("data/competition.csv")
    odd = tx.bonus_usd > 0.2 * tx.debt_usd + 1  # collateral worth far more than the debt: not a normal liquidation
    print(f"dropped {odd.sum()} abnormal seizures (bonus ${tx[odd].bonus_usd.sum() / 1e6:.0f}M on ${tx[odd].debt_usd.sum() / 1e3:.0f}k of debt)")
    tx = tx[~odd].copy()
    tx["bucket"] = pd.Categorical(tx.bucket, LABELS, ordered=True)
    tx["builder_usd"] = tx.priority_usd + tx.coinbase_usd  # NaN where the transaction was not traced
    tx["kept_usd"] = tx.bonus_usd - tx.gas_usd - tx.coinbase_usd
    tx["timing"] = tx.step.map({1: "same block", 2: "next block"}).fillna("later")
    days = (tx.block.max() - tx.block.min()) * 12 / 86400
    print(f"{len(tx)} liquidation transactions over {days:.0f} days, ${tx.debt_usd.sum() / 1e6:.0f}M repaid")

    rows = []
    for b, d in tx.groupby("bucket", observed=True):
        t = d[d.coinbase_usd.notna()]
        scale = len(d) / len(t) * 365 / days
        rows.append({
            "debt repaid": b, "txs": len(d), "per day": len(d) / days, "bonus $k": d.bonus_usd.sum() / 1e3,
            "median bonus $": d.bonus_usd.median(), "median gas $": d.gas_usd.median(),
            "traced": len(t), "to builder % of bonus": 100 * t.builder_usd.sum() / t.bonus_usd.sum(),
            "kept $k/yr": t.kept_usd.sum() * scale / 1e3, "median kept $": t.kept_usd.median(),
            "kept >half %": 100 * (t.kept_usd > 0.5 * t.bonus_usd).mean(), "lost money %": 100 * (t.kept_usd < 0).mean(),
        })
    print("\n== economics by size (bonus net of the protocol's cut at oracle prices, before swap costs) ==")
    print(pd.DataFrame(rows).round(1).to_string(index=False))

    fresh = tx[~tx.follow_up]
    rows = []
    for b, d in fresh.groupby("bucket", observed=True):
        row = {"debt repaid": b, "txs": len(d), "same block %": 100 * (d.step == 1).mean(),
               "next block %": 100 * (d.step == 2).mean(), "1 block late %": 100 * (d.step == 3).mean(),
               "2-24 late %": 100 * d.step.isin([5, 10, 25]).mean(), "longer %": 100 * (~d.step.isin([1, 2, 3, 5, 10, 25])).mean()}
        for timing, t in d[d.coinbase_usd.notna()].groupby("timing"):
            row[f"builder % ({timing})"] = 100 * t.builder_usd.sum() / t.bonus_usd.sum()
        rows.append(row)
    print("\n== how long the account had been liquidatable (first liquidation of an episode) ==")
    print(pd.DataFrame(rows).round(1).to_string(index=False))

    rows = []
    for b, d in tx.groupby("bucket", observed=True):
        count = d.liquidator.value_counts()
        kept = d[d.coinbase_usd.notna()].groupby("liquidator").kept_usd.sum().clip(lower=0).sort_values(ascending=False)
        rows.append({"debt repaid": b, "liquidators": len(count), "with >=10 txs": int((count >= 10).sum()),
                     "top 3 % of txs": 100 * count.iloc[:3].sum() / len(d),
                     "top 3 % of kept $": 100 * kept.iloc[:3].sum() / kept.sum(),
                     "top 10 % of kept $": 100 * kept.iloc[:10].sum() / kept.sum()})
    print("\n== who wins ==")
    print(pd.DataFrame(rows).round(1).to_string(index=False))


if __name__ == "__main__":
    if "--report" not in sys.argv:
        fetch()
    report()
