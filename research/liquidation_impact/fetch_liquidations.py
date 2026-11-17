"""Download Aave v3 (Ethereum) LiquidationCall events into data/liquidations.csv.

  python3 fetch_liquidations.py [lookback_blocks] [end_block]

Every script writes to ./data relative to the working directory, so a second period can be
built in its own folder (e.g. `mkdir old && cd old && python3 ../fetch_liquidations.py ...`).
"""
import csv
import sys

import rpc

AAVE_V3_POOL = "0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2"
LIQUIDATION_CALL = "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286"


def main(lookback_blocks, end_block=None):
    """Events in the `lookback_blocks` before `end_block` (default: the chain head)."""
    head = end_block or int(rpc.call("eth_blockNumber", []), 16)
    start = head - lookback_blocks
    rows = []
    for lo in range(start, head + 1, 50_000):
        logs = rpc.get_logs(AAVE_V3_POOL, [LIQUIDATION_CALL], lo, min(lo + 49_999, head))
        for l in logs:
            d = l["data"][2:]
            rows.append(
                {
                    "block": int(l["blockNumber"], 16),
                    "tx_index": int(l["transactionIndex"], 16),
                    "log_index": int(l["logIndex"], 16),
                    "tx": l["transactionHash"],
                    "collateral": "0x" + l["topics"][1][26:],
                    "debt": "0x" + l["topics"][2][26:],
                    "user": "0x" + l["topics"][3][26:],
                    "debt_to_cover": int(d[0:64], 16),
                    "collateral_amount": int(d[64:128], 16),
                    "liquidator": "0x" + d[128 + 24 : 192],
                    "receive_atoken": int(d[192:256], 16),
                }
            )
        print(lo, len(rows), file=sys.stderr)
    with open("data/liquidations.csv", "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=list(rows[0]))
        w.writeheader()
        w.writerows(rows)
    print(f"{len(rows)} liquidations, blocks {start}..{head}")


if __name__ == "__main__":
    main(int(sys.argv[1]) if len(sys.argv) > 1 else 2_600_000, int(sys.argv[2]) if len(sys.argv) > 2 else None)
