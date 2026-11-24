//! `wq-bt`: WorldQuant-BRAIN-style alpha backtester.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use wq_backtest::ops::OPERATORS;
use wq_backtest::{evaluate, parse, simulate, Metrics, Neutralization, Panel, Settings, SimResult};

#[derive(Parser)]
#[command(
    name = "wq-bt",
    version,
    about = "Cross-sectional alpha backtester (BRAIN-style)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct DataArgs {
    /// Directory of per-instrument CSVs (`<SYMBOL>.csv`, ts,open,high,low,close,volume).
    #[arg(long)]
    data_dir: PathBuf,
    /// Resample every series to this bar before building the panel, e.g. 1h, 4h, 1d.
    #[arg(long)]
    bar: Option<String>,
}

#[derive(Args, Clone)]
struct SimArgs {
    /// Bars between alpha data and position: 1 (BRAIN default) or 0.
    #[arg(long, default_value_t = 1)]
    delay: usize,
    /// Linear decay window in bars (0 = none).
    #[arg(long, default_value_t = 0)]
    decay: usize,
    #[arg(long, value_enum, default_value = "market")]
    neutralization: Neutralization,
    /// Max |weight| as fraction of booksize (0 disables).
    #[arg(long, default_value_t = 0.1)]
    truncation: f64,
    #[arg(long, default_value_t = 1_000_000.0)]
    booksize: f64,
    /// Keep only the top-N instruments by trailing dollar volume (0 = all).
    #[arg(long, default_value_t = 0)]
    universe: usize,
    #[arg(long, default_value_t = 20)]
    universe_window: usize,
    /// Cost per dollar traded in bps (0 = cost-free like BRAIN).
    #[arg(long, default_value_t = 0.0)]
    cost_bps: f64,
}

impl SimArgs {
    fn settings(&self) -> Settings {
        Settings {
            delay: self.delay,
            decay: self.decay,
            neutralization: self.neutralization,
            truncation: self.truncation,
            booksize: self.booksize,
            universe: self.universe,
            universe_window: self.universe_window,
            cost_bps: self.cost_bps,
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Simulate one alpha expression.
    Run {
        /// e.g. "rank(-ts_delta(close, 5))"
        #[arg(allow_hyphen_values = true)]
        alpha: String,
        #[command(flatten)]
        data: DataArgs,
        #[command(flatten)]
        sim: SimArgs,
        /// Write pnl.csv, weights.csv, summary.json here.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Simulate every line of a file (one alpha per line, `#` comments) and rank by fitness.
    Batch {
        file: PathBuf,
        #[command(flatten)]
        data: DataArgs,
        #[command(flatten)]
        sim: SimArgs,
    },
    /// PnL correlation between two alphas (BRAIN's self-correlation check).
    Corr {
        #[arg(allow_hyphen_values = true)]
        alpha_a: String,
        #[arg(allow_hyphen_values = true)]
        alpha_b: String,
        #[command(flatten)]
        data: DataArgs,
        #[command(flatten)]
        sim: SimArgs,
    },
    /// Parse an expression and print its normalized form.
    Check {
        #[arg(allow_hyphen_values = true)]
        alpha: String,
    },
    /// List fields and operators.
    Ops,
    /// Describe the data panel (instruments, bars, coverage).
    Info {
        #[command(flatten)]
        data: DataArgs,
    },
}

fn parse_bar(s: &str) -> Result<i64> {
    let (num, unit) = s.split_at(s.len().checked_sub(1).context("bad bar")?);
    let n: i64 = num.parse().with_context(|| format!("bad bar size {s}"))?;
    Ok(n * match unit {
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => bail!("bar unit must be m/h/d, got {s}"),
    })
}

fn load(d: &DataArgs) -> Result<Panel> {
    let bar_ms = d.bar.as_deref().map(parse_bar).transpose()?;
    let p = Panel::load_dir(&d.data_dir, bar_ms)?;
    eprintln!(
        "panel: {} instruments × {} bars, interval {}s ({} .. {})",
        p.n(),
        p.t(),
        p.interval_ms / 1000,
        fmt_ts(p.ts[0]),
        fmt_ts(p.ts[p.t() - 1])
    );
    Ok(p)
}

fn fmt_ts(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn run_alpha(panel: &Panel, src: &str, settings: &Settings) -> Result<(SimResult, Metrics)> {
    let expr = parse(src).with_context(|| format!("parse `{src}`"))?;
    let alpha = evaluate(panel, &expr).with_context(|| format!("evaluate `{src}`"))?;
    let coverage =
        alpha.data().iter().filter(|v| v.is_finite()).count() as f64 / alpha.data().len() as f64;
    if coverage == 0.0 {
        bail!("alpha `{src}` is NaN everywhere (window longer than the data?)");
    }
    let r = simulate(panel, &alpha, settings);
    let m = Metrics::from_sim(&r, panel.bars_per_year(), panel.bars_per_day())
        .context("no tradeable bars")?;
    Ok((r, m))
}

fn write_outputs(dir: &PathBuf, panel: &Panel, r: &SimResult, m: &Metrics) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut w = csv::Writer::from_path(dir.join("pnl.csv"))?;
    w.write_record([
        "ts",
        "datetime",
        "pnl",
        "gross_pnl",
        "costs",
        "cum_pnl",
        "turnover",
        "long",
        "short",
        "ic",
    ])?;
    let mut cum = 0.0;
    for t in 0..r.ts.len() {
        cum += r.pnl[t];
        w.write_record([
            r.ts[t].to_string(),
            fmt_ts(r.ts[t]),
            format!("{:.4}", r.pnl[t]),
            format!("{:.4}", r.gross_pnl[t]),
            format!("{:.4}", r.costs[t]),
            format!("{:.4}", cum),
            format!("{:.6}", r.turnover[t]),
            r.long_count[t].to_string(),
            r.short_count[t].to_string(),
            format!("{:.4}", r.ic[t]),
        ])?;
    }
    w.flush()?;
    let mut w = csv::Writer::from_path(dir.join("weights.csv"))?;
    let mut header = vec!["ts".to_string()];
    header.extend(panel.symbols.iter().cloned());
    w.write_record(&header)?;
    for t in 0..r.ts.len() {
        let mut rec = vec![r.ts[t].to_string()];
        rec.extend(r.weights.row(t).iter().map(|v| format!("{v:.2}")));
        w.write_record(&rec)?;
    }
    w.flush()?;
    std::fs::write(dir.join("summary.json"), serde_json::to_string_pretty(m)?)?;
    Ok(())
}

fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len()) as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        sxy += (x - ma) * (y - mb);
        sxx += (x - ma).powi(2);
        syy += (y - mb).powi(2);
    }
    if sxx > 0.0 && syy > 0.0 {
        sxy / (sxx * syy).sqrt()
    } else {
        f64::NAN
    }
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Run {
            alpha,
            data,
            sim,
            out,
        } => {
            let panel = load(&data)?;
            let (r, m) = run_alpha(&panel, &alpha, &sim.settings())?;
            println!("alpha: {alpha}\n{}", m.summary());
            if let Some(dir) = out {
                write_outputs(&dir, &panel, &r, &m)?;
                println!("wrote {}/pnl.csv, weights.csv, summary.json", dir.display());
            }
        }
        Cmd::Batch { file, data, sim } => {
            let panel = load(&data)?;
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("read {}", file.display()))?;
            let settings = sim.settings();
            let mut rows = Vec::new();
            for line in text.lines() {
                let src = line.split('#').next().unwrap_or("").trim();
                if src.is_empty() {
                    continue;
                }
                match run_alpha(&panel, src, &settings) {
                    Ok((_, m)) => rows.push((src.to_string(), m)),
                    Err(e) => eprintln!("skip `{src}`: {e:#}"),
                }
            }
            rows.sort_by(|a, b| {
                b.1.fitness
                    .partial_cmp(&a.1.fitness)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            println!(
                "{:>8} {:>8} {:>9} {:>9} {:>9} {:>8}  alpha",
                "fitness", "sharpe", "ret%", "turn/d%", "margin", "maxdd%"
            );
            for (src, m) in rows {
                println!(
                    "{:>8.3} {:>8.3} {:>9.2} {:>9.2} {:>9.2} {:>8.2}  {}",
                    m.fitness,
                    m.sharpe,
                    m.annual_return * 100.0,
                    m.turnover_daily * 100.0,
                    m.margin_bps,
                    m.max_drawdown_pct,
                    src
                );
            }
        }
        Cmd::Corr {
            alpha_a,
            alpha_b,
            data,
            sim,
        } => {
            let panel = load(&data)?;
            let s = sim.settings();
            let (ra, ma) = run_alpha(&panel, &alpha_a, &s)?;
            let (rb, mb) = run_alpha(&panel, &alpha_b, &s)?;
            let c = pearson(&ra.pnl, &rb.pnl);
            println!(
                "A: {alpha_a}\n   sharpe {:.3} fitness {:.3}",
                ma.sharpe, ma.fitness
            );
            println!(
                "B: {alpha_b}\n   sharpe {:.3} fitness {:.3}",
                mb.sharpe, mb.fitness
            );
            println!("PnL correlation: {c:.4}");
        }
        Cmd::Check { alpha } => {
            let e = parse(&alpha)?;
            println!("{e}");
        }
        Cmd::Ops => {
            println!("fields: {}\n", Panel::FIELDS.join(" "));
            for (sig, desc) in OPERATORS {
                println!("{sig:<70} {desc}");
            }
            println!("\noperators: + - * / ^  < <= > >= == !=  && || !  cond ? a : b");
        }
        Cmd::Info { data } => {
            let p = load(&data)?;
            println!(
                "{:<12} {:>8} {:>10} {:>12} {:>12}",
                "symbol", "bars", "coverage", "first", "last"
            );
            for (i, s) in p.symbols.iter().enumerate() {
                let col = p.close.column(i);
                let valid: Vec<usize> = col
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| v.is_finite())
                    .map(|(t, _)| t)
                    .collect();
                let (first, last) = match (valid.first(), valid.last()) {
                    (Some(f), Some(l)) => (fmt_ts(p.ts[*f]), fmt_ts(p.ts[*l])),
                    _ => (String::new(), String::new()),
                };
                println!(
                    "{:<12} {:>8} {:>9.1}% {:>12} {:>12}",
                    s,
                    valid.len(),
                    valid.len() as f64 / p.t() as f64 * 100.0,
                    first,
                    last
                );
            }
        }
    }
    Ok(())
}
