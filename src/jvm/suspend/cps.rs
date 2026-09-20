//! The JVM coroutine transform as a **bytecode → bytecode** pass.
//!
//! The state machine a `suspend fun` needs is built from facts that only exist once every classpath
//! `inline` body reached from it has been spliced: at a suspension inside a spliced inline lambda,
//! kotlinc spills locals that the inline body itself introduced. A pass that runs before the splice
//! cannot see them. See `docs/JVM_INLINE_BEFORE_CPS.md` for the measurement and the staging.
//!
//! This module owns the analyses that transform needs over already-emitted bytecode. It decides
//! nothing about Kotlin semantics; its input is a decoded instruction list plus the method's
//! exception table, both of which the emitter already produces.

mod control_graph;
mod liveness;

pub(crate) use control_graph::{ControlGraph, Handler};
pub(crate) use liveness::LocalLiveness;
