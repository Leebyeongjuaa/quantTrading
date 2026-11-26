# Alpha note: 48h linear trend following

Status: research, weak as a neutral alpha; directional version is mostly beta. Last updated 2026-11-26.

## Signal

`keep_if(ts_slope(log(close), 48), ts_r2(log(close), 48) >= R2 && abs(ts_resid_z(log(close), 48)) < 2)`

- Slope of the OLS line through the last 48 hourly log closes (log return per bar).
- Confidence filter: names whose fit has R² below `R2` are dropped (NaN → out of the book,
  weight 0). Outlier filter: a name whose latest close sits more than 2σ (residual std) from its
  line is dropped for that bar.
- Dropping names re-scales the rest of the book, so every entry/exit from the filters is a
  trade: the filters raise turnover from 157% to 600-1000%/day.

## Results (1h panel, 26 months, delay 0, cost 0)

Market neutral (long strongest trends / short weakest):

| variant | Sharpe | turnover/day | margin | max DD | 2 bps Sharpe |
|---|---|---|---|---|---|
| slope48, no filters | 0.48 | 157% | 2.52 | 54% | 0.10 |
| R² ≥ 0.3 | 0.51 | 455% | 1.17 | 68% | -0.36 |
| R² ≥ 0.5 + outlier | 0.10 | 992% | 0.14 | 83% | -1.35 |
| R² ≥ 0.85 + outlier | 0.21 | 400% | 0.48 | 41% | -0.67 |
| 24h / 96h / 168h (R² ≥ 0.5 + outlier) | -0.19 / 0.24 / 0.01 | | | | |

Directional (no neutralization; long up-trends, short down-trends):

| variant | Sharpe | annual return | turnover/day | margin | max DD | 2 bps Sharpe | yearly Sharpe 24/25/26 |
|---|---|---|---|---|---|---|---|
| slope48, no filters | 0.94 | +61% | 158% | 10.6 | 53% | 0.76 | |
| R² ≥ 0.3 | 1.26 | +91% | 302% | 8.3 | 56% | 0.95 | 2.77 / 1.22 / 0.68 |
| R² ≥ 0.85 + outlier | 1.19 | +101% | 543% | 5.1 | 61% | 0.72 | 2.02 / 1.30 / 0.79 |
| R² ≥ 0.5 + outlier | 0.76 | +65% | 827% | 2.2 | 90% | 0.05 | |
| equal-weight long benchmark | 0.52 | +36% | 1% | | 78% | | 2.96 / -0.24 / 0.45 |

## Reading

- Cross-sectional 48h momentum in this universe is close to zero (Sharpe 0.1-0.5) with 50-80%
  drawdowns; reversing the sign is equally bad. There is no 2-day trend edge between names.
- The directional version works only because it is long crypto during up-moves: Sharpe 1.26
  with a 56% drawdown, decaying 2.8 → 1.2 → 0.7 by year. It beats equal-weight long (0.52),
  so the slope filter adds timing value, but it is not market neutral and the 2025 number is
  one bull run.
- The 2σ outlier filter hurts everywhere. Large residuals are where trend PnL is made; removing
  them removes the signal. R² ≥ 0.85 keeps only ~1-2 names at a time (avg 0.8 long / 0.8 short).
- Filters that drop names create turnover through re-scaling; a version that holds a dropped
  name's weight at zero without re-scaling would trade less (not implemented).

## Artifact

https://claude.ai/artifact/RCwgarDDUK6pGN5UVaVtuE (directional R² 0.3 / no filter / R² 0.5 + 2σ,
neutral R² 0.3, equal-weight long benchmark).

## Verdict

Not a tradeable neutral alpha at 48h. If a directional trend sleeve is wanted, use the plain
slope with a mild R² floor (0.3) and accept beta-sized drawdowns; test longer horizons on daily
bars where crypto trend effects are better documented.
