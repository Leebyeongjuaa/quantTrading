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
        └── analyze.py              # Event study (run: python3 analyze.py [v3|v4])
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
