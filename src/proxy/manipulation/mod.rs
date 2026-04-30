//! Phase 9 — SIP manipulation engine module.
//!
//! Plan 09-01 lands the engine skeleton (this file + `engine.rs`) and the
//! frozen wire-type contract (Rule/Condition/Action/...). Real evaluation
//! body — DB load, regex compile-cache, condition eval, action apply — lands
//! in 09-03 (D-23, D-24).

pub mod engine;

pub use engine::{
    Action, Condition, ConditionMode, ConditionOp, LogLevel, ManipulationContext,
    ManipulationEngine, ManipulationOutcome, ManipulationTrace, Rule,
};
