//! Call-site specialization of lambda implementations that escape an inline expansion.
//!
//! The transfer module owns both the mutation-free specialization plan and its commit rewrite. The
//! facade exposes only that transaction and the runtime-binding query shared with anonymous-object
//! specialization.

mod transfer;

pub(super) use transfer::{apply, expression_facts_use_binding, plan, EscapingLambdaPlan};
