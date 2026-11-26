# Alpha note: 1h cross-sectional reversal (`-returns`)

Status: research, not tradeable as a taker. Last updated 2026-11-25.

## Signal

- Expression: `-returns` on 1h bars. `returns = close_t / close_{t-1} - 1` per instrument, so the
  signal is "short the names that rose over the last hour, long the ones that fell".
- No decay (`--decay 0`): the raw previous-bar signal is what we want. Linear decay 4 was tested
  early and dropped; it halves Sharpe at delay 0.
- Positions: market neutral (cross-sectional demean), scaled to the book, truncation 0.1.
- Timing: `--delay 0` = signal from bar t's close, filled at bar t's close, earns `r_{t+1}`.
  Not look-ahead, but it assumes a fill at the close price the moment the bar closes.
  `--delay 1` = one full hour late.

## Data

- Universe: Lighter crypto perps, top 30 by daily quote volume as of 2026-11-24
  (`lighter-cli candles-universe --top 30 --asset-class crypto`). This is a today's-top-30
  universe, so the 2024-2025 part carries survivorship bias.
- Period: 2024-09-15 .. 2026-11-24, 1h bars (19,196 bars). Lighter history starts 2025-01-17;
  earlier bars come from Binance USDT-M futures archives spliced by the median close ratio.
- Mean |1h return| across the universe: median 0.54% per bar.

## Headline numbers (cost 0 unless stated)

| setting | Sharpe | annual return | turnover/day | margin | max DD |
|---|---|---|---|---|---|
| delay 0, no gate | 5.93 | +180% | 3354% | 1.47 bps | 19% |
| delay 1, no gate | 2.24 | +66% | 3354% | 0.54 bps | 24% |
| delay 0, 3 bps cost | -6.30 | -188% | | | |
| delay 0, mean IC / ICIR | 0.044 / 14.8 | | | | |

The delay-0 vs delay-1 gap says the signal decays within the hour. Execution latency is the
main unknown; see "Open items".

## What was tried for turnover

All with delay 0, decay 0, full-trade-or-nothing (`--entry-speed 1 --exit-speed 0`):

| gate | best threshold | Sharpe | turnover/day | margin | 3 bps Sharpe |
|---|---|---|---|---|---|
| none | | 5.93 | 3354% | 1.47 | -6.30 |
| price, universe-mean sum\|ret\| (`--gate price --distance-scope portfolio`) | 1% | 4.20 | 1519% | 2.33 | -1.22 |
| same | 5% | 1.75 | 407% | 3.57 | +0.28 |
| signal gap target vs held, per name (`--gate signal --distance-scope instrument`) | 2 | 5.36 | 1831% | 2.40 | -1.36 |
| signal change since last trade, per name (`--gate signal-change --distance-scope instrument`) | 5 | 3.42 | 1022% | 3.10 | +0.11 |
| same | 12 | 1.68 | 463% | 3.27 | +0.14 |

- Every gate raises margin (to 2-3.6 bps) at the cost of Sharpe; nothing clears 3 bps with room.
  Break-even one-way cost is about 2-3.5 bps depending on the gate.
- Price distance and signal distance behave alike here because for `-returns` the price move
  is the signal. Signal-change (accumulated) degrades a little more gracefully.
- Gradual entry (`--entry-speed 0.1`) looked great on a 90-day window and failed on 26 months:
  it was a window artefact. Exit fade (`--exit-speed`) never helped except as "exit at once"
  (0.5), which trades Sharpe for low exposure.
- Per-name signal gap D=3 only opens when a name flips from the +10% cap to the -10% cap and
  never opened before 2025-10 (fewer than 30 names); ignore that cell.
- Position smoothing (`--smooth 8`, delay 0): Sharpe 3.30, turnover 413%, margin 2.28, avg gross 32%.
  Decent Sharpe but exposure is not rescaled.

## Universe size (delay 0, no gate)

| N | dynamic (re-rank each bar) | static (fixed at start) |
|---|---|---|
| 5 | 2.28 | 2.45 |
| 10 | 2.86 | 2.82 |
| 15 | 3.65 | 4.10 |
| 20 | 4.47 | 4.61 |
| 30 | 5.86 | 5.88 |

Sharpe rises with N (diversification of a cross-sectional edge; IC is flat at 0.044, ICIR rises
5.6 -> 14.8). Small static universes collapse at delay 1 (5-10 names: Sharpe <= 0). Go wide and
small per name; test N=50 next, with per-name costs.

## Other observations

- Bar-level win ratio is 51% for every variant; the edge is breadth and repetition, not hit rate.
- Net exposure is not zero: truncation after neutralization and names leaving the universe
  leave +0.6..3.9% average and +-10% on a day. Re-neutralize after truncation in production.
- Monthly PnL is uneven; 2026-01 and 2026-02 are the worst months for most settings.

## Latency test on 5m bars (added 2026-11-25, corrected 2026-11-26)

Panel: same 30 names, 5m bars, 2026-05-29 .. 2026-11-25 (51,840 bars). Signal
`-ts_returns(close, 12)` (trailing 1h return) computed at the hour close, executed `delay`
5m bars later and held until the next hour (`--rebalance 1h`), so turnover matches the 1h panel.

| execution delay | Sharpe | annual return | turnover/day | margin | mean IC | 3 bps Sharpe |
|---|---|---|---|---|---|---|
| 0 min | 10.4 | +314% | 3402% | 2.53 bps | 0.051 | -1.9 |
| 5 min | 8.9 | +264% | 3402% | 2.13 bps | 0.022 | -3.6 |
| 15 min | 4.7 | +138% | 3402% | 1.11 bps | 0.012 | -7.9 |
| 60 min | 1.4 | +39% | 3402% | 0.32 bps | 0.006 | |

- Re-trading every 5m (the first version of this test) gave Sharpe 40 / 17 / 9 / 4 with
  turnover 12,936%/day; that was a different strategy, not the intended "hourly signal,
  late fill". Numbers above supersede it.
- The 180-day window is a strong one for the alpha (the 1h panel's 2026 Sharpe is 8.4 vs 5.9
  over 26 months), so compare rows, not levels.
- Delay 0 on 5m closes is inflated by bid-ask bounce. Realistic expectation sits between the
  5 and 15 minute rows: margin 1-2 bps, which no taker fee schedule clears.
- Per-symbol contribution: almost all PnL comes from thin alts (XPL, ZRO, MON, VVV, 1000PEPE);
  BTC/ETH/SOL contribute about zero. Those names also have the widest spreads.
- Conclusion unchanged: a liquidity-provision signal; harvestable only by resting orders.

## Open items

1. Maker execution model (resting limit orders, fill probability), which is the only path left.
2. Wider universe (50) with liquidity-dependent costs.
3. Re-neutralization step after truncation / gating.

## Reproduce

```bash
lighter-cli candles-universe --top 30 --asset-class crypto --resolution 1h --days 800 --fallback binance --out-dir data/
wq-bt run "-returns" --data-dir data/ --delay 0 --decay 0
wq-bt grid "-returns" --data-dir data/ --delay 0 --decays 0 --gate price --distance-scope portfolio --distances 0.01,0.02,0.03,0.05 --speeds 1 --exits 0 --cost-bps 3
wq-bt grid "-returns" --data-dir data/ --delay 0 --decays 0 --gate signal-change --distance-scope instrument --distances 2,5,8,12 --speeds 1 --exits 0 --cost-bps 3
wq-bt run "-returns" --data-dir data/ --delay 0 --decay 0 --universe 15 --universe-window 24 --universe-mode static
```
