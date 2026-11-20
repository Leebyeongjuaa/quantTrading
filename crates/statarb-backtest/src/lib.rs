//! Statistical-arbitrage (pairs) backtester for fixed-interval bars, built around 10-minute
//! candles but agnostic to the interval.
//!
//! Pipeline: [`data`] loads/resamples/aligns two bar series → [`strategy`] turns prices into a
//! spread, hedge ratio and z-score → [`engine`] simulates entries/exits with fees and slippage,
//! filling on the *next* bar's open so no look-ahead leaks in → [`metrics`] summarises the
//! equity curve → [`report`] writes CSV/JSON. [`stats`] holds the numerics (OLS, ADF, half-life).

pub mod data;
pub mod engine;
pub mod metrics;
pub mod report;
pub mod stats;
pub mod strategy;
pub mod synthetic;

pub use data::{align, load_csv, resample, Bar, PairSeries};
pub use engine::{Backtest, BacktestResult, Costs, Trade};
pub use metrics::Metrics;
pub use strategy::{HedgeMode, Params};
