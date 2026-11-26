---
name: wq-report
description: Build and publish the standard backtest comparison page (cumulative PnL, gross/net exposure, daily and hour-of-day turnover, rolling win ratio, metrics and monthly tables) from `wq-bt run --out` directories. Use whenever the user asks to chart, visualise or compare wq-backtest results as an artifact.
---

# wq-report: standard backtest chart page

One page design for every comparison, so results from different sessions look the same and a
new page takes minutes, not an hour.

## Steps

1. Run each configuration with an output directory (same alpha or different alphas, any panel):

   ```bash
   cargo run -q -p wq-backtest --bin wq-bt -- run "<alpha>" --data-dir <panel> <flags> --out <runs>/<name>
   ```

   `pnl.csv` must contain `long_notional`/`short_notional` (any build after 2026-11-25 does).
   Up to 8 runs per page (palette slots). Put runs that share a time axis on one page; do not
   mix a 26-month 1h panel with a 180-day 5m panel on the same page.

2. Build the page:

   ```bash
   python3 tools/wq-report/build.py \
     --title "<short name, 2-4 words>" \
     --subtitle "<universe · bars · period · key settings · cost>" \
     --note "<one finding>" --note "<another finding>" \
     --out <report-dir> \
     "<legend label>|<short end-label>=<runs>/<name>" ...
   ```

   The first series gets palette slot 1 (blue), the second orange, then aqua, yellow; keep the
   baseline first so colours stay stable across pages. Labels are Korean when the conversation is.

3. Publish with the Artifact tool: `file_path=<report-dir>/index.html`,
   `files={"data.json": "<report-dir>/data.json"}`, `icon` "chart" on a first publish, a one
   sentence `description`. To update an existing page, publish the same `file_path` again (or pass
   its `url`). Take one screenshot before the first publish if the data shape is new; otherwise skip.

4. In the reply give the link plus the 3-5 numbers that changed the conclusion. The page carries
   the rest; do not repeat every table.

## What the page shows

Tiles (Sharpe, annual return, margin, turnover/day, bar win ratio, average gross) per run;
cumulative PnL as % of book; gross exposure (Σ|w|/book) and net exposure (long − short) as daily
means; daily turnover; average per-bar turnover by UTC hour; 30-day rolling bar win ratio; a
metrics table (adds fitness, MDD, mean IC, average net); a monthly table (bar win % / day win % /
month return %); optional notes. Everything is aggregated per UTC day in `data.json`, so even a
5m panel over two years stays under a few hundred KB.

## Conventions baked in

- Validated categorical palette (dataviz skill reference palette), text never in series colour,
  light and dark themes, 2px lines with surface-ring end markers, legend plus end labels,
  crosshair tooltip listing every series, tables as the accessible fallback.
- Gated or smoothed runs have low average gross; say in a note that annual return is relative to
  the book and compare on Sharpe and margin.
- Reported `turnover (daily)` is per-bar turnover × bars per day, so 5m panels show large numbers;
  that is by design (BRAIN convention).

## Changing the design

Edit `tools/wq-report/build.py` only. If a new chart is needed, add it to the template and the
`load_run` aggregation together, keep the day-level aggregation, and re-run the dataviz palette
validator if colours change.
