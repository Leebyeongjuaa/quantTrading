"""Combine datasets downloaded for different periods into ./data (relative to the working dir).

  mkdir y3 && cd y3 && python3 ../merge_periods.py ../old/data ../data

Directories are given oldest first. Time series are concatenated and de-duplicated; reference
files and caches are taken from the last directory that has them.
"""
import os
import shutil
import sys

import pandas as pd

SERIES = {  # file -> columns that identify a row
    "liquidations.csv": ["tx", "log_index"],
    "prices.csv": ["block"],
    "prices_btc.csv": ["block"],
    "snapshots.csv": ["block", "user"],
    "oracle_eth.csv": ["block"],
    "oracle_base.csv": ["block"],
    "morpho_snapshots.csv": ["block", "pos"],
    "morpho_liquidations.csv": ["block", "tx", "pos"],
    "fluid_snapshots.csv": ["block", "pos"],
    "fluid_liquidations.csv": ["block", "tx", "vault", "debt_usd"],
}
REFERENCE = ["borrowers.csv", "morpho_markets.csv", "live_cache.json", "oracle_blocks.json"]


def main(sources):
    os.makedirs("data", exist_ok=True)
    for name, key in SERIES.items():
        parts = [pd.read_csv(os.path.join(s, name)) for s in sources if os.path.exists(os.path.join(s, name))]
        if not parts:
            continue
        merged = pd.concat(parts).drop_duplicates(key, keep="last").sort_values(key[0], kind="stable")
        merged.to_csv(os.path.join("data", name), index=False)
        print(f"{name}: {len(merged)} rows, blocks {merged.block.min()}..{merged.block.max()}")
    for name in REFERENCE:
        for s in reversed(sources):
            if os.path.exists(os.path.join(s, name)):
                shutil.copy(os.path.join(s, name), os.path.join("data", name))
                break


if __name__ == "__main__":
    main(sys.argv[1:])
