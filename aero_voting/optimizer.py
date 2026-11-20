"""Realistic sAERO allocation optimizer for Aero (Base): forecast -> dilution-aware allocation -> ladder execution.

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
Run:  python -m aero_voting.optimizer [--amount 100000] [--slices 4] [--report reports/aero_voting_optimizer.md]
"""
import argparse
import itertools
from dataclasses import dataclass, asdict, replace

import numpy as np
import pandas as pd

from .backtest import H, Cube, build_panel
from .config import AERO_TOKEN

COOLDOWN_H = 47
S_FLOOR = 10_000.0   # treat gauges with (almost) no other allocators as having 10k VP, so one dust vote is not "100%"


@dataclass(frozen=True)
class Params:
    lookback_days: int = 7
    recency: float = 0.3
    risk: float = 0.5
    max_share: float = 0.35
    min_share: float = 0.03
    max_gauges: int = 8
    min_history_days: int = 3
    horizon_h: int = COOLDOWN_H


# ----------------------------------------------------------------------------- forecast
class Forecaster:
    def __init__(self, cube: Cube, panel: pd.DataFrame):
        self.cube = cube
        ptab, mult, inc = panel.attrs["ptab"], panel.attrs["price_mult"], panel.attrs["incentives"]
        gidx = {g: i for i, g in enumerate(cube.gauges)}
        self.hours = cube.hours
        self.price_rows = {h: i for i, h in enumerate(ptab.index)}
        progs = []
        for r in inc.itertuples():
            t = r.token.lower()
            if r.gauge in gidx and t in ptab.columns:
                progs.append((gidx[r.gauge], int(r.start), int(r.end), float(r.per_sec_f), t, mult[t]))
        self.progs = progs
        self.ptab = ptab
        self.inc_only = cube.reward - cube.fees

    def fee_forecast(self, h: int, p: Params) -> np.ndarray:
        D = p.lookback_days
        fees = self.cube.fees[:, h - 24 * D:h].reshape(len(self.cube.gauges), D, 24).sum(axis=2)   # [g, day]
        mean_d, std_d = fees.mean(axis=1), fees.std(axis=1)
        last24 = fees[:, -1]
        day = (1 - p.recency) * mean_d + p.recency * last24
        day = np.clip(day - p.risk * std_d, 0, None)
        history = (self.cube.supply[:, h - 24 * D:h] > 0).sum(axis=1) >= 24 * min(p.min_history_days, D)
        return np.where(history, day * p.horizon_h / 24, 0.0)

    def incentive_forecast(self, h: int, p: Params) -> np.ndarray:
        t0 = int(self.hours[0]) + h * H                 # h may be one past the last stored hour ("now")
        t1 = t0 + p.horizon_h * H
        out = np.zeros(len(self.cube.gauges))
        prow = self.ptab.iloc[min(h, len(self.ptab) - 1)]
        for g, start, end, per_sec, tok, m in self.progs:
            if start <= t0 < end:                       # known and streaming now
                ov = min(end, t1) - t0
                out[g] += per_sec * ov * prow[tok] * m
        return out

    def forecast(self, h: int, p: Params) -> tuple[np.ndarray, np.ndarray]:
        F = self.fee_forecast(h, p) + self.incentive_forecast(h, p)
        S = self.cube.supply[:, h - 1]
        return F, S

    def realised(self, h: int, p: Params) -> tuple[np.ndarray, np.ndarray]:
        """Perfect foresight (for the ceiling only): actual rewards over the next H and the average supply."""
        h1 = min(h + p.horizon_h, len(self.hours))
        return self.cube.reward[:, h:h1].sum(axis=1), self.cube.supply[:, h:h1].mean(axis=1)


# ----------------------------------------------------------------------------- allocation
def waterfill(F: np.ndarray, S: np.ndarray, b: np.ndarray, A: float, cap: float) -> np.ndarray:
    """argmax sum F (b+a)/(S+b+a), sum a = A, 0 <= a <= cap. Marginal = F S / (S+b+a)^2."""
    S = np.maximum(S, S_FLOOR)
    live = F > 0
    if not live.any() or A <= 0:
        return np.zeros_like(F)

    def alloc(lam):
        a = np.sqrt(np.where(live, F * S, 0) / lam) - S - b
        return np.clip(np.where(live, a, 0), 0, cap)

    lo, hi = 1e-30, float((F * S / (S + b) ** 2)[live].max()) * 1.01
    if alloc(lo).sum() < A:            # caps bind everywhere: everything at its cap
        return alloc(lo)
    for _ in range(200):
        mid = np.sqrt(lo * hi)
        if alloc(mid).sum() > A:
            lo = mid
        else:
            hi = mid
    a = alloc(hi)
    return a * (A / a.sum()) if a.sum() > 0 else a


def allocate(F, S, b, A, p: Params) -> np.ndarray:
    a = waterfill(F, S, b, A, p.max_share * A)
    need = int(np.ceil(1 / p.max_share - 1e-9))           # fewest gauges that can hold A under the per-gauge cap
    for _ in range(5):
        keep = a >= p.min_share * A
        if keep.sum() > p.max_gauges:
            keep &= a >= np.sort(a)[-p.max_gauges]
        if keep.sum() < need and (a > 0).sum() >= need:
            keep = a >= np.sort(a)[-need]
        if keep.all() or (a[~keep] == 0).all():
            break
        F2 = np.where(keep, F, 0.0)
        a = waterfill(F2, S, b, A, p.max_share * A)
    return a


# ----------------------------------------------------------------------------- execution
@dataclass
class Result:
    usd: float
    gas: float
    apr: float
    daily_apr: pd.Series
    tx: int
    forecast_usd: float
    log: pd.DataFrame


def simulate(cube: Cube, fc: Forecaster, aero_px: np.ndarray, p: Params, amount: float, slices: int,
             h_start: int, h_end: int, mode: str = "forecast", gas_usd: float = 0.02) -> Result:
    """mode: 'forecast' (live bot), 'oracle' (perfect foresight, same constraints), 'market' (pro-rata to supply)."""
    G = len(cube.gauges)
    A = amount / slices
    alloc = np.zeros((slices, G))
    next_dec = [h_start + round(k * p.horizon_h / slices) for k in range(slices)]
    prev_total = np.zeros(G)
    hourly = np.zeros(h_end - h_start)
    tx, fcast, log = 0, 0.0, []
    for h in range(h_start, h_end):
        changed = False
        for k in range(slices):
            if h == next_dec[k]:
                b = alloc.sum(axis=0) - alloc[k]
                if mode == "market":
                    S = cube.supply[:, h - 1]
                    new = A * S / S.sum()
                else:
                    F, S = fc.forecast(h, p) if mode == "forecast" else fc.realised(h, p)
                    new = allocate(F, S, b, A, p)
                    Sx = np.maximum(S, S_FLOOR)
                    with np.errstate(divide="ignore", invalid="ignore"):
                        mine = np.where(new > 0, F * (b + new) / (Sx + b + new) - F * b / (Sx + b), 0)
                    fcast += float(mine.sum())
                    log.append(dict(hour=int(cube.hours[h]), slice=k, picks=[(cube.gauges_label[i], float(new[i]))
                                                                             for i in np.argsort(-new)[:p.max_gauges] if new[i] > 0]))
                if not np.allclose(new, alloc[k]):
                    alloc[k] = new
                    tx += 1
                    changed = True
                next_dec[k] += p.horizon_h
        total = alloc.sum(axis=0)
        eff = np.minimum(total, prev_total) if changed else total
        with np.errstate(divide="ignore", invalid="ignore"):
            share = np.where(eff > 0, eff / (cube.supply[:, h] + total), 0.0)
        hourly[h - h_start] = float((cube.reward[:, h] * share).sum())
        prev_total = total
    gas = tx * gas_usd
    hourly[0] -= gas
    yld = hourly / (amount * aero_px[h_start:h_end])           # hourly yield on the AERO value of the capital
    days = pd.Series(yld, index=pd.to_datetime(cube.hours[h_start:h_end], unit="s")).resample("1D").mean() * 8760 * 100
    return Result(usd=float(hourly.sum()), gas=gas, apr=float(yld.mean() * 8760 * 100), daily_apr=days, tx=tx,
                  forecast_usd=fcast, log=pd.DataFrame(log))


# ----------------------------------------------------------------------------- tuning / reporting
GRID = dict(lookback_days=[3, 7], recency=[0.0, 0.5], risk=[0.0, 0.5, 1.0], max_share=[0.25, 0.5])


def tune(cube, fc, px, amount, slices, h0, h1, base: Params):
    rows = []
    for vals in itertools.product(*GRID.values()):
        p = replace(base, **dict(zip(GRID, vals)))
        r = simulate(cube, fc, px, p, amount, slices, h0, h1)
        rows.append(dict(**dict(zip(GRID, vals)), apr=r.apr))
    return pd.DataFrame(rows).sort_values("apr", ascending=False).reset_index(drop=True)


def recommend(cube, fc, aero_px_now, p: Params, amount: float) -> pd.DataFrame:
    h = len(cube.hours)
    F, S = fc.forecast(h, p)
    a = allocate(F, S, np.zeros_like(F), amount, p)
    Sx = np.maximum(S, S_FLOOR)
    rows = []
    for i in np.argsort(-a):
        if a[i] <= 0:
            break
        usd = F[i] * a[i] / (Sx[i] + a[i])
        rows.append(dict(gauge=cube.gauges_label[i], allocate_vp=a[i], share=a[i] / amount, others_vp=S[i],
                         forecast_usd_47h=usd, forecast_apr=usd / (a[i] * aero_px_now) * 8760 / p.horizon_h * 100))
    return pd.DataFrame(rows)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--amount", type=float, default=100_000)
    ap.add_argument("--slices", type=int, default=4, help="number of sAERO positions (staggered cooldowns)")
    ap.add_argument("--spot-haircut", type=float, default=0.5)
    ap.add_argument("--warmup-days", type=int, default=7)
    ap.add_argument("--report", default=None)
    args = ap.parse_args()

    panel, fees, aero_now = build_panel(verbose=True, spot_haircut=args.spot_haircut)
    cube = Cube.from_panel(panel)
    lab = dict(zip(panel.gauge, panel.label))
    cube.gauges_label = [lab[g] for g in cube.gauges]
    px = panel.attrs["ptab"][AERO_TOKEN.lower()].reindex(cube.hours).ffill().bfill().values
    fc = Forecaster(cube, panel)
    n = len(cube.hours)
    h0 = 24 * args.warmup_days
    mid = h0 + (n - h0) // 2
    fmt = lambda h: pd.to_datetime(cube.hours[min(h, n - 1)], unit="s").strftime("%m-%d %H:%M")
    out = []
    say = lambda s="": (print(s), out.append(s))

    say(f"decision period {fmt(h0)} -> {fmt(n)} UTC; tune on {fmt(h0)} -> {fmt(mid)}, test on {fmt(mid)} -> {fmt(n)}")
    say(f"amount {args.amount:,.0f} sAERO in {args.slices} positions, spot-token haircut {args.spot_haircut:.0%}, AERO now ${aero_now:.4f}")

    base = Params()
    grid = tune(cube, fc, px, args.amount, args.slices, h0, mid, base)
    best = replace(base, **{k: grid.iloc[0][k] for k in GRID})
    best = replace(best, lookback_days=int(best.lookback_days))
    say("\n## Tuning (first half), top 5 of %d parameter sets" % len(grid))
    say(grid.head(5).to_markdown(index=False, floatfmt=".2f"))
    say(f"\nchosen: {asdict(best)}")

    # test: every grid point too, to show the chosen one is not a lucky pick
    test_all = [simulate(cube, fc, px, replace(base, **{k: (int(r[k]) if k == 'lookback_days' else r[k]) for k in GRID}),
                         args.amount, args.slices, mid, n).apr for _, r in grid.iterrows()]
    res = {}
    for name, mode, p in [("optimizer (frozen params)", "forecast", best), ("market average", "market", best),
                          ("perfect foresight, same constraints", "oracle", best)]:
        res[name] = simulate(cube, fc, px, p, args.amount, args.slices, mid, n, mode=mode)
    single = simulate(cube, fc, px, best, args.amount, 1, mid, n)
    big = simulate(cube, fc, px, best, args.amount * 10, args.slices, mid, n)
    big_mkt = simulate(cube, fc, px, best, args.amount * 10, args.slices, mid, n, mode="market")

    tab = pd.DataFrame({k: dict(apr_pct=v.apr, usd=v.usd, tx=v.tx, gas_usd=v.gas,
                                worst_day_apr=v.daily_apr.min(), best_day_apr=v.daily_apr.max()) for k, v in res.items()}).T
    say("\n## Out-of-sample test (second half)")
    say(tab.to_markdown(floatfmt=",.2f"))
    o = res["optimizer (frozen params)"]
    say(f"\nforecast vs realised for the optimizer's own picks: forecast ${o.forecast_usd:,.0f}, realised ${o.usd:,.0f} "
        f"({o.usd / o.forecast_usd:.0%} of forecast)")
    say(f"all {len(grid)} parameter sets on the test half: median {np.median(test_all):.1f}%, "
        f"min {np.min(test_all):.1f}%, max {np.max(test_all):.1f}%")
    say(f"1 position instead of {args.slices}: {single.apr:.1f}%   |   {args.amount * 10:,.0f} sAERO: optimizer {big.apr:.1f}% vs market {big_mkt.apr:.1f}%")
    say("\n## Daily APR on the test half (optimizer vs market)")
    say(pd.DataFrame({"optimizer": o.daily_apr, "market": res["market average"].daily_apr}).rename(
        index=lambda d: d.strftime("%m-%d")).to_markdown(floatfmt=".1f"))

    rec = recommend(cube, fc, aero_now, best, args.amount / args.slices)
    say(f"\n## Allocation for one position of {args.amount / args.slices:,.0f} sAERO as of {fmt(n)} UTC")
    say(rec.to_markdown(index=False, floatfmt=",.2f"))
    say(f"forecast APR of this position: {rec.forecast_usd_47h.sum() / (args.amount / args.slices * aero_now) * 8760 / best.horizon_h * 100:.1f}%")

    if args.report:
        with open(args.report, "w") as f:
            f.write("# Aero sAERO allocation optimizer\n\n" + (__doc__ or "").split("Run:")[0].replace("\n", "\n    ") + "\n\n")
            f.write("\n".join(out) + "\n")
        print("report written:", args.report)


if __name__ == "__main__":
    main()
