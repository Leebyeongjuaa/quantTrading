# quantTrading

Rust workspace with two independent crates:

| crate | binary | purpose |
|---|---|---|
| `crates/lighter-engine` | `lighter-cli` | Live trading engine for the [Lighter](https://lighter.xyz) perp DEX: signer, REST, WebSocket, order gateway, iceberg and pair executors |
| `crates/statarb-backtest` | `statarb-bt` | Event-driven pairs / stat-arb backtester for 10-minute bars (any fixed interval works) |
| `crates/wq-backtest` | `wq-bt` | WorldQuant-BRAIN-style cross-sectional alpha backtester: expression language, decay / neutralization / truncation, Sharpe / turnover / fitness / margin |

`tools/go-signer-check` is a small Go program that cross-checks the Rust signer against Lighter's
official `lighter-go` reference implementation.

## Build & test

```bash
cargo build --release
cargo test
```

## Lighter engine

### Setup

```bash
cp .env.example .env
cargo run -p lighter-engine --bin lighter-cli -- gen-key      # prints a private/public key pair
```

Register the public key on your Lighter account (ChangePubKey, signed by your L1 wallet, via the
Lighter app or the official Python SDK), then put the private key and your account index in `.env`.
Verify with:

```bash
cargo run -p lighter-engine --bin lighter-cli -- check-key
```

### Commands

```bash
lighter-cli markets                                  # symbols, ids, decimals, minimums
lighter-cli book ETH --depth 10                      # live top of book (WebSocket)
lighter-cli candles ETH --resolution 5m --days 30 --resample 10m --out eth_10m.csv
lighter-cli ws-dump ETH --seconds 10                 # raw frames, for debugging
lighter-cli orders ETH                               # open orders (auth token generated locally)
lighter-cli place --symbol ETH --side buy --size 0.01 --price 1500 --tif post-only
lighter-cli place --symbol ETH --side sell --size 0.01 --price 1400 --market   # IOC, price = worst acceptable
lighter-cli cancel --symbol ETH --order-index 281478346434840
lighter-cli cancel-all
lighter-cli iceberg --symbol ETH --side buy --total 5 --display 0.25 --peg-ticks 1 --jitter-pct 20
lighter-cli iceberg --symbol ETH --side sell --total 5 --display 0.5 --price 1800 --refill-at 0.8
lighter-cli pair --long ETH --short BTC --notional 5000 --beta 0.89 --slippage-bps 10
```

Every command accepts `--network testnet` and `--dry-run` (sign but do not send).

### Design

* **Signing** goes through the `Signer` trait. The default backend is the `lighters` crate, a pure
  Rust port of `lighter-go` (Poseidon2 + Schnorr over the Goldilocks field). Its hash and JSON
  layout match the Go reference vector in a unit test, and `tools/go-signer-check` verifies Rust
  signatures with the Go verifier end to end.
* **Prices and sizes** are `rust_decimal::Decimal` at the API boundary and converted to the
  protocol's integer units with each market's `size_decimals` / `price_decimals`. Buys round
  down, sells round up; minimum base and quote amounts are enforced before signing.
* **Nonces** are reserved locally per API key (consecutive values for batches) and resynced from
  `nextNonce` after any send failure.
* **WebSocket** keeps L2 books in a shared store, checks `begin_nonce` continuity and
  resubscribes on gaps, answers pings, reconnects with backoff, and streams own-order updates
  (`account_all_orders`, auth token attached).
* **Iceberg** is emulated client-side (Lighter has no native iceberg): a planner state machine
  rests one visible slice (post-only by default), refills on fill, and with `--peg-ticks` follows
  the best quote and reprices when the market drifts. Slices never cross the spread.
* **Pair** signs both IOC legs with consecutive nonces and submits them in one `sendTxBatch`,
  prices them off the live book with a slippage cap, retries unfilled remainders, and reports the
  net notional left over.

## Stat-arb backtester

Inputs are CSV bars (`ts,open,high,low,close,volume`; `ts` in Unix ms/seconds or ISO-8601), e.g.
from `lighter-cli candles ... --resample 10m`.

```bash
statarb-bt analyze --a eth_10m.csv --b btc_10m.csv            # beta, correlation, ADF t-stat, half-life
statarb-bt run --a eth_10m.csv --b btc_10m.csv \
    --hedge static --train-bars 1008 --lookback 144 \
    --entry-z 2 --exit-z 0.5 --stop-z 4 --max-hold-bars 288 \
    --fee-bps 2 --slippage-bps 1 --notional 10000 --out bt-out
statarb-bt sweep --a eth_10m.csv --b btc_10m.csv --entries 1.5,2,2.5 --exits 0,0.5 --lookbacks 72,144,288
statarb-bt resample --input eth_1m.csv --output eth_10m.csv --bar 10m
statarb-bt synthetic --bars 6000 --out-dir synthetic                 # cointegrated test pair
```

Model: `log A = alpha + beta * log B + spread`. `--hedge static` fits `alpha, beta` once on the
first `train_bars` bars and holds them fixed; `--hedge rolling` refits every bar over `lookback`.
The z-score of the spread over `lookback` drives entries (`|z| >= entry_z`), exits
(`|z| <= exit_z`), stops and max-hold. Signals use bar `t` closes and fill at bar `t+1` open with
slippage; fees are charged per leg per side. Leg A trades `notional`; leg B trades
`notional * beta` (dollar-neutral hedge in log space). Outputs: `equity.csv`, `trades.csv`,
`summary.json` with Sharpe, Sortino, max drawdown, win rate, profit factor, exposure.

## Cross-sectional alpha backtester (WorldQuant BRAIN style)

An alpha is an expression evaluated on a panel of instruments × bars. Each bar the vector of
alpha values becomes dollar-neutral long/short weights and earns the next bar's returns.

```bash
# 1. data: one CSV per instrument in a directory
lighter-cli candles-universe --top 30 --asset-class crypto --resolution 1h --days 90 --out-dir data/
# longer history: Lighter starts 2025-01-17; earlier bars are backfilled from Binance USDT-M
# futures archives (data.binance.vision) and spliced with a median close ratio over the overlap
lighter-cli candles-universe --top 30 --asset-class crypto --resolution 1h --days 800 --fallback binance --out-dir data/

# 2. simulate
wq-bt info --data-dir data/
wq-bt run "rank(-ts_delta(close, 5))" --data-dir data/ --delay 1 --decay 4 --truncation 0.1 --out wq-out
wq-bt run "-ts_corr(rank(close), rank(volume), 24)" --data-dir data/ --bar 4h --universe 10 --cost-bps 3
wq-bt batch crates/wq-backtest/alphas.example.txt --data-dir data/     # many alphas, ranked by fitness
wq-bt corr "rank(-ts_delta(close, 5))" "-ts_rank(returns, 24)" --data-dir data/   # PnL correlation
wq-bt grid "-returns" --data-dir data/ --decays 0,4 --distances 0,0.01,0.02,0.05 --speeds 1,0.25,0.1 --cost-bps 3
wq-bt ops                                                              # fields and operators
```

Two turnover controls sit between target and held weights: `--trade-distance D` re-trades an
instrument only after the sum of |returns| since its last trade reaches `D` (a distance gate),
and `--entry-speed s` moves the held weight only `s` of the way toward the target per trade
(gradual entry), and `--exit-speed r` fades a held weight toward zero by `r` per bar while its
gate stays closed (0 = hold). The gate measures either price travel (`--gate price`: cumulative
|return|, the universe mean per bar under `--distance-scope portfolio`, so it does not change
with the number of instruments) or signal drift (`--gate signal`: pending turnover
`sum|target - held| / booksize`, or `--gate signal-change`: `sum|target_t - target_{t-1}|`
accumulated since the last trade). `--smooth N` holds the trailing N-bar simple moving average
of the scaled target weights (position smoothing, applied before the gate). `--rebalance 1h`
trades only when the signal bar closes on the hour and holds in between, so on a 5m panel
`--delay 1 --rebalance 1h` means "signal at the hour, executed 5 minutes later, held for the
hour". `grid` sweeps
distance × speed × exit with decay.

Simulation settings mirror BRAIN: `--delay` (1 = alpha from the previous close), `--decay`
(linear decay over N bars), `--neutralization market|none`, `--truncation` (max |weight| as a
fraction of the book), `--booksize`, `--universe N` (top-N by trailing dollar volume, re-ranked each bar, or fixed at the start with `--universe-mode static`) and an
optional `--cost-bps`. Reported: annualized Sharpe, annual return, daily turnover,
`fitness = sharpe · sqrt(|return| / max(turnover, 0.125))`, margin (bps per dollar traded),
max drawdown, mean IC / ICIR, long/short counts and a yearly or monthly breakdown. Outputs:
`pnl.csv`, `weights.csv`, `summary.json`.

Operators (BRAIN names): element-wise `abs log sqrt sign signed_power max min if_else ...`;
cross-sectional `rank zscore scale normalize winsorize truncate`; time-series `ts_mean ts_sum
ts_std_dev ts_delta ts_delay ts_returns ts_rank ts_min ts_max ts_arg_max ts_arg_min ts_corr
ts_covariance ts_decay_linear ts_decay_exp_window ts_zscore ts_scale ts_av_diff ts_product
ts_backfill hump`; fields `open high low close volume dollar_volume returns vwap adv(d)`.
Grouping helpers for cross-asset signals: `top_n(x, n)` (1 for the n largest `x` in the
cross-section), `mean_if(x, cond)` (mean of `x` over the names where `cond != 0`, broadcast to
every name) and `ts_beta(y, x, d)` (rolling regression slope), `ts_resid(x, d)` / `ts_resid_z(x, d)` (residual of the last point from the OLS trend over d bars). Example, majors lead minors:
`if_else(top_n(adv(24),5), 0, ts_beta(returns, mean_if(returns, top_n(adv(24),5)), 168) * mean_if(returns, top_n(adv(24),5)))`.
Industry-group operators are not implemented because the universe has no sector data.

## Charting results

`tools/wq-report/build.py` turns one or more `wq-bt run --out` directories into the standard
comparison page (cumulative PnL before and after 3/5 bps costs, gross/net exposure, daily and
hour-of-day turnover, rolling win ratio, per-symbol contribution, tables). The `wq-report` skill in `.claude/skills/` documents the workflow.

## Research notes

Alpha write-ups live in `research/` (one Markdown file per signal, with settings, results and
reproduction commands). Start with `research/reversal-1h.md`, then
`research/reversal-24h-trend.md` (its successor) and `research/major-minor.md`.

## Verifying the signer against Go

```bash
cargo run -p lighter-engine --example sign_vector | (cd tools/go-signer-check && go run .)
```

## Notes and limits

* Lighter serves candles at 1m/5m/15m/30m/1h/4h/12h/1d only; 10m bars are built by resampling 5m.
* `code: 200` from `sendTx` means accepted, not executed. Executors watch the order stream for the
  real outcome and reconcile against `accountActiveOrders` every 15s.
* The exact JSON of the private `account_all_orders` channel is parsed leniently (map or array of
  orders). Check it on your own account with `ws-dump`-style logging before relying on it.
* The `lighters` signer is a community port and is not security audited; the Go cross-check is
  the mitigation. For production, consider FFI to Lighter's official signer shared library.
