# CLAUDE.md — quantTrading

## Overview

This is the `quantTrading` repository — a quantitative trading project. The codebase is currently in its initial setup phase.

## Repository Structure

```
quantTrading/
├── CLAUDE.md          # This file — AI assistant guide
└── research/
    └── liquidation_impact/   # Event study: DEX price around Aave v3 liquidations
        ├── rpc.py                  # JSON-RPC helper (public endpoints, no API key)
        ├── fetch_liquidations.py   # Aave v3 LiquidationCall events -> data/liquidations.csv
        ├── fetch_swaps.py          # Uniswap ETH/USDC swap ticks -> data/swaps.csv
        ├── analyze.py              # Event study (run: python3 analyze.py [v3|v4])
        ├── fetch_prices.py         # Per-block ETH (and `btc`) price from Uniswap pools -> data/prices*.csv
        ├── fetch_borrowers.py      # Large stablecoin borrowers on Aave v3 -> data/borrowers.csv
        ├── snapshots.py            # Aave: daily historical health factors -> data/snapshots.csv
        ├── morpho.py               # Morpho Blue: markets, liquidations, daily positions
        ├── fluid.py                # Fluid vaults: liquidations, daily positions
        ├── liquidator_flows.py     # Do liquidators sell seized collateral on a DEX in the same tx?
        ├── competition.py          # Liquidating ourselves: bonus vs gas/builder tips, delay, who wins, by size
        ├── placebo.py              # Same backtest on fake levels: is the edge liquidation-specific?
        ├── merge_periods.py        # Combine datasets downloaded for different periods
        └── backtest.py             # Short ETH/BTC just above large liquidation prices
                                    # (run: python3 backtest.py [aave] [morpho] [fluid];
                                    #  --grid sweeps entry x stop, --exits compares exit rules;
                                    #  env: ENTRY_BUFFER, STOP, MAX_HOLD, MIN_DEBT_USD, MAX_ENTRY_SLIP,
                                    #  TAKE_PROFIT, DEFEND_EXIT; needs eth-abi)

All scripts read and write `./data` relative to the working directory (git-ignored), so a
second period can be built in its own folder and merged with `merge_periods.py`.
```

> **Note:** This file should be updated as modules, services, and tooling are added.

## Development Workflow

- **Branch naming:** Feature branches use the `claude/` prefix when created by AI assistants.
- **Commits:** Use clear, descriptive commit messages.
- **Push:** Always use `git push -u origin <branch-name>`.

## Conventions

- Keep code simple and avoid over-engineering.
- Do not introduce security vulnerabilities (injection, XSS, etc.).
- Prefer editing existing files over creating new ones.
- Update this CLAUDE.md whenever significant structure or tooling changes are made.
