//! JVM dependency inline templates: an inline function's compiled body, read from the dependency
//! class file that holds it, in a structured form a non-JVM target can expand.
//!
//! The Native and Wasm lanes resolve against JVM dependency class files too (`JvmLibraries`), and
//! an inline function such as `UIntArray(size, init)` has no body there other than its bytecode.
//! [`decode`] turns that bytecode, already read into a [`MethodNode`](crate::jvm::method_node),
//! into a [`DecodedBody`]: basic blocks whose operations consume and produce typed values instead
//! of operand-stack words, local-variable slots as typed [`Local`]s, every call and field access
//! by its exact JVM owner, name and descriptor, the line each operation came from, and the
//! exception table as handlers over blocks.
//!
//! The decoder only restates the bytecode. It drops no instruction (a parameter null check or one
//! of kotlinc's inline markers is an ordinary [`OpKind::Invoke`]) and gives no call a meaning:
//! normalizing a decoded body into common IR, keyed on exact owner/name/descriptor, is the next
//! step. See "JVM dependency inline templates" in `docs/IMPLEMENTATION_PLAN.md`.

mod decode;
mod layout;
mod model;
mod translate;

pub use decode::decode;
pub use model::{
    Anchor, ArrayElement, BinaryOp, Block, BlockId, Comparison, Condition, DecodedBody, Edge,
    FieldRef, Handler, InvokeKind, JvmType, Local, LocalId, LocalScope, MethodRef, Op, OpKind,
    Terminator, Value, ValueDef, ValueId,
};

/// Why a body could not be decoded, at the index of the offending node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// kotlinc's frame analysis rejected the body.
    Analysis { node: usize, message: String },
    /// The body is not one the structured form represents.
    Malformed { node: usize, reason: &'static str },
}

impl DecodeError {
    fn malformed(node: usize, reason: &'static str) -> DecodeError {
        DecodeError::Malformed { node, reason }
    }
}

#[cfg(test)]
mod tests;
