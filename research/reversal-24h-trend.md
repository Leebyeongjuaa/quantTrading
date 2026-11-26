# Alpha note: reversal to the 24h linear trend (`-ts_resid(log(close), 24)`)

Status: research. Last updated 2026-11-26. Successor to `reversal-1h.md`.

## Signal

- Fit an OLS line through the last 24 hourly log closes; the signal is minus the residual of
  the current close from that line (`ts_resid`). Units are log-price deviations, i.e. roughly
  percent. `ts_resid_z` divides by the residual standard deviation.
- Compared with the raw 1h return it trades 2.6x less (turnover 1301% vs 3354%/day) because
  the residual moves slowly; margin 2.58 vs 1.47 bps.

## Results (1h panel, 26 months, delay 0)

| signal | Sharpe 0 bps | turnover/day | margin | Sharpe 2 bps | delay 1 Sharpe |
|---|---|---|---|---|---|
| `-returns` | 5.93 | 3354% | 1.47 | -2.18 | 2.24 |
| `-ts_resid(log(close), 12)` | 3.87 | 1865% | 1.70 | -0.68 | 0.69 |
| **`-ts_resid(log(close), 24)`** | 4.04 | 1301% | 2.58 | +0.91 | 1.11 |
| `-ts_resid(log(close), 48)` | 2.88 | 904% | 2.65 | +0.71 | 0.90 |
| `-ts_resid(log(close), 168)` | 1.09 | 470% | 1.92 | -0.04 | 0.08 |
| `-ts_resid_z(log(close), 24)` | 2.87 | 1461% | 1.40 | -1.25 | 0.47 |
| `rank(-ts_resid(log(close), 24))` | 3.40 | 1239% | 1.84 | -0.29 | 1.05 |

- 24h is the sweet spot; 12h is closer to the raw return (fast, expensive), 48h+ loses the
  signal. Z-scoring and ranking both hurt: the dollar size of the deviation carries information.
- Still latency-sensitive (delay 1 halves it), but less than the raw return (-72% vs -62%).

5m panel, 180 days, hourly rebalance, 5-minute fill: `-ts_resid(log(close), 288)` Sharpe 5.84,
turnover 1320%, margin 3.63 bps, **+2.62 at 2 bps** (raw return: 8.85 / 3402% / 2.13 / +0.53).

## Noise floor on the residual (per name, 1h panel, delay 0)

Median |residual| is 0.69% (p25 0.30%, p75 1.38%).

| θ | δ | Sharpe 0 bps | turnover/day | margin | Sharpe 2 bps |
|---|---|---|---|---|---|
| 0 | 0 | 4.04 | 1301% | 2.58 | 0.91 |
| 0 | 1% | 2.73 | 635% | 3.54 | 1.19 |
| 1% | 1% | 2.41 | 486% | 3.69 | 1.10 |
| 0.5% | 1% | 2.47 | 584% | 3.06 | 0.86 |
| 2% | any | about 1 | 265-350% | 2.3 | about 0 |

The change floor δ = 1% does more than the noise floor here (the residual already suppresses
small moves). Break-even one-way cost: 2.6 bps unfiltered, 3.5-3.7 bps with δ 1%.

## Artifact

Comparison page (1h panel, 26 months, delay 0): https://claude.ai/artifact/Bmhzys8WASsoQPAQtGfPtq
Series: residual 24h, residual 24h + δ 1%, residual 48h, raw 1h return. Built with
`tools/wq-report/build.py` from `wq-bt run --out` directories:

```bash
wq-bt run "-ts_resid(log(close), 24)" --data-dir data/ --delay 0 --decay 0 --out runs/r24
wq-bt run "-ts_resid(log(close), 24)" --data-dir data/ --delay 0 --decay 0 --min-signal-change 0.01 --out runs/r24f
wq-bt run "-ts_resid(log(close), 48)" --data-dir data/ --delay 0 --decay 0 --out runs/r48
wq-bt run "-returns" --data-dir data/ --delay 0 --decay 0 --out runs/ret
python3 tools/wq-report/build.py --title "24h 추세잔차 리버설" --out report/ "잔차 24h=runs/r24" "잔차 24h + δ1%=runs/r24f" "잔차 48h=runs/r48" "1h 수익률 (기존)=runs/ret"
```

## Verdict

Better cost structure than the raw 1h return at the same horizon: break-even 2.6-3.7 bps vs
1.5-2.3. Still a fast signal that needs fills within minutes of the hour and the PnL still
comes from thin alts. Next: per-name cost model, maker fill model, and a combined signal
(`-ts_resid(log(close), 24)` filtered by `-returns` sign).
