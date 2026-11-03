# CLAUDE.md — quantTrading

## Overview

Quantitative trading research repo. First module: **Aero (ex-Aerodrome/Velodrome) sAERO
allocation optimizer** on Base — data collection + walk-forward backtest of allocation
("voting") strategies for the Predictive Allocation system that went live 2026-10-22.

## Repository Structure

```
quantTrading/
├── CLAUDE.md
├── aero_voting/
│   ├── config.py        # Aero Base contract addresses, minimal ABIs, launch timestamp
│   ├── rpc.py           # retrying RPC wrapper + multicall3 batching + chunked eth_getLogs
│   ├── fetch_data.py    # collects gauges / fee releases / incentives / hourly supply / prices → data/
│   └── backtest.py      # hourly panel + walk-forward strategy backtest (47h cooldown)
├── data/                # CSV snapshots produced by fetch_data.py (committed for reproducibility)
└── reports/             # backtest reports (markdown)
```

## How it works (Aero mechanics, verified against Dromos-Labs/metadex-public)

- sAERO holders *allocate* voting power to gauges; allocation is continuous (no weekly epoch).
  A position can re-allocate once every `allocationCooldown` (47h at launch).
- Pool fees flow gauge → per-gauge rewards contract. Every `NotifyFeesAmount` event is a fee
  release credited pro-rata to the gauge's allocation supply at that moment (`supplyAt(ts)`).
- Incentives ("bribes") are streaming programs (`incentive(id)`: token, rate, start, end), also pro-rata.
- Allocator earnings for amount `a` on gauge `g` over a time slice = `(fees + incentives) * a / (supply + a)`.

## Running

```bash
pip install web3 pandas numpy tabulate
python -m aero_voting.fetch_data --stage all     # ~30 min on public RPCs (publicnode: ≤10 addresses & 10k blocks per getLogs)
python -m aero_voting.backtest --amount 100000 --window 48 --topn 5 --report reports/aero_voting_backtest.md
```

## Development Workflow

- **Branch naming:** Feature branches use the `claude/` prefix when created by AI assistants.
- **Commits:** Use clear, descriptive commit messages.
- **Push:** Always use `git push -u origin <branch-name>`.

## Conventions

- Keep code simple and avoid over-engineering.
- Do not introduce security vulnerabilities (injection, XSS, etc.).
- Prefer editing existing files over creating new ones.
- Update this CLAUDE.md whenever significant structure or tooling changes are made.
- Only public RPCs / public APIs (DefiLlama) are used; no keys required.
