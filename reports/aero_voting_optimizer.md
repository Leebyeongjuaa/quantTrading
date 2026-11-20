# Aero sAERO allocation optimizer

Realistic sAERO allocation optimizer for Aero (Base): forecast -> dilution-aware allocation -> ladder execution.
    
    Everything a decision uses is information a live bot has at that moment:
    
    Forecast (per gauge, USD paid to *all* allocators over the next holding period H = cooldown):
      fees  = blend of the mean daily fees over the last `lookback_days` and the last 24h (`recency`),
              minus `risk` x the day-to-day std (consistency: steady payers beat spiky ones), floored at 0.
              Gauges with less than `min_history_days` of allocation history are skipped.
      incentives = programs already streaming at decision time, at their on-chain rate until they end
              (deterministic; programs that start later are not known yet).
      supply = the gauge's current external allocation (others' voting power), assumed flat over H.
    
    Allocation of one position of size A, given our other positions b_g already on each gauge:
      maximise  sum_g F_g * (b_g + a_g) / (S_g + b_g + a_g)   s.t.  sum a_g = A,  0 <= a_g <= max_share * A
      The objective is concave; the optimum equalises the marginal F S / (S + b + a)^2 (water-filling, solved by
      bisection). Allocations under `min_share` of A are dropped and at most `max_gauges` are kept, then re-solved.
    
    Execution (what the chain allows):
      - capital is split into `slices` sAERO positions; each re-allocates only when its own 47h cooldown ends,
        staggered so the portfolio reacts every 47h / slices;
      - capital newly moved into a gauge earns nothing in its first hour (our allocate() releases the gauge's
        pending fees to the allocators already there);
      - fees/incentives in tokens with no external market price are valued at `spot_haircut` (default 50%);
      - each allocate() costs `gas_usd`.
    Yield is measured against the AERO value of the capital at each hour (AERO's price moves).
    
    Validation: parameters are tuned on the first half of the decision period and frozen for the second half.
    

decision period 10-29 00:00 -> 11-20 05:00 UTC; tune on 10-29 00:00 -> 11-09 03:00, test on 11-09 03:00 -> 11-20 05:00
amount 100,000 sAERO in 4 positions, spot-token haircut 50%, AERO now $0.4113

## Tuning (first half), top 5 of 72 parameter sets
|   lookback_days |   recency |   risk |   max_share |   apr |
|----------------:|----------:|-------:|------------:|------:|
|            5.00 |      0.00 |   0.50 |        0.50 | 20.23 |
|            5.00 |      0.00 |   0.00 |        0.50 | 19.77 |
|            3.00 |      0.00 |   0.50 |        0.25 | 19.75 |
|            5.00 |      0.00 |   0.50 |        0.25 | 19.67 |
|            2.00 |      0.50 |   1.00 |        0.50 | 19.64 |

chosen: {'lookback_days': 5, 'recency': np.float64(0.0), 'risk': np.float64(0.5), 'max_share': np.float64(0.5), 'min_share': 0.03, 'max_gauges': 8, 'min_history_days': 3, 'horizon_h': 47}

## Out-of-sample test (second half)
|                                     |   apr_pct |    usd |    tx |   gas_usd |   worst_day_apr |   best_day_apr |
|:------------------------------------|----------:|-------:|------:|----------:|----------------:|---------------:|
| optimizer (frozen params)           |     23.94 | 338.83 | 23.00 |      0.46 |            8.22 |          37.37 |
| previous heuristic: top-5 equal     |     22.71 | 322.26 | 22.00 |      0.44 |            5.17 |          46.12 |
| market average                      |     19.37 | 276.55 | 23.00 |      0.46 |            5.16 |          28.28 |
| perfect foresight, same constraints |     54.97 | 787.98 | 23.00 |      0.46 |            9.89 |          97.71 |

forecast vs realised for the optimizer's own picks: forecast $429, realised $339 (79% of forecast)
all 72 parameter sets on the test half: median 23.8%, min 12.3%, max 26.1%
1 position instead of 4: 21.5%   |   1,000,000 sAERO: optimizer 22.0% vs market 19.3%

## Where the optimizer's test-half earnings came from (44 gauges used)
|                    |   usd |   share |
|:-------------------|------:|--------:|
| ICP/WETH CL50      | 44.45 |    0.13 |
| WETH/ENA CL50      | 26.59 |    0.08 |
| USDC/VVV CL50      | 20.92 |    0.06 |
| WETH/VEIL volatile | 20.38 |    0.06 |
| CHZ/USDC CL50      | 17.97 |    0.05 |
| SOL/WETH CL10      | 17.65 |    0.05 |
| WETH/VELVET CL50   | 17.35 |    0.05 |
| EURC/cbBTC CL100   | 16.12 |    0.05 |
| USDC/LAPTOP CL200  | 14.37 |    0.04 |
| WETH/ETHFI CL200   | 11.65 |    0.03 |
top 3 gauges = 27% of earnings

## Daily APR on the test half (optimizer vs market)
|       |   optimizer |   market |
|:------|------------:|---------:|
| 11-09 |         8.2 |      5.2 |
| 11-10 |        18.4 |     15.2 |
| 11-11 |        18.9 |     16.1 |
| 11-12 |        17.2 |     17.7 |
| 11-13 |        24.7 |     28.3 |
| 11-14 |        33.3 |     27.2 |
| 11-15 |        37.4 |     22.3 |
| 11-16 |        35.0 |     22.5 |
| 11-17 |        24.4 |     18.7 |
| 11-18 |        22.1 |     17.3 |
| 11-19 |        22.1 |     20.8 |
| 11-20 |        22.8 |     19.3 |

## Allocation for one position of 25,000 sAERO as of 11-20 05:00 UTC
| gauge            |   allocate_vp |   share |    others_vp |   forecast_usd_47h |   forecast_apr |
|:-----------------|--------------:|--------:|-------------:|-------------------:|---------------:|
| WETH/COOKIE CL50 |     12,500.00 |    0.50 |    74,418.62 |               9.95 |          36.06 |
| DRB/WETH CL200   |      8,332.48 |    0.33 |   344,139.38 |               5.48 |          29.79 |
| SOL/USDC CL10    |      2,829.38 |    0.11 | 3,783,030.74 |               1.82 |          29.11 |
| USDC/DRV CL200   |      1,338.14 |    0.05 |    14,656.98 |               0.94 |          31.74 |
forecast APR of this position: 33.0%
