//! `statarb-bt`: pairs / stat-arb backtester for 10-minute bars.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use statarb_backtest::data::{self, write_csv};
use statarb_backtest::strategy::{diagnose, HedgeMode, Params};
use statarb_backtest::{align, load_csv, report, resample, synthetic, Backtest, Costs, Metrics};

#[derive(Parser)]
#[command(
    name = "statarb-bt",
    version,
    about = "Statistical-arbitrage pairs backtester"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct StrategyArgs {
    /// Z-score / rolling-regression window in bars (144 = 1 day of 10m bars).
    #[arg(long, default_value_t = 144)]
    lookback: usize,
    /// Bars used to fit the static hedge ratio (1008 = 7 days of 10m bars).
    #[arg(long, default_value_t = 1008)]
    train_bars: usize,
    #[arg(long, value_enum, default_value = "static")]
    hedge: HedgeMode,
    /// Regress raw prices instead of log prices.
    #[arg(long)]
    raw_prices: bool,
    #[arg(long, default_value_t = 2.0)]
    entry_z: f64,
    #[arg(long, default_value_t = 0.5)]
    exit_z: f64,
    /// 0 disables.
    #[arg(long, default_value_t = 4.0)]
    stop_z: f64,
    /// 0 disables.
    #[arg(long, default_value_t = 288)]
    max_hold_bars: usize,
    #[arg(long, default_value_t = 0.1)]
    beta_min: f64,
    #[arg(long, default_value_t = 10.0)]
    beta_max: f64,
}

impl StrategyArgs {
    fn params(&self) -> Params {
        Params {
            lookback: self.lookback,
            train_bars: self.train_bars,
            hedge: self.hedge,
            log_prices: !self.raw_prices,
            entry_z: self.entry_z,
            exit_z: self.exit_z,
            stop_z: self.stop_z,
            max_hold_bars: self.max_hold_bars,
            beta_min: self.beta_min,
            beta_max: self.beta_max,
        }
    }
}

#[derive(Args, Clone)]
struct CostArgs {
    /// Taker fee per side, bps.
    #[arg(long, default_value_t = 2.0)]
    fee_bps: f64,
    /// Slippage per fill, bps.
    #[arg(long, default_value_t = 1.0)]
    slippage_bps: f64,
    /// Leg-A notional per trade (USD).
    #[arg(long, default_value_t = 10_000.0)]
    notional: f64,
}

impl CostArgs {
    fn costs(&self) -> Costs {
        Costs {
            fee_bps: self.fee_bps,
            slippage_bps: self.slippage_bps,
            notional: self.notional,
        }
    }
}

#[derive(Args, Clone)]
struct DataArgs {
    /// CSV of leg A bars (ts,open,high,low,close,volume).
    #[arg(long)]
    a: PathBuf,
    /// CSV of leg B bars.
    #[arg(long)]
    b: PathBuf,
    /// Resample both inputs to this bar size before aligning, e.g. 10m, 1h.
    #[arg(long)]
    bar: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run one backtest and write equity.csv / trades.csv / summary.json.
    Run {
        #[command(flatten)]
        data: DataArgs,
        #[command(flatten)]
        strat: StrategyArgs,
        #[command(flatten)]
        cost: CostArgs,
        #[arg(long, default_value = "bt-out")]
        out: PathBuf,
    },
    /// Cointegration diagnostics for a pair (beta, correlation, ADF t-stat, half-life).
    Analyze {
        #[command(flatten)]
        data: DataArgs,
        #[arg(long)]
        raw_prices: bool,
    },
    /// Grid-search entry/exit thresholds; prints a table sorted by Sharpe.
    Sweep {
        #[command(flatten)]
        data: DataArgs,
        #[command(flatten)]
        strat: StrategyArgs,
        #[command(flatten)]
        cost: CostArgs,
        #[arg(long, value_delimiter = ',', default_value = "1.5,2.0,2.5,3.0")]
        entries: Vec<f64>,
        #[arg(long, value_delimiter = ',', default_value = "0.0,0.5,1.0")]
        exits: Vec<f64>,
        #[arg(long, value_delimiter = ',', default_value = "72,144,288")]
        lookbacks: Vec<usize>,
    },
    /// Resample a bar CSV to a coarser interval (e.g. 1m or 5m -> 10m).
    Resample {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value = "10m")]
        bar: String,
    },
    /// Write a synthetic cointegrated pair for smoke-testing the pipeline.
    Synthetic {
        #[arg(long, default_value_t = 5000)]
        bars: usize,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long, default_value_t = 1.2)]
        beta: f64,
        #[arg(long, default_value = "synthetic")]
        out_dir: PathBuf,
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

fn load_pair(d: &DataArgs) -> Result<statarb_backtest::PairSeries> {
    let mut a = load_csv(&d.a)?;
    let mut b = load_csv(&d.b)?;
    if let Some(bar) = &d.bar {
        let ms = parse_bar(bar)?;
        a = resample(&a, ms);
        b = resample(&b, ms);
    }
    let pair = align(&a, &b);
    if pair.is_empty() {
        bail!(
            "no overlapping timestamps between {} and {}",
            d.a.display(),
            d.b.display()
        );
    }
    eprintln!(
        "loaded {} aligned bars (A {} rows, B {} rows), interval {}s",
        pair.len(),
        a.len(),
        b.len(),
        pair.interval_ms().unwrap_or(0) / 1000
    );
    Ok(pair)
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Run {
            data,
            strat,
            cost,
            out,
        } => {
            let pair = load_pair(&data)?;
            let r = Backtest::new(strat.params(), cost.costs()).run(&pair);
            let Some(m) = Metrics::from_result(&r) else {
                bail!("series too short for lookback/train window")
            };
            print!("{}", report::summary(&m));
            report::write_outputs(&out, &r, &m)?;
            println!(
                "wrote {}/equity.csv, trades.csv, summary.json",
                out.display()
            );
        }
        Cmd::Analyze { data, raw_prices } => {
            let pair = load_pair(&data)?;
            let a: Vec<f64> = pair.a.iter().map(|b| b.close).collect();
            let b: Vec<f64> = pair.b.iter().map(|b| b.close).collect();
            let d = diagnose(&a, &b, !raw_prices).context("not enough data")?;
            println!("{}", serde_json::to_string_pretty(&d)?);
            println!(
                "ADF critical values: 1% {:.2}, 5% {:.2}, 10% {:.2} (more negative = stationary spread)",
                statarb_backtest::stats::ADF_CRIT_1PCT,
                statarb_backtest::stats::ADF_CRIT_5PCT,
                statarb_backtest::stats::ADF_CRIT_10PCT
            );
        }
        Cmd::Sweep {
            data,
            strat,
            cost,
            entries,
            exits,
            lookbacks,
        } => {
            let pair = load_pair(&data)?;
            let mut rows = Vec::new();
            for &lb in &lookbacks {
                for &e in &entries {
                    for &x in &exits {
                        if x >= e {
                            continue;
                        }
                        let mut p = strat.params();
                        p.lookback = lb;
                        p.entry_z = e;
                        p.exit_z = x;
                        let r = Backtest::new(p, cost.costs()).run(&pair);
                        if let Some(m) = Metrics::from_result(&r) {
                            rows.push((lb, e, x, m));
                        }
                    }
                }
            }
            rows.sort_by(|l, r| {
                r.3.sharpe
                    .partial_cmp(&l.3.sharpe)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            println!(
                "{:>8} {:>7} {:>6} {:>8} {:>8} {:>9} {:>7} {:>8}",
                "lookback", "entry", "exit", "sharpe", "ret%", "maxdd%", "trades", "win%"
            );
            for (lb, e, x, m) in rows {
                println!(
                    "{:>8} {:>7.2} {:>6.2} {:>8.2} {:>8.2} {:>9.2} {:>7} {:>8.1}",
                    lb,
                    e,
                    x,
                    m.sharpe,
                    m.total_return_pct,
                    m.max_drawdown_pct,
                    m.trades,
                    m.win_rate_pct
                );
            }
        }
        Cmd::Resample { input, output, bar } => {
            let bars = load_csv(&input)?;
            let out = resample(&bars, parse_bar(&bar)?);
            write_csv(&output, &out)?;
            println!(
                "{} -> {} bars written to {}",
                bars.len(),
                out.len(),
                output.display()
            );
        }
        Cmd::Synthetic {
            bars,
            seed,
            beta,
            out_dir,
        } => {
            let pair = synthetic::cointegrated_pair(bars, seed, 600_000, beta, 0.05, 0.01);
            std::fs::create_dir_all(&out_dir)?;
            data::write_csv(out_dir.join("a_10m.csv"), &pair.a)?;
            data::write_csv(out_dir.join("b_10m.csv"), &pair.b)?;
            println!(
                "wrote {}/a_10m.csv and b_10m.csv ({bars} bars, beta {beta})",
                out_dir.display()
            );
        }
    }
    Ok(())
}
