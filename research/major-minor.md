# Alpha note: majors vs minors (lead-lag and spread)

Status: weak / not tradeable. Last updated 2026-11-25.

## Construction

- Majors `M` = top 5 by trailing 24h dollar volume each bar (`top_n(adv(24), 5)`), minors = the
  other 25 of the 30-name crypto panel.
- `MAJ_h`, `MIN_h` = group mean return over horizon h (`mean_if(returns, M)` summed over h bars).
- Signal is assigned to minors only; market neutralization turns it into long-minor /
  short-major (or the reverse). Majors carry the offsetting weight.
- `beta` = rolling regression slope of `MIN` on `MAJ` (`ts_beta`). Under neutralization +
  scaling a group-level beta is a constant across the minors each bar, so it changes only the
  sign of the book, never its shape: "position proportional to beta" is a no-op unless beta is
  per name. Per-name betas carried no information (Sharpe about 0).

## Results (1h bars, 2024-09 .. 2026-11, cost 0)

| signal | horizon | delay 0 Sharpe | delay 1 Sharpe | turnover/day | margin |
|---|---|---|---|---|---|
| majors lead minors `MAJ_h` | 1h | 1.12 | 0.21 | 2481% | 0.24 |
| | 1D | 0.04 | -0.43 | 477% | 0.04 |
| | 7D | 1.03 | 0.45 | 175% | 3.10 |
| | 14D | 0.43 | 0.01 | 152% | 1.49 |
| | 30D | -0.27 | -0.39 | 106% | -1.33 |
| spread reversal `-(MIN_h - MAJ_h)` | 1h | 1.47 | 1.32 | 2455% | 0.31 |
| | 1D | -0.15 | 0.25 | 463% | -0.17 |
| | 7D | 0.07 | -0.31 | 180% | 0.21 |
| | 14D | -1.04 | -1.05 | 140% | -3.91 |
| | 30D | -0.20 | -0.37 | 87% | -1.25 |
| idiosyncratic reversal `-(r_i - beta_i MAJ)` | 1h | 5.69 | 1.21 | 3331% | 1.28 |
| | 1D | 0.72 | -0.65 | 710% | 0.76 |
| | 7D+ | negative | negative | | |

- 1h lead-lag exists but dies within the hour (delay 1: 0.2). On 5m bars (180 days):
  `MAJ_1h` Sharpe 14.7 / 3.0 / 1.3 at 0 / 5 / 15 min delay; spread reversal 9.7 / 4.1 / 2.8.
  Same decay profile as the single-name reversal, with lower margin (0.2-1.0 bps).
- The 7D lead-lag row (Sharpe 1.0, margin 3.1 bps, turnover 175%) is the only slow variant with a
  positive sign, and it is marginal: +0.03 Sharpe at 3 bps, max DD 26%, 0.45 at delay 1.
- The 1h idiosyncratic reversal is the single-name reversal with the majors' factor removed;
  it is the same alpha as `research/reversal-1h.md`, not a new one.

## Verdict

No horizon from 1h to 30D gives a major/minor signal that pays after costs. The 1h effects are
microstructure-speed (see the reversal note); the multi-day effects are noise or negative.
