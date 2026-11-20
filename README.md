# quantTrading

Rust workspace with two independent crates:

| crate | binary | purpose |
|---|---|---|
| `crates/lighter-engine` | `lighter-cli` | Live trading engine for the [Lighter](https://lighter.xyz) perp DEX: signer, REST, WebSocket, order gateway, iceberg and pair executors |
| `crates/statarb-backtest` | `statarb-bt` | Event-driven pairs / stat-arb backtester for 10-minute bars (any fixed interval works) |

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
