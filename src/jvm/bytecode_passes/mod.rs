//! The bytecode passes kotlinc runs over a [`MethodNode`](crate::jvm::method_node::MethodNode)
//! after codegen and inlining, before the class is written: the dataflow analyses they share, the
//! coroutine state-machine transformation, and the last of the rewrites every method gets.
//!
//! The redundant-null-check and redundant-cast passes, temporary elimination, the stack peephole,
//! `goto` cleanup, jump negation, dead-code elimination and slot compaction run in the compiler so
//! far (from `classfile::method_rewrite`); the rest is used by its tests until the emitter hands
//! the coroutine transformation a method node.

#[cfg(test)]
pub(crate) mod analysis;
#[cfg(test)]
pub(crate) mod coroutines;
pub(crate) mod dead_code;
#[cfg(test)]
pub(crate) mod descriptors;
#[cfg(test)]
pub(crate) mod fix_stack;
pub(crate) mod insn_list;
pub(crate) mod instruction_graph;
pub(crate) mod local_slots;
pub(crate) mod negated_jumps;
#[cfg(test)]
pub(crate) mod opcodes;
#[cfg(test)]
pub(crate) mod redundant_boxing;
pub(crate) mod redundant_checkcasts;
pub(crate) mod redundant_gotos;
pub(crate) mod redundant_null_checks;
pub(crate) mod stack_peephole;
pub(crate) mod temporaries;
