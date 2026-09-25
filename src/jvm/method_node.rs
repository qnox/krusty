//! A method body in the tree form kotlinc's inliner works on.
//!
//! kotlinc inlines by transforming ASM `MethodNode`s: an instruction list where branch targets,
//! try/catch ranges, line numbers and local-variable ranges all hang off label nodes, and every
//! constant-pool operand is symbolic (an owner/name/descriptor, a constant value). Nothing in that
//! form depends on byte offsets or on which class's pool a body was read from, which is what lets
//! `MethodInliner` insert, delete and rewrite instructions freely and then write the result into
//! any class.
//!
//! This module is that form for krusty: [`MethodNode`] and its [`Node`]s, a reader that decodes a
//! method body against the pool of the class it was written into ([`MethodNode::read_code`]: a
//! class file's, or that of a class krusty is still writing) and an assembler that lays one out
//! again against any constant pool ([`MethodNode::assemble`]). Stack-map frames are deliberately
//! absent: kotlinc's writer recomputes them (`COMPUTE_FRAMES`) after inlining, and krusty's frame
//! computer does the same over the assembled body.
//!
//! Its consumers are the bytecode rewrites a class's methods get when it is written, which carry
//! the rewritten body in this form (see `classfile::method_rewrite`), and the unified inliner, a
//! port of kotlinc's `MethodInliner` (see "JVM unified inliner" in `docs/IMPLEMENTATION_PLAN.md`).

mod assemble;
mod flow;
mod nodes;
mod pool;
mod read;

pub use assemble::{AssembleError, AssembledCode, AssembledLocal, ConstantSink};
pub use flow::LabelPositions;
pub use nodes::{Constant, Handle, Insn, LabelId, LocalVariable, MethodNode, Node, TryCatchBlock};
pub use pool::{ConstantPoolView, PoolEntry};
pub use read::{CodeAttribute, MalformedCode};

#[cfg(test)]
mod tests;
