# Alpha note: counter-trend shock inside a trend (buy the dip / sell the rip)

Status: research, promising but sparse. Last updated 2026-11-27.

## Signal

Trend = OLS slope of the last 48 hourly log closes with R² ≥ 0.3. Shock = the latest close sits
≥ z residual standard deviations from the line **against** the trend direction. While both
hold, take a position in the trend direction (continuation); otherwise the name is out of the
book (`keep_if` → NaN → weight 0).

```
keep_if(ts_slope(log(close), 48),
        ts_r2(log(close), 48) >= 0.3 && abs(ts_resid_z(log(close), 48)) >= 2
        && ts_resid_z(log(close), 48) * ts_slope(log(close), 48) < 0)
```

Variants: weight by the residual (`-ts_resid(...)`) instead of the slope; 24h window; z 0.5-4;
directional (no neutralization) or market neutral.

## Results (1h panel, 26 months, delay 0)

| book | window | z | weight | Sharpe 0 bps | annual ret | turnover/day | margin | max DD | 2 bps Sharpe | avg names L/S | yearly Sharpe |
|---|---|---|---|---|---|---|---|---|---|---|---|
| directional | 24h | 2 | slope | 3.03 | +184% | 1244% | 4.05 | 41% | 1.53 | 0.4 / 0.4 | 4.0 / 2.6 / 3.3 |
| directional | 48h | 2 | slope | 1.37 | +95% | 1197% | 2.17 | 62% | 0.11 | | |
| directional | 48h | 3 | slope | 0.93 | +38% | 263% | 3.94 | 44% | 0.46 | | |
| neutral | 48h | 1.5 | slope | 2.63 | +95% | 1282% | 2.03 | 18% | 0.04 | 1.3 / 1.4 | 1.7 / 3.0 / 2.5 |
| neutral | 48h | 2 | residual | 2.42 | +61% | 715% | 2.32 | 16% | 0.34 | 0.5 / 0.5 | 1.0 / 3.0 / 2.1 |
| neutral | 48h | 3 | slope | 1.07 | +10% | 82% | 3.45 | 8% | 0.45 | | |
| reversal (bet the shock continues) | 48h | 2 | slope | -1.37 | | | | | | | |

Does the trend condition matter? (neutral, residual weight, 48h, z ≥ 2)

| condition | Sharpe 0 bps | turnover/day | margin | max DD |
|---|---|---|---|---|
| shock against the trend | 2.42 | 715% | 2.32 | 16% |
| shock with the trend (blow-off) | 0.66 | 440% | 1.15 | 46% |
| any 2σ deviation, no trend test | 0.79 | 1565% | 0.70 | 83% |
| plain 24h residual reversal (`reversal-24h-trend.md`) | 4.04 | 1301% | 2.58 | 22% |

## z threshold ladder (1h panel, neutral, residual weight, 48h, R² ≥ 0.3)

| z ≥ | Sharpe 0 bps | Sharpe 2 bps | annual ret | turnover/day | margin | max DD | avg names L/S | avg gross | yearly Sharpe |
|---|---|---|---|---|---|---|---|---|---|
| 0.5 | 1.95 | -0.64 | | | | | | | |
| 1.0 | 2.07 | -0.62 | | 1667% | 1.54 | 50% | 2.7 / 2.8 | 75% | |
| 1.25 | 2.52 | -0.19 | | | | | | | |
| 1.5 | 2.76 | 0.10 | +100% | 1322% | 2.07 | 24% | 1.4 / 1.4 | 48% | 5.5 / 3.1 / 1.7 |
| 1.75 | 2.60 | 0.14 | | 1027% | 2.11 | 16% | | | |
| 2.0 | 2.42 | 0.34 | +61% | 715% | 2.32 | 16% | 0.5 / 0.5 | 22% | 1.0 / 3.0 / 2.1 |
| 2.5 | 1.01 | -0.27 | | 264% | | | | 7% | |
| 3.0 | 1.16 | 0.57 | +12% | 82% | 3.88 | 9% | ~0 | 2% | |
| 4.0 | 1.67 | 1.44 | | 7.5% | | | ~0 | tiny | |

Bands instead of one-sided thresholds: [1, 2) 0.99, [1.5, 2.5) 1.61, [2, 3) 2.02 (Sharpe, 0 bps).

- Sharpe peaks at z 1.5-1.75 and margin rises monotonically with z, but coverage collapses:
  above z 2.5 the book holds well under one name on average and the PnL is a handful of
  trades. z 3 / z 4 Sharpe numbers are not meaningful.
- Below z 1.25 noise entries dominate: turnover > 1300%/day and the 2 bps Sharpe is negative.
- Practical range z 1.5-2.0: z 1.5 has the best raw Sharpe and most coverage (48% gross),
  z 2 has the best margin and after-cost result (0.34 at 2 bps) with half the gross exposure.
- Bands do not help: the extreme tail (z > 2.5) is where the margin is, so capping |z| removes
  the best trades.

## R² threshold ladder (1h panel, neutral, residual weight, 48h)

R² 0.3 was inherited from the 48h trend-following note, not tuned. Sharpe at 0 bps / 2 bps:

| R² ≥ | z ≥ 2 | turn/day | avg names L/S | z ≥ 1.5 | turn/day | avg names L/S |
|---|---|---|---|---|---|---|
| 0 (no trend filter) | 1.28 / -0.92 | 1006% | 0.8 / 0.8 | 2.12 / -0.77 | 1732% | 2.1 / 2.1 |
| 0.1 | 1.80 / -0.42 | 858% | 0.7 / 0.7 | 2.45 / -0.39 | 1540% | 1.8 / 1.8 |
| 0.2 | 2.13 / -0.08 | 789% | 0.6 / 0.6 | 2.52 / -0.25 | 1438% | 1.6 / 1.6 |
| 0.3 | 2.41 / 0.33 | 715% | 0.5 / 0.5 | 2.76 / 0.10 | 1322% | 1.4 / 1.4 |
| 0.4 | 1.84 / -0.17 | 639% | 0.5 / 0.4 | 3.03 / 0.46 | 1206% | 1.2 / 1.2 |
| 0.45 | | | | 2.72 / 0.23 | 1140% | 1.1 / 1.1 |
| 0.5 | 1.60 / -0.37 | 557% | 0.4 / 0.4 | 3.09 / 0.66 | 1061% | 1.0 / 1.0 |
| 0.55 | | | | 2.60 / 0.18 | 985% | 0.9 / 0.9 |
| 0.6 | 1.73 / -0.10 | 455% | 0.3 / 0.3 | 2.60 / 0.24 | 896% | 0.8 / 0.8 |
| 0.7 | 0.30 / -1.43 | 333% | 0.2 / 0.2 | 1.15 / -0.98 | 672% | 0.5 / 0.5 |

z ≥ 1.75: R² 0.3 2.60, 0.4 2.15, 0.5 2.28. 5m panel (180 d, hourly rebalance, 5 min fill):
z 2 R² 0.3 3.78 vs R² 0.5 2.26; z 1.5 R² 0.3 2.27 vs R² 0.5 2.31.

- Both ends are clearly worse: no trend filter (R² 0) loses 1-1.3 Sharpe and R² ≥ 0.7 leaves
  almost no names. The filter matters.
- Inside 0.2-0.6 the curve is bumpy, not monotone (z 2 peaks at 0.3, z 1.5 at 0.5, z 1.75 at
  0.3, and 0.45 / 0.55 sit below 0.5). With 0.5-1.5 names in the book each step changes a
  handful of trades, so the differences are noise. The 5m panel does not reproduce the z 1.5 /
  R² 0.5 peak.
- Keep R² 0.3: it is in the flat region, it has the most coverage within that region, and it
  is the value shared with the trend-following note. Do not tune further on this sample.

## Reading

- A sudden move against an established trend is a pullback to fade, not a reversal signal:
  continuation is positive in every variant and the reversal bet is its mirror image.
- The trend condition is what makes it work. Fading any 2σ deviation is weak (0.79) and
  fading with-trend blow-offs is weak (0.66); fading counter-trend shocks is 2.4.
- The book is very sparse: on average 1-3 names in position, 20-50% of the book deployed.
  Sharpe is per unit of deployed risk; the dollar PnL is small and concentrated in single
  names. Yearly numbers are consistent (no single-year artefact), which is the encouraging part.
- Costs: break-even 2.0-2.3 bps for the neutral variants, 4 bps for the directional 24h one;
  turnover is high because names enter and exit the book abruptly.
- Compared with the unconditional 24h residual reversal (Sharpe 4.0, 22% DD), this is a subset
  with lower Sharpe but shallower drawdown (16%) and a clearer story.
- Intended use case: an established trend (R² ≥ 0.3) where the name gets knocked against the
  trend by a market-wide move, a liquidation cascade or a short-covering burst, and reverts to
  the trend line within hours. It fails when the shock is the first bar of a real reversal
  (fundamental news) and it is not designed for with-trend blow-offs.

## 5m panel (180 days, hourly rebalance, 5-minute fill, neutral)

| variant | Sharpe 0 bps | annual ret | turnover/day | margin | max DD | 2 bps Sharpe |
|---|---|---|---|---|---|---|
| 48h, z ≥ 2, residual weight | 3.78 | +127% | 1142% | 3.05 | 8% | 1.30 |
| 48h, z ≥ 1.5, slope weight | 2.87 | +118% | 1889% | 1.70 | 16% | -0.50 |

The residual-weighted version holds up with a 5-minute fill (break-even 3 bps); the
slope-weighted one does not. Same ordering as the 1h panel.

## Artifact

1h panel comparison (neutral residual z ≥ 1.5 / 2 / 3, neutral slope z ≥ 1.5, directional 24h
z ≥ 2, and the with-trend shock control), with an intro card describing the intent and the
situations where it does and does not work: https://claude.ai/artifact/QRiwAXQtieriXQXE76SE3a
Rebuild with `tools/wq-report/build.py --intro ...` from the six `wq-bt run --out` directories
listed in the Results section's expressions.

## Open items

1. ~~5m panel with hourly rebalance and 5-minute fill.~~ Done above.
2. Ease entries: scale into the position over a few bars instead of the full jump, and hold a
   minimum number of bars after the shock clears, to cut the entry/exit churn.
3. Combine with the 24h residual reversal as a conviction overlay (larger weight when the shock
   is counter-trend).
