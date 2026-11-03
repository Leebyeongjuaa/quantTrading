"""Walk-forward backtest of sAERO allocation strategies on Aero (Base) since launch.

Builds an hourly panel (gauge x hour) of:
  fees_usd        trading fees released to allocators (NotifyFeesAmount, priced in USD)
  inc_usd         streaming incentives ("bribes") paid to allocators
  supply          allocation voting power on the gauge (decaying veNFT weight + permanent)
and simulates an allocator of size A (sAERO) that may re-allocate every COOLDOWN hours.

Earnings of amount a on gauge g during hour h = (fees_usd + inc_usd)[g,h] * a / (supply[g,h] + a)
(exactly the pro-rata rule of the rewards contracts, with the position's own dilution included).

Run:  python -m aero_voting.backtest [--amount 100000] [--window 48] [--topn 5]
"""
import argparse
import os
from dataclasses import dataclass

import numpy as np
import pandas as pd

from .config import AERO_TOKEN, LAUNCH_TS

DATA = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "data")
H = 3600
COOLDOWN_H = 47  # allocationCooldown on the Base leaf voter at launch
STEP_H = 48      # a bot re-allocates on the first full hour after the cooldown


# ----------------------------------------------------------------------------- data
def load_prices() -> pd.DataFrame:
    p = pd.read_csv(os.path.join(DATA, "prices.csv"))
    p["token"] = p.token.str.lower()
    p["hour"] = (p.ts // H) * H
    return p.groupby(["token", "hour"]).price.mean().reset_index()


def price_lookup(prices: pd.DataFrame, hours: np.ndarray) -> pd.DataFrame:
    """Dense token x hour price table: nearest-hour price, forward/back filled per token."""
    tab = prices.pivot(index="hour", columns="token", values="price")
    tab = tab.reindex(sorted(set(tab.index) | set(hours))).sort_index().ffill().bfill()
    return tab.loc[hours]


def build_panel(verbose=True):
    gauges = pd.read_csv(os.path.join(DATA, "gauges.csv"))
    fees = pd.read_csv(os.path.join(DATA, "fee_events.csv"))
    inc = pd.read_csv(os.path.join(DATA, "incentives.csv"))
    sup = pd.read_csv(os.path.join(DATA, "supply_hourly.csv"))
    prices = load_prices()

    hours = np.array(sorted(sup.ts.unique()))
    ptab = price_lookup(prices, hours)
    priced = set(ptab.columns)

    def usd(token_series, amount_series, hour_series):
        out = np.zeros(len(token_series))
        toks = token_series.str.lower().values
        for i, (t, a, h) in enumerate(zip(toks, amount_series.values, hour_series.values)):
            if t in priced:
                out[i] = a * ptab.at[h, t]
            else:
                out[i] = np.nan
        return out

    # --- fees: value each release, and the exact per-VP credit at release time
    fees["hour"] = (fees.ts // H) * H
    fees["usd"] = usd(fees.token, fees.amount_f, fees.hour)
    unpriced = fees.usd.isna().sum()
    fees["usd"] = fees.usd.fillna(0.0)
    fees["usd_per_vp"] = np.where(fees.supply > 0, fees.usd / fees.supply, 0.0)
    fee_h = fees.groupby(["gauge", "hour"]).agg(fees_usd=("usd", "sum"), fee_releases=("usd", "size")).reset_index()

    # --- incentives: stream each program linearly between start and end
    rows = []
    for r in inc.itertuples():
        tok = r.token.lower()
        if tok not in priced:
            continue
        h0, h1 = (r.start // H) * H, ((r.end - 1) // H) * H
        for h in range(h0, h1 + 1, H):
            ov = max(0, min(r.end, h + H) - max(r.start, h))
            if ov > 0 and h in ptab.index:
                rows.append((r.gauge, h, r.per_sec_f * ov * ptab.at[h, tok]))
    inc_h = pd.DataFrame(rows, columns=["gauge", "hour", "inc_usd"]).groupby(["gauge", "hour"]).inc_usd.sum().reset_index()

    panel = sup.rename(columns={"ts": "hour"}).merge(fee_h, how="left", on=["gauge", "hour"]).merge(inc_h, how="left", on=["gauge", "hour"])
    panel = panel.fillna({"fees_usd": 0.0, "inc_usd": 0.0, "fee_releases": 0})
    panel = panel[panel.hour < hours.max()]  # drop the partial last hour
    panel = panel.merge(gauges[["gauge", "label", "pool_type"]], on="gauge", how="left")
    aero_px = float(ptab[AERO_TOKEN.lower()].iloc[-1]) if AERO_TOKEN.lower() in priced else np.nan
    if verbose:
        print(f"panel: {panel.gauge.nunique()} gauges x {panel.hour.nunique()} hours, "
              f"fees ${panel.fees_usd.sum():,.0f}, incentives ${panel.inc_usd.sum():,.0f}, "
              f"{unpriced} unpriced fee events of {len(fees)}, AERO=${aero_px:.4f}")
    return panel, fees, aero_px


@dataclass
class Cube:
    gauges: list
    hours: np.ndarray
    reward: np.ndarray   # [g, h] USD paid to allocators (fees + incentives)
    fees: np.ndarray     # [g, h] fee part only
    supply: np.ndarray   # [g, h] VP

    @classmethod
    def from_panel(cls, panel: pd.DataFrame):
        gauges = sorted(panel.gauge.unique())
        hours = np.array(sorted(panel.hour.unique()))
        piv = lambda c: panel.pivot(index="gauge", columns="hour", values=c).reindex(index=gauges, columns=hours).fillna(0).values
        return cls(gauges, hours, piv("fees_usd") + piv("inc_usd"), piv("fees_usd"), piv("supply"))

    def earn(self, w: np.ndarray, amount: float, h0: int, h1: int) -> float:
        """USD earned over hours [h0,h1) by allocating amount*w[g] to each gauge (dilution included)."""
        a = amount * w[:, None]
        with np.errstate(divide="ignore", invalid="ignore"):
            share = np.where(a > 0, a / (self.supply[:, h0:h1] + a), 0.0)
        return float((self.reward[:, h0:h1] * share).sum())


# ----------------------------------------------------------------------------- strategies
def expected_rate(cube: Cube, amount: float, h0: int, h1: int) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """Per-gauge trailing stats over [h0,h1): hourly USD earned by `amount` (dilution included)."""
    a = amount
    with np.errstate(divide="ignore", invalid="ignore"):
        share = a / (cube.supply[:, h0:h1] + a)
    e = cube.reward[:, h0:h1] * share                     # [g, hours] USD/hour for our whole amount
    mean = e.mean(axis=1)
    std = e.std(axis=1)
    cover = (cube.reward[:, h0:h1] > 0).mean(axis=1)      # share of hours in which the gauge paid anything
    return mean, std, cover


def w_from_scores(scores: np.ndarray, topn: int, cap: float) -> np.ndarray:
    w = np.zeros_like(scores)
    idx = np.argsort(-scores)[:topn]
    idx = [i for i in idx if scores[i] > 0]
    if not idx:
        return w
    s = scores[idx] / scores[idx].sum()
    # cap each gauge's share, redistribute the excess
    for _ in range(10):
        over = s > cap
        if not over.any():
            break
        excess = (s[over] - cap).sum()
        s[over] = cap
        under = ~over
        if under.any():
            s[under] += excess * s[under] / s[under].sum()
    w[idx] = s
    return w


def strat_greedy(cube, amount, h0, h1, topn, cap):
    mean, std, cover = expected_rate(cube, amount, h0, h1)
    return w_from_scores(mean, 1, 1.0)


def strat_topn_equal(cube, amount, h0, h1, topn, cap):
    mean, std, cover = expected_rate(cube, amount, h0, h1)
    w = np.zeros_like(mean)
    idx = [i for i in np.argsort(-mean)[:topn] if mean[i] > 0]
    w[idx] = 1.0 / len(idx)
    return w


def strat_mean_weighted(cube, amount, h0, h1, topn, cap):
    mean, std, cover = expected_rate(cube, amount, h0, h1)
    return w_from_scores(mean, topn, cap)


def strat_steady(cube, amount, h0, h1, topn, cap, lam=1.0, min_cover=0.5):
    """Prefer gauges that pay consistently: penalise volatility and sparse payers."""
    mean, std, cover = expected_rate(cube, amount, h0, h1)
    score = (mean - lam * std / np.sqrt(max(1, (h1 - h0) / 6)))  # std of a 6h-bucket mean, roughly
    score = np.where(cover >= min_cover, score, 0.0)
    score = np.clip(score, 0, None)
    return w_from_scores(score, topn, cap)


def strat_blend(cube, amount, h0, h1, topn, cap):
    """What the user asked for: mix 'highest' and 'most consistent' instead of chasing the single top gauge.
    score = mean * sqrt(cover) / (1 + cv)   with cv = std/mean (coefficient of variation)."""
    mean, std, cover = expected_rate(cube, amount, h0, h1)
    with np.errstate(divide="ignore", invalid="ignore"):
        cv = np.where(mean > 0, std / mean, np.inf)
    score = np.where(mean > 0, mean * np.sqrt(cover) / (1 + cv), 0.0)
    return w_from_scores(score, topn, cap)


def strat_market(cube, amount, h0, h1, topn, cap):
    """Allocate pro-rata to current supply = the average allocator."""
    s = cube.supply[:, h1 - 1]
    return s / s.sum() if s.sum() > 0 else np.zeros_like(s)


def strat_oracle(cube, amount, h0, h1, topn, cap):
    """Hindsight: best single gauge of the *next* period (upper bound, not tradeable)."""
    return None  # handled in the loop


STRATS = {
    "greedy_top1": strat_greedy,
    "topN_equal": strat_topn_equal,
    "topN_mean_weighted": strat_mean_weighted,
    "steady": strat_steady,
    "blend_high_x_steady": strat_blend,
    "market_avg": strat_market,
    "oracle_next_period": strat_oracle,
}


# ----------------------------------------------------------------------------- walk-forward
def run(cube: Cube, amount: float, window_h: int, topn: int, cap: float, aero_px: float, start_h: int | None = None):
    n_h = len(cube.hours)
    first = start_h if start_h is not None else window_h
    out, log = {}, []
    for name, fn in STRATS.items():
        total, h = 0.0, first
        while h < n_h:
            h_end = min(h + STEP_H, n_h)
            if name == "oracle_next_period":
                g = np.argmax([cube.earn(np.eye(len(cube.gauges))[i], amount, h, h_end) for i in range(len(cube.gauges))])
                w = np.eye(len(cube.gauges))[g]
            else:
                w = fn(cube, amount, max(0, h - window_h), h, topn, cap)
            e = cube.earn(w, amount, h, h_end)
            total += e
            picks = ", ".join(f"{cube.gauges_label[i]} {w[i]:.0%}" for i in np.argsort(-w)[:topn] if w[i] > 0)
            log.append(dict(strategy=name, hour=int(cube.hours[h]), period_h=h_end - h, usd=e, picks=picks))
            h = h_end
        days = (n_h - first) / 24
        apr = total / (amount * aero_px) * 365 / days if aero_px and days > 0 else np.nan
        out[name] = dict(usd=total, days=days, apr=apr, usd_per_day=total / days)
    return pd.DataFrame(out).T, pd.DataFrame(log)


def gauge_table(panel: pd.DataFrame, fees: pd.DataFrame, aero_px: float) -> pd.DataFrame:
    g = panel.groupby(["gauge", "label"]).agg(fees_usd=("fees_usd", "sum"), inc_usd=("inc_usd", "sum"),
                                              avg_supply=("supply", "mean"), hours=("hour", "count"),
                                              hours_paying=("fees_usd", lambda s: (s > 0).sum()),
                                              releases=("fee_releases", "sum")).reset_index()
    g["total_usd"] = g.fees_usd + g.inc_usd
    g["usd_per_1M_vp_per_day"] = g.total_usd / g.avg_supply.replace(0, np.nan) * 1e6 / (g.hours / 24)
    g["apr_pct"] = g.usd_per_1M_vp_per_day * 365 / (1e6 * aero_px) * 100
    # steadiness: coefficient of variation of daily reward per VP
    daily = panel.assign(day=panel.hour // 86400).groupby(["gauge", "day"]).apply(
        lambda d: (d.fees_usd + d.inc_usd).sum() / max(d.supply.mean(), 1e-9), include_groups=False).reset_index(name="r")
    cv = daily.groupby("gauge").r.agg(lambda s: s.std() / s.mean() if s.mean() > 0 else np.nan).rename("daily_cv")
    g = g.merge(cv, on="gauge", how="left")
    return g.sort_values("total_usd", ascending=False)


def release_cadence(fees: pd.DataFrame) -> pd.Series:
    f = fees.sort_values("ts")
    gaps = f.groupby("gauge").ts.diff().dropna()
    per_day = f.groupby(f.ts // 86400).size()
    return pd.Series({
        "fee_release_events": len(f),
        "gauges_with_releases": f.gauge.nunique(),
        "median_gap_between_releases_min": gaps.median() / 60,
        "p90_gap_min": gaps.quantile(0.9) / 60,
        "releases_per_day_median": per_day.median(),
    })


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--amount", type=float, default=100_000, help="sAERO position size (AERO)")
    ap.add_argument("--window", type=int, default=48, help="trailing look-back hours")
    ap.add_argument("--topn", type=int, default=5)
    ap.add_argument("--cap", type=float, default=0.4, help="max share per gauge for split strategies")
    ap.add_argument("--report", default=None, help="write markdown report to this path")
    args = ap.parse_args()

    panel, fees, aero_px = build_panel()
    cube = Cube.from_panel(panel)
    lab = dict(zip(panel.gauge, panel.label))
    cube.gauges_label = [lab[g] for g in cube.gauges]

    print("\n== fee release cadence (continuous, not weekly) ==")
    print(release_cadence(fees).to_string())

    gt = gauge_table(panel, fees, aero_px)
    print(f"\n== top gauges by USD paid to allocators since launch (AERO=${aero_px:.4f}) ==")
    cols = ["label", "fees_usd", "inc_usd", "avg_supply", "hours_paying", "usd_per_1M_vp_per_day", "apr_pct", "daily_cv"]
    print(gt[cols].head(20).to_string(index=False, float_format=lambda x: f"{x:,.2f}"))

    print(f"\n== walk-forward, amount={args.amount:,.0f} sAERO, window={args.window}h, step={STEP_H}h, topN={args.topn}, cap={args.cap:.0%} ==")
    res, log = run(cube, args.amount, args.window, args.topn, args.cap, aero_px)
    res["apr_pct"] = res.apr * 100
    print(res[["usd", "usd_per_day", "apr_pct", "days"]].to_string(float_format=lambda x: f"{x:,.2f}"))

    print("\n== sensitivity: APR % by amount (rows) x strategy ==")
    sens = {}
    for amt in [10_000, 100_000, 1_000_000, 10_000_000]:
        r, _ = run(cube, amt, args.window, args.topn, args.cap, aero_px)
        sens[f"{amt:,.0f}"] = r.apr * 100
    print(pd.DataFrame(sens).T.to_string(float_format=lambda x: f"{x:,.1f}"))

    print("\n== sensitivity: APR % by look-back window (rows), amount=%s ==" % f"{args.amount:,.0f}")
    sens = {}
    for w in [24, 48, 96, 168]:
        r, _ = run(cube, args.amount, w, args.topn, args.cap, aero_px, start_h=168)
        sens[f"{w}h"] = r.apr * 100
    print(pd.DataFrame(sens).T.to_string(float_format=lambda x: f"{x:,.1f}"))

    print("\n== picks per period (blend_high_x_steady) ==")
    print(log[log.strategy == "blend_high_x_steady"][["hour", "usd", "picks"]].assign(
        hour=lambda d: pd.to_datetime(d.hour, unit="s").dt.strftime("%m-%d %Hh")).to_string(index=False, float_format=lambda x: f"{x:,.0f}"))

    if args.report:
        with open(args.report, "w") as f:
            f.write("# Aero sAERO allocation backtest\n\n")
            f.write(f"Data: Base gauges, {pd.to_datetime(cube.hours[0], unit='s')} to {pd.to_datetime(cube.hours[-1], unit='s')} UTC. AERO = ${aero_px:.4f}\n\n")
            f.write("## Fee release cadence\n\n```\n" + release_cadence(fees).to_string() + "\n```\n\n")
            f.write("## Top gauges\n\n" + gt[cols].head(25).to_markdown(index=False, floatfmt=",.2f") + "\n\n")
            f.write(f"## Walk-forward (amount={args.amount:,.0f}, window={args.window}h, step={STEP_H}h, topN={args.topn}, cap={args.cap:.0%})\n\n")
            f.write(res[["usd", "usd_per_day", "apr_pct", "days"]].to_markdown(floatfmt=",.2f") + "\n\n")
            f.write("## Picks (blend_high_x_steady)\n\n" + log[log.strategy == "blend_high_x_steady"][["hour", "usd", "picks"]].assign(
                hour=lambda d: pd.to_datetime(d.hour, unit="s")).to_markdown(index=False, floatfmt=",.0f") + "\n")
        print("report written:", args.report)


if __name__ == "__main__":
    main()
