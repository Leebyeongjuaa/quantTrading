"""How do liquidators dispose of seized collateral? -> data/liquidator_flows.csv

For every Aave v3 liquidation transaction, reads the receipt and records
  * whether the same transaction contains a DEX swap (known AMM swap events),
  * whether the liquidator took a flash loan,
  * how much of the seized collateral token left the liquidator inside the transaction.
A liquidation with no swap and no collateral outflow was paid from inventory: the bot kept
the collateral and whatever hedge it did (CEX or later on-chain) is not visible here.
"""
import csv
import sys

import pandas as pd
import requests

URL = "https://rpc.mevblocker.io"
TRANSFER = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
SWAP_TOPICS = {
    "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67": "univ3",
    "0x40e9cecb9f5f1f1c5b9c97dec2917b7ee92e57ba5563708daca94dd84ad7112f": "univ4",
    "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822": "univ2",
    "0x19b47279256b2a23a1665c810c8d55a1758940ee09377d4f8d26497a3577dc83": "pancake3",
    "0x2170c741c41531aec20e7c107c24eecfdd15e69c9bb0a8dd37b1840b9e0b207b": "balancer2",
    "0x8b3e96f2b889fa771c53c981b40daf005f63f637f1869f707052d15a3dd97140": "curve",
    "0xb2e76ae99761dc136e598d4a629bb347eccb9532a5f8bbd72e18467c3c34cc98": "curve",
}
FLASH_TOPICS = {
    "0xefefaba5e921573100900a3ad9cf29f222d995fb3b6045797eaea7521bd8d6f0",  # Aave FlashLoan
    "0x0d7d75e01ab95780d3cd1c8ec0dd6c2ce19e3a20427eec8bf53283b6fb8e95f0",  # Balancer v2 FlashLoan
    "0xc76f1b4fe4396ac07a9fa55a415d4ca430e72651d37d3401f3bed7cb13fc4f12",  # Morpho FlashLoan
    "0xbdbdb71d7860376ba52b25a5028beea23581364a40522f6bcfb86bb1f2dca633",  # Uniswap v3 Flash
}


def receipts(hashes):
    for i in range(0, len(hashes), 50):
        chunk = hashes[i : i + 50]
        for attempt in range(6):
            try:
                out = requests.post(URL, timeout=120, json=[
                    {"jsonrpc": "2.0", "id": n, "method": "eth_getTransactionReceipt", "params": [h]}
                    for n, h in enumerate(chunk)]).json()
                by_id = {r["id"]: r.get("result") for r in out}
                if all(by_id.get(n) for n in range(len(chunk))):
                    break
            except Exception:
                pass
        else:
            raise RuntimeError("receipt batch failed")
        for n in range(len(chunk)):
            yield by_id[n]
        if i % 1000 == 0:
            print(i, len(hashes), file=sys.stderr, flush=True)


def main():
    liq = pd.read_csv("data/liquidations.csv")
    groups = {tx: g for tx, g in liq.groupby("tx")}
    with open("data/liquidator_flows.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["tx", "block", "liquidator", "collateral", "debt", "debt_to_cover", "collateral_amount",
                    "venues", "flash_loan", "collateral_out_share"])
        for rc in receipts(list(groups)):
            logs = rc["logs"]
            venues = sorted({SWAP_TOPICS[l["topics"][0]] for l in logs if l["topics"] and l["topics"][0] in SWAP_TOPICS})
            flash = any(l["topics"] and l["topics"][0] in FLASH_TOPICS for l in logs)
            for r in groups[rc["transactionHash"]].itertuples():
                pad = "0x" + r.liquidator[2:].rjust(64, "0")
                moved_in = moved_out = 0
                for l in logs:
                    if l["address"] == r.collateral and len(l["topics"]) == 3 and l["topics"][0] == TRANSFER:
                        amount = int(l["data"], 16) if l["data"] != "0x" else 0
                        moved_in += amount if l["topics"][2] == pad else 0
                        moved_out += amount if l["topics"][1] == pad else 0
                share = min(1.0, moved_out / moved_in) if moved_in else float("nan")
                w.writerow([r.tx, r.block, r.liquidator, r.collateral, r.debt, r.debt_to_cover, r.collateral_amount,
                            "+".join(venues), int(flash), round(share, 4)])


if __name__ == "__main__":
    main()
