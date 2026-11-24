# CLAUDE.md — quantTrading

## Overview

Rust workspace for quantitative crypto trading on the Lighter perp DEX plus two research
backtesters: a pairs/stat-arb engine and a WorldQuant-BRAIN-style cross-sectional alpha engine.
See `README.md` for usage.

## Repository Structure

```
quantTrading/
├── Cargo.toml                       # workspace (resolver 2, shared deps)
├── crates/
│   ├── lighter-engine/              # live trading engine (lib + `lighter-cli` bin)
│   │   ├── src/config.rs            # env-based config (LIGHTER_* vars, mainnet/testnet presets)
│   │   ├── src/types.rs             # MarketInfo, OrderBook, Order, Candle, decimal<->units scaling
│   │   ├── src/signer.rs            # Signer trait + LightersSigner (pure-Rust lighter-go port)
│   │   ├── src/rest.rs              # async REST client (/api/v1/*)
│   │   ├── src/ws.rs                # reconnecting WebSocket, shared book store, typed events
│   │   ├── src/nonce.rs             # per-key nonce manager
│   │   ├── src/gateway.rs           # OrderGateway: round, validate, sign, send (single/batch)
│   │   ├── src/candles.rs           # resample (5m -> 10m) + CSV export
│   │   ├── src/binance.rs           # Binance futures archive backfill + splice (history before Lighter)
│   │   ├── src/exec/iceberg.rs      # IcebergPlanner (pure state machine) + IcebergExecutor
│   │   ├── src/exec/pair.rs         # PairExecutor (batched IOC legs) + PairSizer
│   │   ├── src/bin/lighter-cli.rs   # CLI
│   │   └── examples/sign_vector.rs  # deterministic signed tx for the Go cross-check
│   └── statarb-backtest/            # pairs backtester (lib + `statarb-bt` bin)
│       ├── src/data.rs              # CSV load, resample, align
│       ├── src/stats.rs             # OLS, rolling moments, ADF, half-life
│       ├── src/strategy.rs          # Params, hedge fitting, spread, diagnostics
│       ├── src/engine.rs            # bar loop: next-open fills, fees, slippage, stops
│       ├── src/metrics.rs / report.rs / synthetic.rs
│       └── src/bin/statarb-bt.rs    # run / analyze / sweep / resample / synthetic
│   └── wq-backtest/                 # BRAIN-style alpha backtester (lib + `wq-bt` bin)
│       ├── src/mat.rs               # T×N f64 matrix (NaN = missing)
│       ├── src/panel.rs             # per-instrument CSVs -> aligned field matrices
│       ├── src/expr.rs              # alpha expression parser (calls, kwargs, ternary)
│       ├── src/ops.rs               # operator library: element-wise, cross-sectional, ts_*
│       ├── src/sim.rs               # decay -> universe -> neutralize -> scale -> truncate -> PnL
│       ├── src/metrics.rs           # Sharpe, returns, turnover, fitness, margin, IC, drawdown
│       ├── src/bin/wq-bt.rs         # run / batch / corr / check / ops / info
│       └── alphas.example.txt       # sample alphas for `wq-bt batch`
├── tools/go-signer-check/           # Go program verifying Rust signatures with lighter-go
├── .env.example                     # LIGHTER_ACCOUNT_INDEX / LIGHTER_API_KEY_INDEX / LIGHTER_API_KEY_PRIVATE
└── CLAUDE.md
```

## Development Workflow

- **Branch naming:** Feature branches use the `claude/` prefix when created by AI assistants.
- **Commits:** Use clear, descriptive commit messages.
- **Push:** Always use `git push -u origin <branch-name>`.
- **Checks before committing:** `cargo fmt --all`, `cargo clippy --workspace`, `cargo test --workspace`.

## Conventions

- Keep code simple and avoid over-engineering.
- Do not introduce security vulnerabilities (injection, XSS, etc.).
- Prefer editing existing files over creating new ones.
- Secrets only via environment / `.env` (git-ignored); never as CLI flags or in logs.
- Execution algos are split into a pure planner (unit-tested, no I/O) and an async runner.
- Money values at API boundaries are `rust_decimal::Decimal`; protocol integers only inside
  `gateway`/`signer`.
- Protocol facts (tx type codes, order type / TIF enums, decimals) mirror `lighter-go`; do not
  change constants without checking the reference.
- `wq-backtest` operator names and semantics follow WorldQuant BRAIN; window ops return NaN
  until at least `ceil(d/2)` valid points exist, cross-sectional ops ignore NaN.
- Backtests never look ahead: signals from bar `t` are filled at `t+1` (statarb) or earn
  `r_{t+1}` (wq); keep that invariant when adding features.
- Update this CLAUDE.md whenever significant structure or tooling changes are made.
