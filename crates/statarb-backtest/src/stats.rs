//! Numerics: OLS, rolling moments, Engle-Granger style ADF t-statistic, OU half-life.

pub fn mean(x: &[f64]) -> f64 {
    if x.is_empty() {
        return f64::NAN;
    }
    x.iter().sum::<f64>() / x.len() as f64
}

/// Sample standard deviation (n-1).
pub fn std(x: &[f64]) -> f64 {
    if x.len() < 2 {
        return f64::NAN;
    }
    let m = mean(x);
    (x.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (x.len() - 1) as f64).sqrt()
}

/// Ordinary least squares `y = alpha + beta * x`. Returns `(alpha, beta)`.
pub fn ols(y: &[f64], x: &[f64]) -> Option<(f64, f64)> {
    let n = y.len().min(x.len());
    if n < 3 {
        return None;
    }
    let (mx, my) = (mean(&x[..n]), mean(&y[..n]));
    let mut sxx = 0.0;
    let mut sxy = 0.0;
    for i in 0..n {
        let dx = x[i] - mx;
        sxx += dx * dx;
        sxy += dx * (y[i] - my);
    }
    if sxx <= 0.0 {
        return None;
    }
    let beta = sxy / sxx;
    Some((my - beta * mx, beta))
}

pub fn pearson(x: &[f64], y: &[f64]) -> f64 {
    let n = x.len().min(y.len());
    if n < 3 {
        return f64::NAN;
    }
    let (mx, my) = (mean(&x[..n]), mean(&y[..n]));
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let (dx, dy) = (x[i] - mx, y[i] - my);
        sxy += dx * dy;
        sxx += dx * dx;
        syy += dy * dy;
    }
    sxy / (sxx * syy).sqrt()
}

/// Multiple-regression helper: solves least squares for small `k` via normal equations with
/// Gaussian elimination. Returns coefficients and the standard error of each coefficient.
fn ols_multi(y: &[f64], xs: &[Vec<f64>]) -> Option<(Vec<f64>, Vec<f64>)> {
    let n = y.len();
    let k = xs.len();
    if n <= k + 1 || xs.iter().any(|c| c.len() != n) {
        return None;
    }
    // Normal matrix (X'X) and X'y.
    let mut a = vec![vec![0.0; k]; k];
    let mut b = vec![0.0; k];
    for i in 0..n {
        for p in 0..k {
            b[p] += xs[p][i] * y[i];
            for q in 0..k {
                a[p][q] += xs[p][i] * xs[q][i];
            }
        }
    }
    let inv = invert(&a)?;
    let coef: Vec<f64> = (0..k)
        .map(|p| (0..k).map(|q| inv[p][q] * b[q]).sum())
        .collect();
    let mut rss = 0.0;
    for i in 0..n {
        let fit: f64 = (0..k).map(|p| coef[p] * xs[p][i]).sum();
        rss += (y[i] - fit).powi(2);
    }
    let sigma2 = rss / (n - k) as f64;
    let se: Vec<f64> = (0..k).map(|p| (sigma2 * inv[p][p]).sqrt()).collect();
    Some((coef, se))
}

fn invert(m: &[Vec<f64>]) -> Option<Vec<Vec<f64>>> {
    let n = m.len();
    let mut a: Vec<Vec<f64>> = m
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let mut r = row.clone();
            r.extend((0..n).map(|j| if i == j { 1.0 } else { 0.0 }));
            r
        })
        .collect();
    for col in 0..n {
        let pivot =
            (col..n).max_by(|&i, &j| a[i][col].abs().partial_cmp(&a[j][col].abs()).unwrap())?;
        if a[pivot][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        let p = a[col][col];
        for v in a[col].iter_mut() {
            *v /= p;
        }
        for r in 0..n {
            if r != col {
                let f = a[r][col];
                if f != 0.0 {
                    let pivot_row = a[col].clone();
                    for (dst, src) in a[r].iter_mut().zip(&pivot_row) {
                        *dst -= f * src;
                    }
                }
            }
        }
    }
    Some(a.into_iter().map(|r| r[n..].to_vec()).collect())
}

/// Augmented Dickey-Fuller t-statistic (constant, `lags` lagged differences) for the null of
/// a unit root in `x`. More negative = more stationary. Approximate 1%/5%/10% critical values
/// for n>100 are about -3.43 / -2.86 / -2.57 (constant, no trend).
pub fn adf_tstat(x: &[f64], lags: usize) -> Option<f64> {
    let n = x.len();
    if n < lags + 10 {
        return None;
    }
    let dx: Vec<f64> = x.windows(2).map(|w| w[1] - w[0]).collect();
    let start = lags;
    let m = dx.len() - start;
    let y: Vec<f64> = dx[start..].to_vec();
    let mut cols: Vec<Vec<f64>> = Vec::with_capacity(lags + 2);
    cols.push(vec![1.0; m]);
    cols.push((0..m).map(|i| x[start + i]).collect()); // x_{t-1}
    for l in 1..=lags {
        cols.push((0..m).map(|i| dx[start + i - l]).collect());
    }
    let (coef, se) = ols_multi(&y, &cols)?;
    if se[1] <= 0.0 || !se[1].is_finite() {
        return None;
    }
    Some(coef[1] / se[1])
}

pub const ADF_CRIT_1PCT: f64 = -3.43;
pub const ADF_CRIT_5PCT: f64 = -2.86;
pub const ADF_CRIT_10PCT: f64 = -2.57;

/// Mean-reversion half-life in bars from the AR(1) fit `dx_t = a + b * x_{t-1}`:
/// `-ln(2) / ln(1 + b)`. `None` if the series does not mean-revert (b >= 0).
pub fn half_life(x: &[f64]) -> Option<f64> {
    if x.len() < 10 {
        return None;
    }
    let lag: Vec<f64> = x[..x.len() - 1].to_vec();
    let dx: Vec<f64> = x.windows(2).map(|w| w[1] - w[0]).collect();
    let (_, b) = ols(&dx, &lag)?;
    if b >= 0.0 || b <= -1.0 {
        return None;
    }
    Some(-(2f64).ln() / (1.0 + b).ln())
}

/// Rolling mean/std over a trailing window, computed incrementally. Entry `i` covers
/// `x[i+1-window ..= i]`; entries before the window fills are NaN.
pub fn rolling_mean_std(x: &[f64], window: usize) -> (Vec<f64>, Vec<f64>) {
    let n = x.len();
    let mut means = vec![f64::NAN; n];
    let mut stds = vec![f64::NAN; n];
    if window < 2 || n < window {
        return (means, stds);
    }
    let mut sum = 0.0;
    let mut sumsq = 0.0;
    for i in 0..n {
        sum += x[i];
        sumsq += x[i] * x[i];
        if i >= window {
            sum -= x[i - window];
            sumsq -= x[i - window] * x[i - window];
        }
        if i + 1 >= window {
            let w = window as f64;
            let m = sum / w;
            let var = ((sumsq - w * m * m) / (w - 1.0)).max(0.0);
            means[i] = m;
            stds[i] = var.sqrt();
        }
    }
    (means, stds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(seed: &mut u64) -> f64 {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((*seed >> 11) as f64) / ((1u64 << 53) as f64)
    }

    fn gauss(seed: &mut u64) -> f64 {
        let u1 = lcg(seed).max(1e-12);
        let u2 = lcg(seed);
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }

    #[test]
    fn ols_recovers_line() {
        let x: Vec<f64> = (0..50).map(|i| i as f64).collect();
        let y: Vec<f64> = x.iter().map(|v| 3.0 + 2.0 * v).collect();
        let (a, b) = ols(&y, &x).unwrap();
        assert!((a - 3.0).abs() < 1e-9 && (b - 2.0).abs() < 1e-9);
        assert!((pearson(&x, &y) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn rolling_matches_naive() {
        let x: Vec<f64> = (0..20).map(|i| (i * i % 7) as f64).collect();
        let (m, s) = rolling_mean_std(&x, 5);
        assert!(m[3].is_nan());
        for i in 4..20 {
            let w = &x[i + 1 - 5..=i];
            assert!((m[i] - mean(w)).abs() < 1e-9);
            assert!((s[i] - std(w)).abs() < 1e-9);
        }
    }

    #[test]
    fn adf_separates_random_walk_from_ou() {
        // The DF statistic is a distribution, so judge over several seeds: every OU series
        // must reject the unit root at 1%, and random walks must mostly fail to reject at 5%.
        let mut rw_rejects = 0;
        for seed in 1..=12u64 {
            let mut s = seed * 7919;
            let mut rw = vec![0.0];
            let mut ou = vec![0.0];
            for _ in 0..2000 {
                rw.push(rw.last().unwrap() + gauss(&mut s));
                let prev = *ou.last().unwrap();
                ou.push(prev - 0.2 * prev + gauss(&mut s));
            }
            let t_ou = adf_tstat(&ou, 2).unwrap();
            assert!(t_ou < ADF_CRIT_1PCT, "seed {seed}: ou t={t_ou}");
            if adf_tstat(&rw, 2).unwrap() < ADF_CRIT_5PCT {
                rw_rejects += 1;
            }
            let hl = half_life(&ou).unwrap();
            // theta = 0.2 -> half-life = ln2 / -ln(0.8) ~ 3.1 bars
            assert!((hl - 3.1).abs() < 1.2, "seed {seed}: half-life {hl}");
        }
        assert!(
            rw_rejects <= 3,
            "random walks rejected unit root {rw_rejects}/12 times"
        );
        assert!(half_life(&(0..100).map(|i| i as f64).collect::<Vec<_>>()).is_none());
    }
}
