//! kotlinc's JVM coroutine state machine (`codegen/coroutines/`), built on the already-inlined
//! method body the way `CoroutineTransformerMethodVisitor` builds it: codegen leaves each suspend
//! call between `InlineMarker.mark` calls, and [`transform_named_function`] turns the marked body
//! into a state machine over a continuation object — or, when every suspension point is a tail
//! call, into a body that needs none. `transform_suspend_lambda` is the same transformation for
//! a suspend lambda's `invokeSuspend`, whose class is its own continuation.

mod change_boxing;
// The suspend-lambda mode lands ahead of the `SuspendLambda` class emission that will call it;
// until then only the tests reach it.
#[cfg(test)]
mod lambda_mode;
pub(crate) mod markers;
mod redundant_locals;
mod spilled_types;
mod spilling;
mod state_machine;
mod suspension_points;
mod tail_calls;
mod transform;
mod uninitialized_stores;

#[cfg(test)]
pub(crate) use lambda_mode::{transform_suspend_lambda, SuspendLambda};
pub(crate) use transform::transform_named_function;

use super::analysis::AnalyzerError;
use super::fix_stack::FixStackError;
use super::opcodes::INVOKESTATIC;
use crate::jvm::method_node::{Insn, MethodNode, Node};

/// Why a body could not be transformed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CoroutineError {
    Analysis(AnalyzerError),
    FixStack(FixStackError),
    /// A suspension point inside a `synchronized` block, at this source line (kotlinc's
    /// `SUSPENSION_POINT_INSIDE_MONITOR`).
    SuspensionPointInsideMonitor {
        line: Option<u16>,
    },
    /// The frontend-recorded `$completion` parameter does not name a local in the physical body.
    InvalidCompletionSlot {
        slot: u16,
        max_locals: u16,
    },
    /// The recorded completion identity disagrees with the method's physical parameter layout.
    CompletionSlotMismatch {
        recorded: u16,
        physical: u16,
    },
    /// FixStack must leave only the suspend call's result on the operand stack.
    InvalidSuspensionResultStack {
        size: usize,
    },
    /// A body shape the transformation relies on and the input does not have.
    Unsupported(&'static str),
}

/// The named suspend function being transformed: what the state machine needs beyond its body.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NamedFunction<'a> {
    /// The internal name of the class declaring the method.
    pub owner: &'a str,
    /// The internal name of the function's continuation class (`AKt$f$1`).
    pub continuation_class: &'a str,
    pub source_file: &'a str,
    /// The function's first line: the machine's own code is attributed to it.
    pub line_number: u16,
    /// The physical local slot assigned to the source-level `$completion` parameter. This is
    /// recorded by codegen; the transformer must not reconstruct parameter identity from a JVM
    /// descriptor.
    pub completion_slot: u16,
    /// The dispatch receiver the continuation's constructor takes, by internal name, when it takes
    /// one (`needDispatchReceiver`, which differs from the method being an instance method for
    /// `DefaultImpls`).
    pub dispatch_receiver: Option<&'a str>,
}

/// Spill fields of one kind a state class declares before the machine is built: `L$0` through
/// `L$max_index` for the reference kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DeclaredSpillFields<'a> {
    /// The normalized field descriptor: `Ljava/lang/Object;` for every reference, else the
    /// primitive's.
    pub descriptor: &'a str,
    pub max_index: usize,
}

/// A field the state machine spills into, which the continuation class must declare.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpillField {
    pub name: String,
    pub descriptor: String,
}

/// A local spilled at a suspension point, as `@DebugMetadata` records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpilledLocal {
    pub field: String,
    pub local: String,
}

/// What a state machine adds to its continuation class.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StateMachineLayout {
    /// The spill fields, in the order kotlinc declares them.
    pub fields: Vec<SpillField>,
    /// The locals spilled at each suspension point.
    pub spilled_locals: Vec<Vec<SpilledLocal>>,
}

/// The values of the continuation class's `kotlin.coroutines.jvm.internal.DebugMetadata`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DebugMetadata {
    /// `f`
    pub source_file: String,
    /// `l`: each suspension point's line, `-1` where it has none.
    pub line_numbers: Vec<i32>,
    /// `nl`: the line after each suspension point.
    pub next_line_numbers: Vec<i32>,
    /// `i`: for each spilled local, the index of its suspension point.
    pub index_to_label: Vec<i32>,
    /// `s`: the spill field of each spilled local.
    pub spilled: Vec<String>,
    /// `n`: the name of each spilled local.
    pub local_names: Vec<String>,
    /// `m`
    pub method_name: String,
    /// `c`: the declaring class's binary name.
    pub class_name: String,
    /// `v`
    pub version: i32,
}

/// A function transformed into a state machine, with what its continuation class needs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StateMachine {
    pub method: MethodNode,
    pub layout: StateMachineLayout,
    pub debug_metadata: DebugMetadata,
}

/// A transformed named suspend function.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Transformed {
    /// Every suspension point is a tail call: no continuation class is generated.
    TailCalls(MethodNode),
    StateMachine(Box<StateMachine>),
}

/// `loadCoroutineSuspendedMarker`.
fn coroutine_suspended() -> Node {
    Node::Insn(Insn::Method {
        op: INVOKESTATIC,
        owner: "kotlin/coroutines/intrinsics/IntrinsicsKt".to_string(),
        name: "getCOROUTINE_SUSPENDED".to_string(),
        desc: "()Ljava/lang/Object;".to_string(),
        interface: false,
    })
}

#[cfg(test)]
mod tests;
