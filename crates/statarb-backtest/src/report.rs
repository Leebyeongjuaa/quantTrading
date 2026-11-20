//! Human-readable summary and CSV/JSON export.

use std::path::Path;

use anyhow::Result;

use crate::engine::BacktestResult;
use crate::metrics::Metrics;

fn fmt_ts(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| ms.to_string())
}

pub fn summary(m: &Metrics) -> String {
    let mut s = String::new();
    let row = |s: &mut String, k: &str, v: String| s.push_str(&format!("{k:<24}{v:>18}\n"));
    row(
        &mut s,
        "period",
        format!("{} .. {}", fmt_ts(m.start_ts), fmt_ts(m.end_ts)),
    );
    row(&mut s, "bars", m.bars.to_string());
    row(&mut s, "initial equity", format!("{:.2}", m.initial_equity));
    row(&mut s, "final equity", format!("{:.2}", m.final_equity));
    row(
        &mut s,
        "total return",
        format!("{:.2}%", m.total_return_pct),
    );
    row(
        &mut s,
        "annualized return",
        format!("{:.2}%", m.annualized_return_pct),
    );
    row(
        &mut s,
        "annualized vol",
        format!("{:.2}%", m.annualized_vol_pct),
    );
    row(&mut s, "sharpe", format!("{:.2}", m.sharpe));
    row(&mut s, "sortino", format!("{:.2}", m.sortino));
    row(
        &mut s,
        "max drawdown",
        format!("{:.2}%", m.max_drawdown_pct),
    );
    row(&mut s, "calmar", format!("{:.2}", m.calmar));
    row(&mut s, "trades", m.trades.to_string());
    row(&mut s, "win rate", format!("{:.1}%", m.win_rate_pct));
    row(&mut s, "profit factor", format!("{:.2}", m.profit_factor));
    row(&mut s, "avg trade pnl", format!("{:.2}", m.avg_trade_pnl));
    row(&mut s, "avg bars held", format!("{:.1}", m.avg_bars_held));
    row(&mut s, "exposure", format!("{:.1}%", m.exposure_pct));
    row(&mut s, "total fees", format!("{:.2}", m.total_fees));
    row(
        &mut s,
        "exits (rev/stop/hold)",
        format!(
            "{}/{}/{}",
            m.exits_mean_reversion, m.exits_stop, m.exits_max_hold
        ),
    );
    s
}

pub fn write_outputs(dir: &Path, r: &BacktestResult, m: &Metrics) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut w = csv::Writer::from_path(dir.join("equity.csv"))?;
    w.write_record([
        "ts", "datetime", "equity", "zscore", "spread", "beta", "position",
    ])?;
    for e in &r.equity {
        w.write_record([
            e.ts.to_string(),
            fmt_ts(e.ts),
            format!("{:.4}", e.equity),
            format!("{:.4}", e.zscore),
            format!("{:.6}", e.spread),
            format!("{:.6}", e.beta),
            e.position.to_string(),
        ])?;
    }
    w.flush()?;

    let mut w = csv::Writer::from_path(dir.join("trades.csv"))?;
    w.write_record([
        "dir",
        "entry",
        "exit",
        "bars_held",
        "entry_z",
        "exit_z",
        "beta",
        "qty_a",
        "qty_b",
        "entry_px_a",
        "entry_px_b",
        "exit_px_a",
        "exit_px_b",
        "gross_pnl",
        "fees",
        "net_pnl",
        "reason",
    ])?;
    for t in &r.trades {
        w.write_record([
            format!("{:?}", t.dir),
            fmt_ts(t.entry_ts),
            fmt_ts(t.exit_ts),
            t.bars_held.to_string(),
            format!("{:.3}", t.entry_z),
            format!("{:.3}", t.exit_z),
            format!("{:.6}", t.beta),
            format!("{:.6}", t.qty_a),
            format!("{:.6}", t.qty_b),
            format!("{:.4}", t.entry_px_a),
            format!("{:.4}", t.entry_px_b),
            format!("{:.4}", t.exit_px_a),
            format!("{:.4}", t.exit_px_b),
            format!("{:.4}", t.gross_pnl),
            format!("{:.4}", t.fees),
            format!("{:.4}", t.net_pnl),
            format!("{:?}", t.reason),
        ])?;
    }
    w.flush()?;
    std::fs::write(dir.join("summary.json"), serde_json::to_string_pretty(m)?)?;
    Ok(())
}
