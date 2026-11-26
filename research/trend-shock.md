# Alpha note: counter-trend shock inside a trend (buy the dip / sell the rip)

Status: research, promising but sparse. Last updated 2026-11-26.

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

Variants: weight by the residual (`-ts_resid(...)`) instead of the slope; 24h window; z 1.5/2/3;
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

## 5m panel (180 days, hourly rebalance, 5-minute fill, neutral)

| variant | Sharpe 0 bps | annual ret | turnover/day | margin | max DD | 2 bps Sharpe |
|---|---|---|---|---|---|---|
| 48h, z ≥ 2, residual weight | 3.78 | +127% | 1142% | 3.05 | 8% | 1.30 |
| 48h, z ≥ 1.5, slope weight | 2.87 | +118% | 1889% | 1.70 | 16% | -0.50 |

The residual-weighted version holds up with a 5-minute fill (break-even 3 bps); the
slope-weighted one does not. Same ordering as the 1h panel.

## Open items

1. ~~5m panel with hourly rebalance and 5-minute fill.~~ Done above.
2. Ease entries: scale into the position over a few bars instead of the full jump, and hold a
   minimum number of bars after the shock clears, to cut the entry/exit churn.
3. Combine with the 24h residual reversal as a conviction overlay (larger weight when the shock
   is counter-trend).
