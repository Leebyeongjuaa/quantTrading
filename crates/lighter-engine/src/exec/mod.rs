//! Execution algorithms built on the gateway + stream.
//!
//! * [`iceberg`] – shows only a small slice of a large order and refills it as it fills.
//! * [`pair`]    – submits two legs together (long A / short B) and reports the imbalance.
//!
//! Each algo is split into a pure, synchronous *planner* (state machine, fully unit-tested)
//! and an async *runner* that wires the planner to live events.

pub mod iceberg;
pub mod pair;

pub use iceberg::{IcebergConfig, IcebergExecutor, IcebergPlanner, IcebergReport, Pricing};
pub use pair::{Leg, PairConfig, PairExecutor, PairReport, PairSizer};
