//! The bytecode passes kotlinc runs over a [`MethodNode`](crate::jvm::method_node::MethodNode)
//! after codegen and inlining, before the class is written: the dataflow analyses they share and
//! the coroutine state-machine transformation.

pub(crate) mod analysis;
pub(crate) mod coroutines;
pub(crate) mod descriptors;
pub(crate) mod fix_stack;
pub(crate) mod insn_list;
pub(crate) mod opcodes;
