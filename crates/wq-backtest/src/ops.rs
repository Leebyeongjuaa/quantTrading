//! Operator library and expression evaluator (BRAIN-style names).
//!
//! * Fields: `open high low close volume dollar_volume returns vwap`, `adv20` / `adv(d)`.
//! * Element-wise: `+ - * / ^`, comparisons, `abs log sqrt exp sign inverse reverse
//!   signed_power power max min if_else add subtract multiply divide`.
//! * Cross-sectional (per bar, NaNs ignored): `rank zscore scale normalize neutralize
//!   winsorize truncate`.
//! * Time-series (per instrument, trailing window of `d` bars): `ts_mean ts_sum ts_std_dev
//!   ts_delta ts_delay ts_returns ts_rank ts_min ts_max ts_arg_max ts_arg_min ts_corr
//!   ts_covariance ts_decay_linear ts_decay_exp_window ts_zscore ts_scale ts_av_diff
//!   ts_product ts_backfill hump`.
//!
//! Window ops need at least `ceil(d/2)` valid points, otherwise NaN.

use anyhow::{anyhow, bail, Result};

use crate::expr::{BinOp, Expr, UnOp};
use crate::mat::Mat;
use crate::panel::Panel;

#[derive(Debug, Clone)]
pub enum Value {
    Scalar(f64),
    Mat(Mat),
}

impl Value {
    fn into_mat(self, t: usize, n: usize) -> Mat {
        match self {
            Value::Scalar(v) => Mat::filled(t, n, v),
            Value::Mat(m) => m,
        }
    }
    fn as_scalar(&self) -> Option<f64> {
        match self {
            Value::Scalar(v) => Some(*v),
            Value::Mat(_) => None,
        }
    }
}

pub struct Evaluator<'a> {
    panel: &'a Panel,
}

pub fn evaluate(panel: &Panel, expr: &Expr) -> Result<Mat> {
    let ev = Evaluator { panel };
    Ok(ev.eval(expr)?.into_mat(panel.t(), panel.n()))
}

/// Operator names accepted by [`evaluate`], for `--help`-style listings.
pub const OPERATORS: &[(&str, &str)] = &[
    (
        "abs(x) log(x) sqrt(x) exp(x) sign(x) inverse(x) reverse(x)",
        "element-wise",
    ),
    (
        "signed_power(x, e) power(x, e) max(a, b, ...) min(a, b, ...)",
        "element-wise",
    ),
    (
        "if_else(cond, a, b) add(a, b) subtract(a, b) multiply(a, b) divide(a, b)",
        "element-wise",
    ),
    ("rank(x)", "cross-sectional rank in [0, 1]"),
    (
        "zscore(x) normalize(x) neutralize(x)",
        "cross-sectional z-score / demean",
    ),
    ("scale(x, scale=1)", "cross-sectional: sum |x| = scale"),
    ("winsorize(x, std=4)", "clip at mean ± std·σ"),
    (
        "truncate(x, max_percent=0.01)",
        "cap |x| at max_percent · sum |x|",
    ),
    (
        "ts_mean(x, d) ts_sum(x, d) ts_std_dev(x, d) ts_product(x, d)",
        "trailing window",
    ),
    (
        "ts_delta(x, d) ts_delay(x, d) ts_returns(x, d) ts_av_diff(x, d)",
        "trailing window",
    ),
    (
        "ts_rank(x, d) ts_min(x, d) ts_max(x, d) ts_arg_max(x, d) ts_arg_min(x, d)",
        "trailing window",
    ),
    ("ts_corr(x, y, d) ts_covariance(x, y, d)", "trailing window"),
    (
        "ts_decay_linear(x, d) ts_decay_exp_window(x, d, factor=0.5)",
        "trailing window",
    ),
    (
        "ts_zscore(x, d) ts_scale(x, d) ts_backfill(x, d)",
        "trailing window",
    ),
    (
        "hump(x, hump=0.01)",
        "ignore changes smaller than hump · mean|x|",
    ),
    ("adv(d) adv20", "trailing mean dollar volume"),
    (
        "top_n(x, n)",
        "1 for the n largest x in the cross-section, else 0",
    ),
    (
        "mean_if(x, cond)",
        "cross-sectional mean of x where cond != 0, broadcast to all",
    ),
    (
        "ts_beta(y, x, d)",
        "rolling regression slope of y on x (cov/var)",
    ),
    (
        "ts_resid(x, d) ts_resid_z(x, d)",
        "last point's residual from the OLS trend line over d bars (z: / residual std)",
    ),
];

fn valid(w: &[f64]) -> Vec<f64> {
    w.iter().copied().filter(|v| v.is_finite()).collect()
}

fn min_count(d: usize) -> usize {
    d.div_ceil(2).max(1)
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

fn std_dev(v: &[f64]) -> f64 {
    if v.len() < 2 {
        return f64::NAN;
    }
    let m = mean(v);
    (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (v.len() - 1) as f64).sqrt()
}

/// Average-rank of each finite value, scaled to [0, 1]; NaN stays NaN.
pub fn rank_row(row: &mut [f64]) {
    let mut idx: Vec<usize> = (0..row.len()).filter(|&i| row[i].is_finite()).collect();
    let n = idx.len();
    if n == 0 {
        return;
    }
    if n == 1 {
        row[idx[0]] = 0.5;
        return;
    }
    idx.sort_by(|&a, &b| row[a].partial_cmp(&row[b]).unwrap());
    let vals: Vec<f64> = idx.iter().map(|&i| row[i]).collect();
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j + 1 < n && vals[j + 1] == vals[i] {
            j += 1;
        }
        let avg_rank = (i + j) as f64 / 2.0; // zero-based average rank
        for k in i..=j {
            row[idx[k]] = avg_rank / (n - 1) as f64;
        }
        i = j + 1;
    }
}

fn ts_apply(x: &Mat, d: usize, f: impl Fn(&[f64]) -> f64) -> Mat {
    let mut out = Mat::nan(x.t, x.n);
    if d == 0 {
        return out;
    }
    for i in 0..x.n {
        let col = x.column(i);
        for t in 0..x.t {
            if t + 1 < d {
                continue;
            }
            out.set(t, i, f(&col[t + 1 - d..=t]));
        }
    }
    out
}

fn ts_apply2(x: &Mat, y: &Mat, d: usize, f: impl Fn(&[f64], &[f64]) -> f64) -> Mat {
    let mut out = Mat::nan(x.t, x.n);
    if d == 0 {
        return out;
    }
    for i in 0..x.n {
        let cx = x.column(i);
        let cy = y.column(i);
        for t in 0..x.t {
            if t + 1 < d {
                continue;
            }
            out.set(t, i, f(&cx[t + 1 - d..=t], &cy[t + 1 - d..=t]));
        }
    }
    out
}

fn windowed(d: usize, f: impl Fn(&[f64]) -> f64) -> impl Fn(&[f64]) -> f64 {
    let need = min_count(d);
    move |w: &[f64]| {
        let v = valid(w);
        if v.len() < need {
            f64::NAN
        } else {
            f(&v)
        }
    }
}

pub fn ts_decay_linear(x: &Mat, d: usize) -> Mat {
    ts_apply(x, d, |w| {
        let mut num = 0.0;
        let mut den = 0.0;
        let mut cnt = 0;
        for (k, v) in w.iter().enumerate() {
            if v.is_finite() {
                let wgt = (k + 1) as f64;
                num += wgt * v;
                den += wgt;
                cnt += 1;
            }
        }
        if cnt < min_count(d) || den == 0.0 {
            f64::NAN
        } else {
            num / den
        }
    })
}

fn pairs(x: &[f64], y: &[f64]) -> (Vec<f64>, Vec<f64>) {
    let mut a = Vec::new();
    let mut b = Vec::new();
    for (p, q) in x.iter().zip(y) {
        if p.is_finite() && q.is_finite() {
            a.push(*p);
            b.push(*q);
        }
    }
    (a, b)
}

fn covariance(a: &[f64], b: &[f64]) -> f64 {
    let (ma, mb) = (mean(a), mean(b));
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - ma) * (y - mb))
        .sum::<f64>()
        / (a.len() - 1) as f64
}

impl<'a> Evaluator<'a> {
    fn shape(&self) -> (usize, usize) {
        (self.panel.t(), self.panel.n())
    }

    fn eval(&self, e: &Expr) -> Result<Value> {
        match e {
            Expr::Num(v) => Ok(Value::Scalar(*v)),
            Expr::Ident(name) => self.field(name),
            Expr::Unary(op, inner) => {
                let v = self.eval(inner)?;
                Ok(self.map1(v, |x| match op {
                    UnOp::Neg => -x,
                    UnOp::Not => {
                        if x.is_nan() {
                            f64::NAN
                        } else if x != 0.0 {
                            0.0
                        } else {
                            1.0
                        }
                    }
                }))
            }
            Expr::Binary(op, a, b) => {
                let (a, b) = (self.eval(a)?, self.eval(b)?);
                let f = |x: f64, y: f64| -> f64 {
                    if x.is_nan() || y.is_nan() {
                        return f64::NAN;
                    }
                    match op {
                        BinOp::Add => x + y,
                        BinOp::Sub => x - y,
                        BinOp::Mul => x * y,
                        BinOp::Div => {
                            if y == 0.0 {
                                f64::NAN
                            } else {
                                x / y
                            }
                        }
                        BinOp::Pow => x.powf(y),
                        BinOp::Lt => (x < y) as u8 as f64,
                        BinOp::Le => (x <= y) as u8 as f64,
                        BinOp::Gt => (x > y) as u8 as f64,
                        BinOp::Ge => (x >= y) as u8 as f64,
                        BinOp::Eq => (x == y) as u8 as f64,
                        BinOp::Ne => (x != y) as u8 as f64,
                        BinOp::And => ((x != 0.0) && (y != 0.0)) as u8 as f64,
                        BinOp::Or => ((x != 0.0) || (y != 0.0)) as u8 as f64,
                    }
                };
                Ok(self.map2(a, b, f))
            }
            Expr::Ternary(c, a, b) => self.if_else(self.eval(c)?, self.eval(a)?, self.eval(b)?),
            Expr::Call { name, args, kwargs } => self.call(name, args, kwargs),
        }
    }

    fn field(&self, name: &str) -> Result<Value> {
        if let Some(m) = self.panel.field(name) {
            return Ok(Value::Mat(m.clone()));
        }
        if let Some(d) = name
            .strip_prefix("adv")
            .and_then(|s| s.parse::<usize>().ok())
        {
            return Ok(Value::Mat(ts_apply(
                &self.panel.dollar_volume,
                d,
                windowed(d, mean),
            )));
        }
        bail!(
            "unknown field or variable `{name}` (fields: {})",
            Panel::FIELDS.join(" ")
        )
    }

    fn map1(&self, v: Value, f: impl Fn(f64) -> f64) -> Value {
        match v {
            Value::Scalar(x) => Value::Scalar(f(x)),
            Value::Mat(m) => Value::Mat(m.map(f)),
        }
    }

    fn map2(&self, a: Value, b: Value, f: impl Fn(f64, f64) -> f64) -> Value {
        match (a, b) {
            (Value::Scalar(x), Value::Scalar(y)) => Value::Scalar(f(x, y)),
            (a, b) => {
                let (t, n) = self.shape();
                Value::Mat(a.into_mat(t, n).zip(&b.into_mat(t, n), f))
            }
        }
    }

    fn if_else(&self, c: Value, a: Value, b: Value) -> Result<Value> {
        let (t, n) = self.shape();
        let (c, a, b) = (c.into_mat(t, n), a.into_mat(t, n), b.into_mat(t, n));
        let mut out = Mat::nan(t, n);
        for i in 0..t * n {
            let (ti, ni) = (i / n, i % n);
            let cv = c.get(ti, ni);
            out.set(
                ti,
                ni,
                if cv.is_nan() {
                    f64::NAN
                } else if cv != 0.0 {
                    a.get(ti, ni)
                } else {
                    b.get(ti, ni)
                },
            );
        }
        Ok(Value::Mat(out))
    }

    fn arg_mat(&self, name: &str, args: &[Expr], i: usize) -> Result<Mat> {
        let (t, n) = self.shape();
        let e = args
            .get(i)
            .ok_or_else(|| anyhow!("{name}: missing argument {}", i + 1))?;
        Ok(self.eval(e)?.into_mat(t, n))
    }

    fn arg_num(
        &self,
        name: &str,
        args: &[Expr],
        kwargs: &[(String, Expr)],
        i: usize,
        key: &str,
        default: Option<f64>,
    ) -> Result<f64> {
        let e = kwargs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
            .or_else(|| args.get(i));
        match e {
            Some(e) => self
                .eval(e)?
                .as_scalar()
                .ok_or_else(|| anyhow!("{name}: `{key}` must be a constant")),
            None => default.ok_or_else(|| anyhow!("{name}: missing `{key}`")),
        }
    }

    fn arg_window(
        &self,
        name: &str,
        args: &[Expr],
        kwargs: &[(String, Expr)],
        i: usize,
    ) -> Result<usize> {
        let d = self.arg_num(name, args, kwargs, i, "d", None)?;
        if d < 1.0 || d.fract() != 0.0 {
            bail!("{name}: window must be a positive integer, got {d}");
        }
        Ok(d as usize)
    }

    fn call(&self, name: &str, args: &[Expr], kwargs: &[(String, Expr)]) -> Result<Value> {
        let (t, n) = self.shape();
        let x = |i: usize| self.arg_mat(name, args, i);
        let m = match name {
            // ---- fields with a parameter
            "adv" => {
                let d = self.arg_window(name, args, kwargs, 0)?;
                ts_apply(&self.panel.dollar_volume, d, windowed(d, mean))
            }
            // ---- element-wise
            "abs" => x(0)?.map(f64::abs),
            "log" => x(0)?.map(|v| if v > 0.0 { v.ln() } else { f64::NAN }),
            "sqrt" => x(0)?.map(|v| if v >= 0.0 { v.sqrt() } else { f64::NAN }),
            "exp" => x(0)?.map(f64::exp),
            "sign" => x(0)?.map(|v| {
                if v.is_nan() {
                    v
                } else if v > 0.0 {
                    1.0
                } else if v < 0.0 {
                    -1.0
                } else {
                    0.0
                }
            }),
            "inverse" => x(0)?.map(|v| if v == 0.0 { f64::NAN } else { 1.0 / v }),
            "reverse" => x(0)?.map(|v| -v),
            "signed_power" => {
                let e = self.arg_num(name, args, kwargs, 1, "e", None)?;
                x(0)?.map(|v| v.signum() * v.abs().powf(e))
            }
            "power" => {
                let e = self.arg_num(name, args, kwargs, 1, "e", None)?;
                x(0)?.map(|v| v.powf(e))
            }
            "max" | "min" => {
                if args.is_empty() {
                    bail!("{name}: needs at least one argument");
                }
                let mut acc = x(0)?;
                for i in 1..args.len() {
                    let other = x(i)?;
                    acc = acc.zip(&other, |a, b| {
                        if a.is_nan() || b.is_nan() {
                            f64::NAN
                        } else if name == "max" {
                            a.max(b)
                        } else {
                            a.min(b)
                        }
                    });
                }
                acc
            }
            "if_else" => {
                return self.if_else(
                    self.eval(&args[0])?,
                    self.eval(&args[1])?,
                    self.eval(&args[2])?,
                )
            }
            "add" => x(0)?.zip(&x(1)?, |a, b| a + b),
            "subtract" => x(0)?.zip(&x(1)?, |a, b| a - b),
            "multiply" => x(0)?.zip(&x(1)?, |a, b| a * b),
            "divide" => x(0)?.zip(&x(1)?, |a, b| if b == 0.0 { f64::NAN } else { a / b }),

            // ---- grouping helpers
            "top_n" => {
                let k = self.arg_num(name, args, kwargs, 1, "n", None)?;
                if k < 1.0 {
                    bail!("top_n: n must be >= 1");
                }
                let mut m = x(0)?;
                m.for_each_row(|row| {
                    let mut idx: Vec<usize> =
                        (0..row.len()).filter(|&i| row[i].is_finite()).collect();
                    idx.sort_by(|&a, &b| row[b].partial_cmp(&row[a]).unwrap());
                    let top: std::collections::HashSet<usize> =
                        idx.iter().take(k as usize).copied().collect();
                    for (i, v) in row.iter_mut().enumerate() {
                        if v.is_finite() {
                            *v = if top.contains(&i) { 1.0 } else { 0.0 };
                        }
                    }
                });
                m
            }
            "mean_if" => {
                let m = x(0)?;
                let c = x(1)?;
                let mut out = Mat::nan(t, n);
                for ti in 0..t {
                    let (mut sum, mut cnt) = (0.0, 0usize);
                    for i in 0..n {
                        let v = m.get(ti, i);
                        let cv = c.get(ti, i);
                        if v.is_finite() && cv.is_finite() && cv != 0.0 {
                            sum += v;
                            cnt += 1;
                        }
                    }
                    let mu = if cnt > 0 { sum / cnt as f64 } else { f64::NAN };
                    for i in 0..n {
                        out.set(ti, i, mu);
                    }
                }
                out
            }
            "ts_resid" | "ts_resid_z" => {
                let d = self.arg_window(name, args, kwargs, 1)?;
                let zscore = name == "ts_resid_z";
                ts_apply(&x(0)?, d, move |w| {
                    // OLS of x on bar index over the window; residual of the newest point.
                    let pts: Vec<(f64, f64)> = w
                        .iter()
                        .enumerate()
                        .filter(|(_, v)| v.is_finite())
                        .map(|(i, v)| (i as f64, *v))
                        .collect();
                    if pts.len() < min_count(d).max(3) || !w[w.len() - 1].is_finite() {
                        return f64::NAN;
                    }
                    let n = pts.len() as f64;
                    let mx = pts.iter().map(|p| p.0).sum::<f64>() / n;
                    let my = pts.iter().map(|p| p.1).sum::<f64>() / n;
                    let sxx: f64 = pts.iter().map(|p| (p.0 - mx).powi(2)).sum();
                    let sxy: f64 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
                    if sxx <= 0.0 {
                        return f64::NAN;
                    }
                    let slope = sxy / sxx;
                    let fit = |t: f64| my + slope * (t - mx);
                    let last_t = (w.len() - 1) as f64;
                    let resid = w[w.len() - 1] - fit(last_t);
                    if !zscore {
                        return resid;
                    }
                    let var: f64 =
                        pts.iter().map(|p| (p.1 - fit(p.0)).powi(2)).sum::<f64>() / (n - 2.0);
                    if var > 0.0 {
                        resid / var.sqrt()
                    } else {
                        f64::NAN
                    }
                })
            }
            "ts_beta" => {
                let d = self.arg_window(name, args, kwargs, 2)?;
                ts_apply2(&x(0)?, &x(1)?, d, move |wy, wx| {
                    let (b, a) = pairs(wy, wx);
                    if a.len() < min_count(d).max(3) {
                        return f64::NAN;
                    }
                    let m = mean(&a);
                    let var = a.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (a.len() - 1) as f64;
                    if var > 0.0 {
                        covariance(&b, &a) / var
                    } else {
                        f64::NAN
                    }
                })
            }
            // ---- cross-sectional
            "rank" => {
                let mut m = x(0)?;
                m.for_each_row(rank_row);
                m
            }
            "zscore" => {
                let mut m = x(0)?;
                m.for_each_row(|row| {
                    let v = valid(row);
                    let (mu, sd) = (mean(&v), std_dev(&v));
                    for r in row.iter_mut() {
                        if r.is_finite() {
                            *r = if sd > 0.0 { (*r - mu) / sd } else { 0.0 };
                        }
                    }
                });
                m
            }
            "normalize" | "neutralize" => {
                let mut m = x(0)?;
                m.for_each_row(|row| {
                    let v = valid(row);
                    if v.is_empty() {
                        return;
                    }
                    let mu = mean(&v);
                    for r in row.iter_mut() {
                        if r.is_finite() {
                            *r -= mu;
                        }
                    }
                });
                m
            }
            "scale" => {
                let target = self.arg_num(name, args, kwargs, 1, "scale", Some(1.0))?;
                let mut m = x(0)?;
                m.for_each_row(|row| scale_row(row, target));
                m
            }
            "winsorize" => {
                let k = self.arg_num(name, args, kwargs, 1, "std", Some(4.0))?;
                let mut m = x(0)?;
                m.for_each_row(|row| {
                    let v = valid(row);
                    let (mu, sd) = (mean(&v), std_dev(&v));
                    if sd <= 0.0 || sd.is_nan() {
                        return;
                    }
                    for r in row.iter_mut() {
                        if r.is_finite() {
                            *r = r.clamp(mu - k * sd, mu + k * sd);
                        }
                    }
                });
                m
            }
            "truncate" => {
                let pct = self.arg_num(name, args, kwargs, 1, "max_percent", Some(0.01))?;
                let mut m = x(0)?;
                m.for_each_row(|row| truncate_row(row, pct));
                m
            }

            // ---- time-series
            "ts_mean" => {
                let d = self.arg_window(name, args, kwargs, 1)?;
                ts_apply(&x(0)?, d, windowed(d, mean))
            }
            "ts_sum" => {
                let d = self.arg_window(name, args, kwargs, 1)?;
                ts_apply(&x(0)?, d, windowed(d, |v| v.iter().sum()))
            }
            "ts_product" => {
                let d = self.arg_window(name, args, kwargs, 1)?;
                ts_apply(&x(0)?, d, windowed(d, |v| v.iter().product()))
            }
            "ts_std_dev" => {
                let d = self.arg_window(name, args, kwargs, 1)?;
                ts_apply(&x(0)?, d, windowed(d, std_dev))
            }
            "ts_min" => {
                let d = self.arg_window(name, args, kwargs, 1)?;
                ts_apply(
                    &x(0)?,
                    d,
                    windowed(d, |v| v.iter().cloned().fold(f64::INFINITY, f64::min)),
                )
            }
            "ts_max" => {
                let d = self.arg_window(name, args, kwargs, 1)?;
                ts_apply(
                    &x(0)?,
                    d,
                    windowed(d, |v| v.iter().cloned().fold(f64::NEG_INFINITY, f64::max)),
                )
            }
            "ts_arg_max" | "ts_arg_min" => {
                let d = self.arg_window(name, args, kwargs, 1)?;
                let want_max = name == "ts_arg_max";
                // Bars since the extreme (0 = current bar), as in BRAIN.
                ts_apply(&x(0)?, d, move |w| {
                    let mut best: Option<(usize, f64)> = None;
                    for (k, v) in w.iter().enumerate() {
                        if !v.is_finite() {
                            continue;
                        }
                        let better = match best {
                            None => true,
                            Some((_, b)) => {
                                if want_max {
                                    *v > b
                                } else {
                                    *v < b
                                }
                            }
                        };
                        if better {
                            best = Some((k, *v));
                        }
                    }
                    match best {
                        Some((k, _)) if valid(w).len() >= min_count(d) => (w.len() - 1 - k) as f64,
                        _ => f64::NAN,
                    }
                })
            }
            "ts_delay" => {
                let d = self.arg_window(name, args, kwargs, 1)?;
                ts_apply(&x(0)?, d + 1, |w| w[0])
            }
            "ts_delta" => {
                let d = self.arg_window(name, args, kwargs, 1)?;
                ts_apply(&x(0)?, d + 1, |w| w[w.len() - 1] - w[0])
            }
            "ts_returns" => {
                let d = self.arg_window(name, args, kwargs, 1)?;
                ts_apply(&x(0)?, d + 1, |w| {
                    if w[0] == 0.0 {
                        f64::NAN
                    } else {
                        w[w.len() - 1] / w[0] - 1.0
                    }
                })
            }
            "ts_av_diff" => {
                let d = self.arg_window(name, args, kwargs, 1)?;
                ts_apply(&x(0)?, d, windowed(d, |v| v[v.len() - 1] - mean(v)))
            }
            "ts_rank" => {
                let d = self.arg_window(name, args, kwargs, 1)?;
                ts_apply(&x(0)?, d, move |w| {
                    let cur = w[w.len() - 1];
                    let v = valid(w);
                    if !cur.is_finite() || v.len() < min_count(d) {
                        return f64::NAN;
                    }
                    if v.len() == 1 {
                        return 0.5;
                    }
                    let less = v.iter().filter(|&&a| a < cur).count() as f64;
                    let eq = v.iter().filter(|&&a| a == cur).count() as f64;
                    (less + 0.5 * (eq - 1.0)) / (v.len() - 1) as f64
                })
            }
            "ts_zscore" => {
                let d = self.arg_window(name, args, kwargs, 1)?;
                ts_apply(
                    &x(0)?,
                    d,
                    windowed(d, |v| {
                        let sd = std_dev(v);
                        if sd > 0.0 {
                            (v[v.len() - 1] - mean(v)) / sd
                        } else {
                            0.0
                        }
                    }),
                )
            }
            "ts_scale" => {
                let d = self.arg_window(name, args, kwargs, 1)?;
                ts_apply(
                    &x(0)?,
                    d,
                    windowed(d, |v| {
                        let lo = v.iter().cloned().fold(f64::INFINITY, f64::min);
                        let hi = v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                        if hi > lo {
                            (v[v.len() - 1] - lo) / (hi - lo)
                        } else {
                            0.5
                        }
                    }),
                )
            }
            "ts_backfill" => {
                let d = self.arg_window(name, args, kwargs, 1)?;
                ts_apply(&x(0)?, d, |w| {
                    w.iter()
                        .rev()
                        .cloned()
                        .find(|v| v.is_finite())
                        .unwrap_or(f64::NAN)
                })
            }
            "ts_decay_linear" => {
                let d = self.arg_window(name, args, kwargs, 1)?;
                ts_decay_linear(&x(0)?, d)
            }
            "ts_decay_exp_window" => {
                let d = self.arg_window(name, args, kwargs, 1)?;
                let factor = self.arg_num(name, args, kwargs, 2, "factor", Some(0.5))?;
                ts_apply(&x(0)?, d, move |w| {
                    let (mut num, mut den, mut cnt) = (0.0, 0.0, 0);
                    for (k, v) in w.iter().enumerate() {
                        if v.is_finite() {
                            let wgt = factor.powi((w.len() - 1 - k) as i32);
                            num += wgt * v;
                            den += wgt;
                            cnt += 1;
                        }
                    }
                    if cnt < min_count(d) || den == 0.0 {
                        f64::NAN
                    } else {
                        num / den
                    }
                })
            }
            "ts_corr" | "ts_covariance" => {
                let d = self.arg_window(name, args, kwargs, 2)?;
                let corr = name == "ts_corr";
                ts_apply2(&x(0)?, &x(1)?, d, move |wx, wy| {
                    let (a, b) = pairs(wx, wy);
                    if a.len() < min_count(d).max(2) {
                        return f64::NAN;
                    }
                    let cov = covariance(&a, &b);
                    if corr {
                        let den = std_dev(&a) * std_dev(&b);
                        if den > 0.0 {
                            cov / den
                        } else {
                            f64::NAN
                        }
                    } else {
                        cov
                    }
                })
            }
            "hump" => {
                let h = self.arg_num(name, args, kwargs, 1, "hump", Some(0.01))?;
                hump(&x(0)?, h)
            }
            _ => bail!("unknown operator `{name}`"),
        };
        debug_assert_eq!((m.t, m.n), (t, n));
        Ok(Value::Mat(m))
    }
}

/// Rescales finite entries so that `sum |x| = target`.
pub fn scale_row(row: &mut [f64], target: f64) {
    let s: f64 = row.iter().filter(|v| v.is_finite()).map(|v| v.abs()).sum();
    if s > 0.0 {
        for r in row.iter_mut() {
            if r.is_finite() {
                *r *= target / s;
            }
        }
    }
}

/// Caps `|x_i|` at `max_percent · sum|x|` while keeping the original gross `sum|x|`.
/// Exact water-filling: the `k` largest entries sit at the cap and the rest are scaled up so
/// the total is unchanged; `k` is the smallest count for which no remaining entry exceeds the
/// cap. With `max_percent >= 1` (or too small to be feasible) nothing changes.
pub fn truncate_row(row: &mut [f64], max_percent: f64) {
    if max_percent <= 0.0 || max_percent >= 1.0 {
        return;
    }
    let mut idx: Vec<usize> = (0..row.len())
        .filter(|&i| row[i].is_finite() && row[i] != 0.0)
        .collect();
    let gross: f64 = idx.iter().map(|&i| row[i].abs()).sum();
    if gross <= 0.0 || (idx.len() as f64) * max_percent < 1.0 {
        return; // infeasible: fewer than 1/max_percent names
    }
    let cap = max_percent * gross;
    idx.sort_by(|&a, &b| row[b].abs().partial_cmp(&row[a].abs()).unwrap());
    let abs: Vec<f64> = idx.iter().map(|&i| row[i].abs()).collect();
    let mut rest_sum = gross;
    for k in 0..=abs.len() {
        // k largest clipped to cap; the rest scaled by f so the total equals gross.
        let f = if k < abs.len() && rest_sum > 0.0 {
            (gross - k as f64 * cap) / rest_sum
        } else {
            0.0
        };
        let largest_rest = abs.get(k).copied().unwrap_or(0.0) * f;
        if k == abs.len() || largest_rest <= cap * (1.0 + 1e-12) {
            for (j, &i) in idx.iter().enumerate() {
                let mag = if j < k { cap } else { abs[j] * f };
                row[i] = row[i].signum() * mag;
            }
            return;
        }
        rest_sum -= abs[k];
    }
}

/// Keeps the previous value unless the input moved by more than `hump · mean|x_t|`.
pub fn hump(x: &Mat, hump: f64) -> Mat {
    let mut out = x.clone();
    for t in 1..x.t {
        let row = x.row(t);
        let v = valid(row);
        if v.is_empty() {
            continue;
        }
        let thresh = hump * v.iter().map(|a| a.abs()).sum::<f64>() / v.len() as f64;
        for i in 0..x.n {
            let prev = out.get(t - 1, i);
            let cur = x.get(t, i);
            if prev.is_finite() && cur.is_finite() && (cur - prev).abs() <= thresh {
                out.set(t, i, prev);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::parse;
    use crate::panel::tests::bars;

    fn panel() -> Panel {
        Panel::from_series(vec![
            ("A".into(), bars(0, &[10.0, 11.0, 12.0, 11.0, 13.0, 14.0])),
            ("B".into(), bars(0, &[50.0, 49.0, 48.0, 50.0, 47.0, 46.0])),
            ("C".into(), bars(0, &[5.0, 5.0, 5.5, 6.0, 6.0, 7.0])),
        ])
        .unwrap()
    }

    fn ev(src: &str) -> Mat {
        let p = panel();
        evaluate(&p, &parse(src).unwrap()).unwrap()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn rank_with_ties_and_nan() {
        let mut row = vec![3.0, f64::NAN, 1.0, 3.0, 2.0];
        rank_row(&mut row);
        assert!(row[1].is_nan());
        assert!(close(row[2], 0.0));
        assert!(close(row[4], 1.0 / 3.0));
        assert!(close(row[0], (2.0 + 3.0) / 2.0 / 3.0));
        assert!(close(row[3], row[0]));
    }

    #[test]
    fn arithmetic_and_fields() {
        let m = ev("close * 2 - open");
        assert!(close(m.get(0, 0), 10.0));
        let m = ev("-rank(close)");
        assert!(close(m.get(0, 1), -1.0));
        assert!(close(m.get(0, 2), 0.0));
        let m = ev("close > 10 ? 1 : 0");
        assert_eq!(m.row(0), &[0.0, 1.0, 0.0]);
        assert_eq!(
            ev("if_else(close >= 11 && close < 13, 1, 0)").row(2),
            &[1.0, 0.0, 0.0]
        );
    }

    #[test]
    fn time_series_ops() {
        let m = ev("ts_delta(close, 1)");
        assert!(m.get(0, 0).is_nan());
        assert!(close(m.get(1, 0), 1.0));
        let m = ev("ts_delay(close, 2)");
        assert!(close(m.get(2, 0), 10.0));
        let m = ev("ts_mean(close, 3)");
        assert!(m.get(1, 0).is_nan());
        assert!(close(m.get(2, 0), 11.0));
        let m = ev("ts_rank(close, 3)");
        assert!(close(m.get(2, 0), 1.0)); // 12 is the max of [10,11,12]
    }

    #[test]
    fn ts_rank_ties() {
        let m = ev("ts_rank(close, 3)");
        // window [11,12,11] for A at t=3: less=0, eq=2 -> (0 + 0.5*1)/2 = 0.25
        assert!(close(m.get(3, 0), 0.25));
    }

    #[test]
    fn decay_and_corr() {
        let m = ev("ts_decay_linear(close, 3)");
        // weights 1,2,3 over [10,11,12] -> (10+22+36)/6 = 11.333..
        assert!(close(m.get(2, 0), 68.0 / 6.0));
        let m = ev("ts_corr(close, ts_delay(close, 0 + 1), 4)");
        assert!(m.get(3, 0).is_finite());
        let m = ev("ts_corr(close, close, 3)");
        assert!(close(m.get(2, 0), 1.0));
        let m = ev("ts_arg_max(close, 3)");
        assert!(close(m.get(3, 0), 1.0)); // window [11,12,11]: max at k=1 -> 1 bar ago
    }

    #[test]
    fn cross_sectional_scaling() {
        let m = ev("scale(normalize(close))");
        let row = m.row(0);
        assert!(close(row.iter().sum::<f64>(), 0.0));
        assert!(close(row.iter().map(|v| v.abs()).sum::<f64>(), 1.0));
        let m = ev("zscore(close)");
        assert!(close(m.row(0).iter().sum::<f64>(), 0.0));
        let mut r = vec![0.8, 0.1, 0.1];
        truncate_row(&mut r, 0.5);
        assert!(close(r[0], 0.5) && close(r[1], 0.25) && close(r[2], 0.25));
        let mut r = vec![-0.6, 0.3, 0.05, 0.05];
        truncate_row(&mut r, 0.3);
        assert!(r.iter().all(|v| v.abs() <= 0.3 + 1e-9));
        assert!(close(r.iter().map(|v| v.abs()).sum::<f64>(), 1.0));
        assert!(close(r[0], -0.3) && close(r[1], 0.3) && close(r[2], 0.2));
        let mut r = vec![0.9, 0.1];
        truncate_row(&mut r, 0.3); // infeasible with 2 names: unchanged
        assert!(close(r[0], 0.9));
    }

    #[test]
    fn hump_suppresses_small_changes() {
        let x = Mat::from_rows(vec![vec![1.0, -1.0], vec![1.001, -1.0], vec![2.0, -1.0]]);
        let h = hump(&x, 0.01);
        assert!(close(h.get(1, 0), 1.0));
        assert!(close(h.get(2, 0), 2.0));
    }

    #[test]
    fn linear_trend_residual() {
        let p = Panel::from_series(vec![(
            "L".into(),
            bars(0, &[10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 17.0]),
        )])
        .unwrap();
        // Perfect line over the first six bars: residual 0.
        let m = evaluate(&p, &parse("ts_resid(close, 6)").unwrap()).unwrap();
        assert!(close(m.get(5, 0), 0.0));
        // Window [11..15,17]: the last point sits above the OLS line (resid ~0.48).
        let r = m.get(6, 0);
        assert!(r > 0.3 && r < 1.0, "resid {r}");
        let z = evaluate(&p, &parse("ts_resid_z(close, 6)").unwrap()).unwrap();
        assert!(z.get(6, 0) > 1.0);
        assert!(m.get(4, 0).is_nan());
    }

    #[test]
    fn grouping_helpers() {
        let m = ev("top_n(close, 1)");
        assert_eq!(m.row(0), &[0.0, 1.0, 0.0]); // B has the highest close
        let m = ev("mean_if(close, top_n(close, 2))");
        assert!(close(m.get(0, 0), (10.0 + 50.0) / 2.0));
        assert!(close(m.get(0, 2), (10.0 + 50.0) / 2.0));
        let m = ev("mean_if(close, !top_n(close, 2))");
        assert!(close(m.get(0, 1), 5.0));
        // y = 2x exactly -> beta 2
        let m = ev("ts_beta(close * 2, close, 4)");
        assert!(close(m.get(3, 0), 2.0));
        assert!(m.get(2, 0).is_nan());
    }

    #[test]
    fn adv_and_errors() {
        let m = ev("adv2");
        assert!(close(m.get(1, 0), (100.0 + 110.0) / 2.0));
        assert!(close(ev("adv(2)").get(1, 0), m.get(1, 0)));
        let p = panel();
        assert!(evaluate(&p, &parse("foo(close)").unwrap()).is_err());
        assert!(evaluate(&p, &parse("ts_mean(close, close)").unwrap()).is_err());
        assert!(evaluate(&p, &parse("nope").unwrap()).is_err());
    }
}
