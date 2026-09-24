//! Target-owned analyses over decoded JVM bytecode.
//!
//! These analyses model class-file control flow and verifier state. Coroutine planning, finished
//! method rewrites, and other bytecode consumers share them without making generic class-file code
//! depend on one particular transform.

mod control_graph;
mod frame_computer;
mod frame_types;

pub(crate) use control_graph::{ControlGraph, Handler};
pub(crate) use frame_computer::{
    entry_frame, ComputedFrame, ComputedFrames, Decline, FrameComputation, TypedHandler,
};
pub(crate) use frame_types::{FrameTypes, PoolView, VerificationType};
