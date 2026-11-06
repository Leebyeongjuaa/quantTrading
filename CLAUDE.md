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
- A gauge's pending fees are released by whoever touches its rewards contract (an `allocate`, a claim, or
  `advanceGlobalPoints`), so capital newly moved into a gauge only earns from the next release. The backtest
  models this as a 1h entry lag.
- Base block time is **not constant**: 2s until ~2026-11-05 18:00 UTC, 0.2s after. Never convert blocks to
  time with a fixed block time; `fetch_data.Fetcher.block_to_ts` interpolates sampled real block timestamps.

## Running

```bash
pip install web3 pandas numpy tabulate
pip install scipy
python -m aero_voting.fetch_data --stage all                # full fetch, ~30 min on public RPCs (publicnode: ≤10 addresses & 10k blocks per getLogs)
python -m aero_voting.fetch_data --stage all --incremental  # refresh: fee releases only after the last stored block
python -m aero_voting.fetch_data --stage repair-ts          # recompute fee-event timestamps/supply from real block times
python -m aero_voting.backtest --amount 100000 --window 168 --topn 5 --report reports/aero_voting_backtest.md
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
