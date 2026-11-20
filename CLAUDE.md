# CLAUDE.md — quantTrading

## Overview

Rust workspace for quantitative crypto trading on the Lighter perp DEX plus a stat-arb research
backtester. See `README.md` for usage.

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
- Update this CLAUDE.md whenever significant structure or tooling changes are made.
