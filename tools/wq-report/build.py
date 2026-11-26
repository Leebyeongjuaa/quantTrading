#!/usr/bin/env python3
"""Build a comparison page from `wq-bt run --out <dir>` outputs.

Usage:
  python3 tools/wq-report/build.py --title "Reversal 5m latency" \
      --subtitle "30 crypto perps, 5m bars, 2026-05-29..11-25" \
      --note "First observation" --note "Second observation" \
      --out report/ \
      "0 min=runs/d0" "5 min=runs/d1" "15 min=runs/d3"

Each positional argument is `label=path` (optionally `label|short=path`). The page is written to
<out>/index.html with <out>/data.json beside it (fetched at runtime), ready to publish with the
Artifact tool: file_path=<out>/index.html, files={"data.json": "<out>/data.json"}.

Charts: cumulative PnL (% of book), gross and net exposure, daily turnover, hour-of-day turnover,
30-day rolling bar win ratio, a metrics table and a monthly table. All series share one palette
slot order (max 8); numbers are aggregated per UTC day so the page stays small.
"""
import argparse
import csv
import datetime as dt
import json
import os
from collections import OrderedDict

PALETTE = [("#2a78d6", "#3987e5"), ("#eb6834", "#d95926"), ("#1baf7a", "#199e70"), ("#eda100", "#c98500"),
           ("#e87ba4", "#d55181"), ("#008300", "#0ca30c"), ("#6250d6", "#9085e9"), ("#e34948", "#e66767")]
DAY = 86_400_000


def load_run(path, book):
    rows = list(csv.DictReader(open(os.path.join(path, "pnl.csv"))))
    summ = json.load(open(os.path.join(path, "summary.json")))
    first = next((i for i, r in enumerate(rows) if int(r["long"]) + int(r["short"]) > 0), 0)
    rows = rows[first:]
    days, hod_sum, hod_n = OrderedDict(), [0.0] * 24, [0] * 24
    wins = nz = 0
    for r in rows:
        ts = int(r["ts"]); d = ts - ts % DAY
        pnl = float(r["pnl"]); to = float(r["turnover"])
        ln, sn = float(r.get("long_notional", 0) or 0), float(r.get("short_notional", 0) or 0)
        e = days.setdefault(d, {"cum": 0.0, "g": 0.0, "net": 0.0, "to": 0.0, "w": 0, "nz": 0, "n": 0, "pnl": 0.0})
        e["cum"] = float(r["cum_pnl"]) / book * 100
        e["g"] += (ln + sn) / book; e["net"] += (ln - sn) / book; e["to"] += to; e["n"] += 1; e["pnl"] += pnl
        if pnl != 0.0:
            e["nz"] += 1; nz += 1
            if pnl > 0:
                e["w"] += 1; wins += 1
        h = (ts // 3_600_000) % 24
        hod_sum[h] += to; hod_n[h] += 1
    daily = [[d, round(e["cum"], 3), round(e["g"] / e["n"] * 100, 2), round(e["net"] / e["n"] * 100, 2),
              round(e["to"] * 100, 2), e["w"], e["nz"], round(e["pnl"] / book * 100, 4)] for d, e in days.items()]
    hod = [round(hod_sum[h] / hod_n[h] * 100, 3) if hod_n[h] else None for h in range(24)]
    months = OrderedDict()
    for d, e in days.items():
        m = dt.datetime.fromtimestamp(d / 1000, dt.timezone.utc).strftime("%Y-%m")
        me = months.setdefault(m, {"w": 0, "nz": 0, "dw": 0, "dn": 0, "pnl": 0.0})
        me["w"] += e["w"]; me["nz"] += e["nz"]; me["dn"] += 1; me["dw"] += 1 if e["pnl"] > 0 else 0; me["pnl"] += e["pnl"]
    keys = ["sharpe", "annual_return", "turnover_daily", "fitness", "margin_bps", "max_drawdown_pct", "mean_ic",
            "avg_gross", "avg_net", "bars", "bars_per_year"]
    return {
        "daily": daily, "hod": hod,
        "summary": {k: summ.get(k) for k in keys},
        "win_ratio_bars": wins / nz if nz else None,
        "months": [[m, round(v["w"] / v["nz"] * 100, 1) if v["nz"] else None, round(v["dw"] / v["dn"] * 100, 1),
                    round(v["pnl"] / book * 100, 2)] for m, v in months.items()],
    }


TEMPLATE = r"""<title>__TITLE__</title>
<meta name="claude:viewport-fit-sides" content="cover">
<style>
/* Layout: one column; summary tiles, then cumulative PnL, exposure, turnover, win ratio, tables, notes. */
:root {
  --bg: #f9f9f7; --surface: #fcfcfb; --ink: #0b0b0b; --ink-2: #52514e; --muted: #898781;
  --grid: #e1e0d9; --axis: #c3c2b7; --border: rgba(11,11,11,0.10);
  __LIGHT_SLOTS__
  --neg: #d03b3b; --pos: #006300;
  --font: system-ui, -apple-system, "Segoe UI", sans-serif;
}
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) {
    --bg: #0d0d0d; --surface: #1a1a19; --ink: #f0efec; --ink-2: #c3c2b7; --muted: #898781;
    --grid: #2c2c2a; --axis: #383835; --border: rgba(255,255,255,0.10);
    __DARK_SLOTS__
    --neg: #e66767; --pos: #0ca30c;
    color-scheme: dark;
  }
}
:root[data-theme="dark"] {
  --bg: #0d0d0d; --surface: #1a1a19; --ink: #f0efec; --ink-2: #c3c2b7; --muted: #898781;
  --grid: #2c2c2a; --axis: #383835; --border: rgba(255,255,255,0.10);
  __DARK_SLOTS__
  --neg: #e66767; --pos: #0ca30c;
  color-scheme: dark;
}
body { background: var(--bg); color: var(--ink); font-family: var(--font); font-size: 14px; line-height: 1.5;
  padding-left: max(16px, env(safe-area-inset-left, 0px)); padding-right: max(16px, env(safe-area-inset-right, 0px)); padding-block: 24px 48px; }
main { max-width: 1040px; margin: 0 auto; display: grid; gap: 24px; }
h1 { font-size: 22px; margin: 0; font-weight: 600; text-wrap: balance; }
h2 { font-size: 15px; margin: 0; font-weight: 600; }
.sub { color: var(--ink-2); margin: 4px 0 0; }
.tiles { display: grid; grid-template-columns: repeat(auto-fit, minmax(210px, 1fr)); gap: 12px; }
.tile { background: var(--surface); border: 1px solid var(--border); border-radius: 8px; padding: 12px 14px; display: grid; gap: 2px; min-width: 0; }
.tile .name { display: flex; align-items: center; gap: 8px; color: var(--ink-2); font-size: 13px; }
.key { width: 14px; height: 2px; border-radius: 1px; flex: none; display: inline-block; }
.tile .value { font-size: 26px; font-weight: 600; }
.tile .meta { color: var(--muted); font-size: 12px; display: flex; gap: 10px; flex-wrap: wrap; }
.tile .meta b { color: var(--ink-2); font-weight: 500; }
.card { background: var(--surface); border: 1px solid var(--border); border-radius: 8px; padding: 16px; display: grid; gap: 10px; min-width: 0; }
.head { display: flex; justify-content: space-between; align-items: baseline; gap: 12px; flex-wrap: wrap; }
.legend { display: flex; gap: 14px; flex-wrap: wrap; color: var(--ink-2); font-size: 13px; }
.legend span { display: inline-flex; align-items: center; gap: 6px; }
.legend .sw { width: 14px; height: 2px; border-radius: 1px; }
.chart { position: relative; width: 100%; min-width: 0; }
.chart svg { display: block; width: 100%; height: auto; overflow: visible; }
.chart text { font-family: var(--font); font-size: 11px; fill: var(--muted); }
.chart .endlab { fill: var(--ink-2); font-size: 11px; }
.chart .grid { stroke: var(--grid); stroke-width: 1; }
.chart .axis, .chart .zero { stroke: var(--axis); stroke-width: 1; }
.chart .ln { fill: none; stroke-width: 2; stroke-linejoin: round; stroke-linecap: round; }
.chart .dot { stroke: var(--surface); stroke-width: 2; }
.chart .xh { stroke: var(--axis); stroke-width: 1; pointer-events: none; }
.tip { position: absolute; pointer-events: none; background: var(--surface); border: 1px solid var(--border); border-radius: 6px; padding: 8px 10px; font-size: 12px; box-shadow: 0 4px 16px rgba(0,0,0,0.12); display: grid; gap: 3px; min-width: 170px; z-index: 2; }
.tip[hidden] { display: none; }
.tip .t { color: var(--muted); }
.tip .r { display: flex; align-items: center; gap: 8px; justify-content: space-between; }
.tip .r .k { display: inline-flex; align-items: center; gap: 6px; color: var(--ink-2); }
.tip .r b { font-variant-numeric: tabular-nums; }
table { width: 100%; border-collapse: collapse; font-size: 13px; }
th, td { text-align: right; padding: 7px 8px; border-bottom: 1px solid var(--grid); font-variant-numeric: tabular-nums; white-space: nowrap; }
th:first-child, td:first-child { text-align: left; }
th { color: var(--ink-2); font-weight: 500; }
td.neg { color: var(--neg); }
td.pos { color: var(--pos); }
.scroll { overflow-x: auto; }
.notes { color: var(--ink-2); font-size: 13px; display: grid; gap: 6px; padding-left: 18px; margin: 0; }
.rel { color: var(--muted); font-size: 12px; }
.err { color: var(--neg); }
</style>
<main>
  <header><h1>__TITLE__</h1><p class="sub">__SUBTITLE__</p></header>
  <section class="tiles" id="tiles"></section>
  <section class="card"><div class="head"><div><h2>누적 PnL (북 대비 %)</h2><div class="rel">일 단위 집계. 노출이 낮은 설정은 북 대비 수익률이 작게 보이므로 Sharpe와 margin으로 비교.</div></div><div class="legend" data-legend></div></div><div class="chart" id="cum"></div></section>
  <section class="card"><div class="head"><div><h2>총 노출 (Σ|w| / 북, 일 평균 %)</h2></div><div class="legend" data-legend></div></div><div class="chart" id="gross"></div></section>
  <section class="card"><div class="head"><div><h2>순 노출 (롱 − 숏, 일 평균 %)</h2></div><div class="legend" data-legend></div></div><div class="chart" id="net"></div></section>
  <section class="card"><div class="head"><div><h2>일 turnover (봉별 turnover의 하루 합, %)</h2></div><div class="legend" data-legend></div></div><div class="chart" id="turn"></div></section>
  <section class="card"><div class="head"><div><h2>시간대별 평균 봉 turnover (UTC 시, %)</h2></div><div class="legend" data-legend></div></div><div class="chart" id="hod"></div></section>
  <section class="card"><div class="head"><div><h2>승률 (봉 단위, 30일 롤링 %)</h2><div class="rel">PnL이 0이 아닌 봉 중 양수 비율.</div></div><div class="legend" data-legend></div></div><div class="chart" id="win"></div></section>
  <section class="card"><h2>지표 요약</h2><div class="scroll"><table id="table"></table></div></section>
  <section class="card"><h2>월별 (봉 승률 % / 일 승률 % / 월 수익률 %)</h2><div class="scroll"><table id="months"></table></div></section>
  __NOTES__
</main>
<script>
const fmtPct = (v, d = 2) => (v > 0 ? '+' : '') + v.toFixed(d) + '%';
const fmtP = (v, d = 1) => v.toFixed(d) + '%';
const fmtDay = ms => { const d = new Date(ms); return `${d.getUTCFullYear()}-${String(d.getUTCMonth() + 1).padStart(2, '0')}-${String(d.getUTCDate()).padStart(2, '0')}`; };
const svgEl = (tag, attrs = {}) => { const e = document.createElementNS('http://www.w3.org/2000/svg', tag); for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v); return e; };
const txt = (tag, cls, s) => { const e = document.createElement(tag); if (cls) e.className = cls; e.textContent = s; return e; };
function niceStep(raw) { const p = Math.pow(10, Math.floor(Math.log10(raw))); const f = raw / p; return (f <= 1 ? 1 : f <= 2 ? 2 : f <= 5 ? 5 : 10) * p; }
function nearest(xs, t) { let lo = 0, hi = xs.length - 1; while (lo < hi) { const mid = (lo + hi) >> 1; if (xs[mid] < t) lo = mid + 1; else hi = mid; } if (lo > 0 && Math.abs(xs[lo - 1] - t) < Math.abs(xs[lo] - t)) lo--; return lo; }
function lineChart(host, lines, opts) {
  const W = 960, H = opts.H || 300, m = { l: 52, r: opts.endLabels ? 150 : 16, t: 12, b: 32 };
  const allX = lines.flatMap(l => l.xs), allY = lines.flatMap(l => l.ys).filter(Number.isFinite);
  const x0 = Math.min(...allX), x1 = Math.max(...allX);
  let y0 = opts.yMin ?? Math.min(opts.zero ? 0 : Infinity, ...allY), y1 = opts.yMax ?? Math.max(opts.zero ? 0 : -Infinity, ...allY);
  const step = niceStep((y1 - y0) / 5 || 1); if (opts.yMin == null) y0 = Math.floor(y0 / step) * step; if (opts.yMax == null) y1 = Math.ceil(y1 / step) * step;
  const X = v => m.l + (v - x0) / (x1 - x0 || 1) * (W - m.l - m.r);
  const Y = v => m.t + (y1 - v) / (y1 - y0 || 1) * (H - m.t - m.b);
  const svg = svgEl('svg', { viewBox: `0 0 ${W} ${H}`, role: 'img', 'aria-label': opts.label || '' });
  for (let v = y0; v <= y1 + 1e-9; v += step) {
    svg.append(svgEl('line', { x1: m.l, x2: W - m.r, y1: Y(v), y2: Y(v), class: Math.abs(v) < 1e-9 ? 'zero' : 'grid' }));
    const t = svgEl('text', { x: m.l - 8, y: Y(v) + 4, 'text-anchor': 'end' }); t.textContent = opts.yFmt(v); svg.append(t);
  }
  for (const [x, label] of opts.xLabels(x0, x1)) { const t = svgEl('text', { x: X(x), y: H - m.b + 18, 'text-anchor': 'middle' }); t.textContent = label; svg.append(t); }
  svg.append(svgEl('line', { x1: m.l, x2: W - m.r, y1: H - m.b, y2: H - m.b, class: 'axis' }));
  const ends = [];
  for (const l of lines) {
    let d = '', pen = false;
    l.xs.forEach((x, i) => { const y = l.ys[i]; if (!Number.isFinite(y)) { pen = false; return; } d += (pen ? 'L' : 'M') + X(x).toFixed(1) + ' ' + Y(y).toFixed(1); pen = true; });
    svg.append(svgEl('path', { d, class: 'ln', stroke: l.s.color }));
    const li = l.ys.length - 1;
    if (opts.endLabels && Number.isFinite(l.ys[li])) { svg.append(svgEl('circle', { cx: X(l.xs[li]), cy: Y(l.ys[li]), r: 4, fill: l.s.color, class: 'dot' })); ends.push({ s: l.s, y: Y(l.ys[li]), v: l.ys[li] }); }
  }
  ends.sort((a, b) => a.y - b.y);
  for (let i = 1; i < ends.length; i++) if (ends[i].y - ends[i - 1].y < 14) ends[i].y = ends[i - 1].y + 14;
  for (const e of ends) {
    svg.append(svgEl('line', { x1: W - m.r + 2, x2: W - m.r + 12, y1: Y(e.v), y2: e.y, class: 'grid' }));
    const t = svgEl('text', { x: W - m.r + 16, y: e.y + 4, class: 'endlab' }); t.textContent = `${e.s.short}  ${opts.yFmt(e.v)}`; svg.append(t);
  }
  const xh = svgEl('line', { class: 'xh', y1: m.t, y2: H - m.b, visibility: 'hidden' }); svg.append(xh);
  const dots = lines.map(l => { const c = svgEl('circle', { r: 4, fill: l.s.color, class: 'dot', visibility: 'hidden' }); svg.append(c); return c; });
  host.replaceChildren(svg);
  const tip = document.createElement('div'); tip.className = 'tip'; tip.hidden = true; host.append(tip);
  const ref = lines.reduce((a, b) => (a.xs.length >= b.xs.length ? a : b)).xs;
  svg.addEventListener('pointermove', ev => {
    const r = svg.getBoundingClientRect(); const px = (ev.clientX - r.left) / r.width * W;
    if (px < m.l || px > W - m.r) { leave(); return; }
    const at = ref[nearest(ref, x0 + (px - m.l) / (W - m.l - m.r) * (x1 - x0))];
    xh.setAttribute('x1', X(at)); xh.setAttribute('x2', X(at)); xh.setAttribute('visibility', 'visible');
    tip.replaceChildren(txt('div', 't', opts.xFmt(at)));
    lines.forEach((l, i) => {
      const j = nearest(l.xs, at); const y = l.ys[j];
      if (l.xs[j] !== at || !Number.isFinite(y)) { dots[i].setAttribute('visibility', 'hidden'); return; }
      dots[i].setAttribute('cx', X(at)); dots[i].setAttribute('cy', Y(y)); dots[i].setAttribute('visibility', 'visible');
      const row = txt('div', 'r', ''); const k = txt('span', 'k', ''); const sw = document.createElement('i'); sw.className = 'key'; sw.style.background = l.s.color; k.append(sw, txt('span', '', l.s.name));
      row.append(k, txt('b', '', opts.yFmt(y))); tip.append(row);
    });
    tip.hidden = false;
    const left = (X(at) / W) * r.width; tip.style.left = (left + 200 > r.width ? left - 210 : left + 12) + 'px'; tip.style.top = '8px';
  });
  function leave() { xh.setAttribute('visibility', 'hidden'); dots.forEach(d => d.setAttribute('visibility', 'hidden')); tip.hidden = true; }
  svg.addEventListener('pointerleave', leave);
}
const monthLabels = (x0, x1) => { const out = []; const d = new Date(x0); d.setUTCDate(1); d.setUTCHours(0, 0, 0, 0); d.setUTCMonth(d.getUTCMonth() + 1); const span = (x1 - x0) / 86400000; const every = span > 500 ? 3 : span > 200 ? 2 : 1; let k = 0; while (d.getTime() <= x1) { if (k % every === 0) out.push([d.getTime(), `${d.getUTCFullYear()}-${String(d.getUTCMonth() + 1).padStart(2, '0')}`]); d.setUTCMonth(d.getUTCMonth() + 1); k++; } return out; };
function render(DATA) {
  const SERIES = DATA.series.map(s => ({ ...s, color: `var(--s${s.slot})` }));
  document.querySelectorAll('[data-legend]').forEach(l => { for (const s of SERIES) { const sp = document.createElement('span'); const sw = document.createElement('i'); sw.className = 'sw'; sw.style.background = s.color; sp.append(sw, txt('span', '', s.name)); l.append(sp); } });
  const book = DATA.booksize;
  const tiles = document.getElementById('tiles');
  for (const s of SERIES) {
    const d = DATA.runs[s.id], m = d.summary;
    const t = document.createElement('div'); t.className = 'tile';
    const name = txt('div', 'name', ''); const key = document.createElement('i'); key.className = 'key'; key.style.background = s.color; name.append(key, txt('span', '', s.name));
    const meta = txt('div', 'meta', ''); const add = (k, v) => { const b = txt('span', '', k + ' '); b.append(txt('b', '', v)); meta.append(b); };
    add('연수익', fmtPct(m.annual_return * 100)); add('margin', m.margin_bps.toFixed(2) + ' bps'); add('turn/d', fmtP(m.turnover_daily * 100, 0)); add('승률', fmtP(d.win_ratio_bars * 100)); add('평균 노출', fmtP(m.avg_gross / book * 100, 0));
    t.append(name, txt('div', 'value', 'Sharpe ' + m.sharpe.toFixed(2)), meta); tiles.append(t);
  }
  const col = (s, i) => ({ s, xs: DATA.runs[s.id].daily.map(r => r[0]), ys: DATA.runs[s.id].daily.map(r => r[i]) });
  const L = i => SERIES.map(s => col(s, i));
  lineChart(document.getElementById('cum'), L(1), { H: 340, yFmt: v => fmtPct(v, 0), xFmt: fmtDay, xLabels: monthLabels, zero: true, endLabels: true, label: '누적 PnL' });
  lineChart(document.getElementById('gross'), L(2), { H: 240, yFmt: v => fmtP(v, 0), xFmt: fmtDay, xLabels: monthLabels, yMin: 0, yMax: 120, label: '총 노출' });
  lineChart(document.getElementById('net'), L(3), { H: 240, yFmt: v => fmtPct(v, 0), xFmt: fmtDay, xLabels: monthLabels, zero: true, label: '순 노출' });
  lineChart(document.getElementById('turn'), L(4), { H: 260, yFmt: v => fmtP(v, 0), xFmt: fmtDay, xLabels: monthLabels, yMin: 0, label: '일 turnover' });
  lineChart(document.getElementById('hod'), SERIES.map(s => ({ s, xs: [...Array(24).keys()], ys: DATA.runs[s.id].hod })), { H: 240, yFmt: v => fmtP(v, 0), xFmt: h => `${h}:00 UTC`, xLabels: () => [...Array(24).keys()].filter(h => h % 3 === 0).map(h => [h, `${h}:00`]), yMin: 0, label: '시간대별 turnover' });
  const win = SERIES.map(s => { const d = DATA.runs[s.id].daily; const xs = [], ys = []; let w = 0, n = 0; for (let i = 0; i < d.length; i++) { w += d[i][5]; n += d[i][6]; if (i >= 30) { w -= d[i - 30][5]; n -= d[i - 30][6]; } xs.push(d[i][0]); ys.push(i >= 29 && n > 0 ? w / n * 100 : NaN); } return { s, xs, ys }; });
  lineChart(document.getElementById('win'), win, { H: 260, yFmt: v => fmtP(v, 0), xFmt: fmtDay, xLabels: monthLabels, yMin: 40, yMax: 60, label: '롤링 승률' });
  const t = document.getElementById('table');
  const cols = ['설정', 'Sharpe', '연수익', '일 turnover', 'fitness', 'margin (bps)', 'MDD', 'mean IC', '봉 승률', '평균 총노출', '평균 순노출'];
  const thead = document.createElement('thead'); const hr = document.createElement('tr'); cols.forEach(c => hr.append(txt('th', '', c))); thead.append(hr); t.append(thead);
  const tb = document.createElement('tbody');
  for (const s of SERIES) {
    const d = DATA.runs[s.id], m = d.summary; const tr = document.createElement('tr');
    const first = txt('td', '', ''); const sw = document.createElement('i'); sw.className = 'key'; sw.style.background = s.color; sw.style.marginRight = '8px'; sw.style.verticalAlign = 'middle'; first.append(sw, txt('span', '', s.name)); tr.append(first);
    const cell = (v, cls) => tr.append(txt('td', cls || '', v));
    cell(m.sharpe.toFixed(2), m.sharpe < 0 ? 'neg' : ''); cell(fmtPct(m.annual_return * 100), m.annual_return < 0 ? 'neg' : 'pos'); cell(fmtP(m.turnover_daily * 100, 0)); cell(m.fitness.toFixed(2)); cell(m.margin_bps.toFixed(2)); cell(fmtP(m.max_drawdown_pct)); cell(m.mean_ic.toFixed(3)); cell(fmtP(d.win_ratio_bars * 100)); cell(fmtP(m.avg_gross / book * 100, 0)); cell(fmtPct(m.avg_net / book * 100, 1));
    tb.append(tr);
  }
  t.append(tb);
  const mt = document.getElementById('months');
  const months = [...new Set(SERIES.flatMap(s => DATA.runs[s.id].months.map(r => r[0])))].sort();
  const h2 = document.createElement('tr'); h2.append(txt('th', '', '월')); SERIES.forEach(s => h2.append(txt('th', '', s.name))); const th2 = document.createElement('thead'); th2.append(h2); mt.append(th2);
  const tb2 = document.createElement('tbody');
  months.forEach(mo => { const tr = document.createElement('tr'); tr.append(txt('td', '', mo)); SERIES.forEach(s => { const r = DATA.runs[s.id].months.find(x => x[0] === mo); tr.append(txt('td', r && r[3] < 0 ? 'neg' : '', r ? `${r[1] ?? '-'} / ${r[2]} / ${fmtPct(r[3])}` : '-')); }); tb2.append(tr); });
  mt.append(tb2);
}
fetch('data.json').then(r => r.json()).then(render).catch(e => { document.querySelector('main').prepend(txt('p', 'err', '데이터를 불러오지 못했습니다: ' + e.message)); });
</script>
"""


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--title", required=True)
    ap.add_argument("--subtitle", default="")
    ap.add_argument("--note", action="append", default=[], help="bullet for the notes section (repeatable)")
    ap.add_argument("--booksize", type=float, default=1_000_000.0)
    ap.add_argument("--out", required=True)
    ap.add_argument("series", nargs="+", help="label=path or label|short=path (max 8)")
    a = ap.parse_args()
    if len(a.series) > 8:
        raise SystemExit("at most 8 series (palette slots)")
    runs, series = {}, []
    for i, spec in enumerate(a.series):
        label, path = spec.split("=", 1)
        name, _, short = label.partition("|")
        sid = f"s{i + 1}"
        runs[sid] = load_run(path, a.booksize)
        series.append({"id": sid, "name": name, "short": short or name, "slot": i + 1})
    os.makedirs(a.out, exist_ok=True)
    json.dump({"booksize": a.booksize, "series": series, "runs": runs}, open(os.path.join(a.out, "data.json"), "w"), separators=(",", ":"))
    esc = lambda s: s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
    notes = ""
    if a.note:
        notes = '<section class="card"><h2>읽는 법</h2><ul class="notes">' + "".join(f"<li>{esc(n)}</li>" for n in a.note) + "</ul></section>"
    light = " ".join(f"--s{i + 1}: {PALETTE[i][0]};" for i in range(len(series)))
    dark = " ".join(f"--s{i + 1}: {PALETTE[i][1]};" for i in range(len(series)))
    html = (TEMPLATE.replace("__TITLE__", esc(a.title)).replace("__SUBTITLE__", esc(a.subtitle)).replace("__NOTES__", notes)
            .replace("__LIGHT_SLOTS__", light).replace("__DARK_SLOTS__", dark))
    open(os.path.join(a.out, "index.html"), "w").write(html)
    print(f"wrote {a.out}/index.html and data.json ({os.path.getsize(os.path.join(a.out, 'data.json'))} bytes)")


if __name__ == "__main__":
    main()
