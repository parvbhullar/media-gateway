//! Phase 8 — Number translation engine module.
//!
//! Plan 08-01 lands the engine skeleton (this file + `engine.rs`). Real
//! body — DB load, regex compile-cache, precedence, replacement — lands in
//! 08-03 (D-12, D-13, D-20).

pub mod engine;

pub use engine::{AppliedRule, TranslationEngine, TranslationTrace};
