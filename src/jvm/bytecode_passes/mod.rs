//! The bytecode passes kotlinc runs over a [`MethodNode`](crate::jvm::method_node::MethodNode)
//! after codegen and inlining, before the class is written: the dataflow analyses they share, the
//! coroutine state-machine transformation, and the last of the rewrites every method gets.
//!
//! The coroutine transformation runs from `classfile::coroutine_transform`; redundant-null-check,
//! redundant-cast, captured-vars and redundant-boxing elimination, temporary elimination, the
//! stack peephole, `goto` cleanup, jump negation, dead-code elimination and slot compaction run
//! from `classfile::method_rewrite`.

pub(crate) mod analysis;
pub(crate) mod captured_vars;
pub(crate) mod coroutines;
pub(crate) mod dead_code;
pub(crate) mod descriptors;
pub(crate) mod fix_stack;
pub(crate) mod insn_list;
pub(crate) mod instruction_graph;
pub(crate) mod local_slots;
pub(crate) mod negated_jumps;
pub(crate) mod opcodes;
pub(crate) mod redundant_boxing;
pub(crate) mod redundant_checkcasts;
pub(crate) mod redundant_gotos;
pub(crate) mod redundant_null_checks;
pub(crate) mod stack_peephole;
pub(crate) mod temporaries;
