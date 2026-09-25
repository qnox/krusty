//! The bytecode passes kotlinc runs over a [`MethodNode`](crate::jvm::method_node::MethodNode)
//! after codegen and inlining, before the class is written: the dataflow analyses they share, the
//! coroutine state-machine transformation, and the last of the rewrites every method gets.
//!
//! The coroutine transformation runs from `classfile::coroutine_transform`. The optimizer passes
//! (redundant-null-check, redundant-cast, captured-vars and redundant-boxing elimination, temporary
//! elimination, the stack peephole, `goto` and `nop` cleanup, jump negation, the casts before array
//! stores, dead-code elimination and slot compaction) run in kotlinc's order from [`pipeline`], which `classfile::method_rewrite`
//! calls for every method, the coroutine transformation's result included.

pub(crate) mod analysis;
pub(crate) mod captured_vars;
pub(crate) mod checkcasts_before_aastore;
pub(crate) mod coroutines;
pub(crate) mod dead_code;
pub(crate) mod descriptors;
pub(crate) mod fix_stack;
pub(crate) mod insn_list;
pub(crate) mod instruction_graph;
#[cfg(test)]
mod labelled_body;
pub(crate) mod local_slots;
pub(crate) mod negated_jumps;
pub(crate) mod opcodes;
pub(crate) mod pipeline;
pub(crate) mod redundant_boxing;
pub(crate) mod redundant_checkcasts;
pub(crate) mod redundant_gotos;
pub(crate) mod redundant_nops;
pub(crate) mod redundant_null_checks;
pub(crate) mod stack_peephole;
pub(crate) mod temporaries;
