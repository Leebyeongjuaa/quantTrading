//! WorldQuant-BRAIN-style alpha backtester.
//!
//! An *alpha* is an expression over a panel of instruments × bars, e.g.
//! `rank(-ts_delta(close, 5))` or `-ts_rank(returns, 20) * rank(volume / adv(20))`.
//! Each bar the expression yields one number per instrument; the simulator turns that vector
//! into dollar-neutral long/short weights (linear decay → neutralization → truncation →
//! scaling to the book size) and earns the next bar's returns. Reported statistics follow
//! BRAIN: annualized Sharpe, annual return, turnover, fitness, margin, drawdown, plus IC.
//!
//! Modules: [`panel`] (data matrix), [`expr`] (parser), [`ops`] (operators / evaluator),
//! [`sim`] (position construction and PnL), [`metrics`].

pub mod expr;
pub mod mat;
pub mod metrics;
pub mod ops;
pub mod panel;
pub mod sim;

pub use expr::{parse, Expr};
pub use mat::Mat;
pub use metrics::Metrics;
pub use ops::{evaluate, Evaluator};
pub use panel::Panel;
pub use sim::{
    simulate, DistanceScope, GateMode, Neutralization, Settings, SimResult, UniverseMode,
};
