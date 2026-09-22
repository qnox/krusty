//! The JVM coroutine transform for suspensions the IR machine cannot see.
//!
//! The state machine a `suspend fun` needs is built from facts that only exist once every classpath
//! `inline` body reached from it has been spliced: at a suspension inside a spliced inline lambda,
//! kotlinc spills locals that the inline body itself introduced. A pass that runs before the splice
//! cannot see them. See `docs/JVM_INLINE_BEFORE_CPS.md` for the measurement and the staging.
//!
//! So the machine for those functions is built during emission, from bytecode facts: the body is
//! emitted once with its inline bodies spliced, the spill set is read off those bytes, and the real
//! method is emitted with the machine around it. This module owns the analyses that answers needs —
//! which suspensions are affected, and which locals are live across each one. It decides nothing
//! about Kotlin semantics.

mod control_graph;
mod frame_types;
mod inline_suspension;
mod liveness;
mod routing;

pub(crate) use control_graph::{ControlGraph, Handler};
pub(crate) use frame_types::{FrameTypes, VerificationType};
pub(crate) use inline_suspension::{
    frame_suspensions, spliced_inline_suspensions, spliced_suspension_lambda_impls,
    suspends_in_a_value_try,
};
pub(crate) use liveness::LocalLiveness;
pub(crate) use routing::{EmitTimeMachines, SplicedSuspension};
