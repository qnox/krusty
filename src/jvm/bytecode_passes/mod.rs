//! The bytecode passes kotlinc runs over a [`MethodNode`](crate::jvm::method_node::MethodNode)
//! after codegen and inlining, before the class is written: the dataflow analyses they share, the
//! coroutine state-machine transformation, and the last of the rewrites every method gets.
//!
//! The coroutine transformation runs from `classfile::coroutine_transform`; the stack peephole,
//! `goto` cleanup, jump negation, dead-code elimination and slot compaction from
//! `classfile::method_rewrite`.

pub(crate) mod analysis;
pub(crate) mod coroutines;
pub(crate) mod dead_code;
pub(crate) mod descriptors;
pub(crate) mod fix_stack;
pub(crate) mod insn_list;
pub(crate) mod local_slots;
pub(crate) mod negated_jumps;
pub(crate) mod opcodes;
pub(crate) mod redundant_gotos;
pub(crate) mod stack_peephole;
