//! kotlinc's bytecode inliner, ported over [`crate::jvm::method_node`].
//!
//! kotlinc inlines a call by reading the callee's compiled body as an ASM `MethodNode` and letting
//! `MethodInliner` rewrite it into the caller: parameters become the call's temporaries (or the
//! caller locals the arguments already live in), the body's own locals move above them, every
//! `return` becomes a jump to the end of the inlined code, dead code and parameter null checks go,
//! and the body's debug information is renamed (`x$iv`) or dropped (`@InlineOnly`). This module is
//! that rewrite; the call site (`ir_emit`) evaluates the arguments and writes the result into the
//! caller's method.
//!
//! The port follows the order of kotlinc's own passes, because each one sees the previous one's
//! output: `prepareNode`, `removeFakeVariablesInitializationIfPresent`, `LocalReturnsNormalizer`,
//! dead-code removal, `LocalVariablesSorter`, `removeClosureAssertions`, `LocalVarRemapper` and
//! `processReturns`.
//!
//! Lambda arguments, anonymous-object regeneration, `$default` masks and `finally` rewriting are
//! later stages of the port (see "JVM unified inliner" in `docs/IMPLEMENTATION_PLAN.md`). Bodies on
//! this route do not fall back to the legacy byte splicer.

mod callee_shape;
mod local_sorter;
mod parameters;
mod preparation;
mod reified;
mod returns;

#[cfg(test)]
mod tests;

use crate::jvm::method_node::{Insn, MethodNode, Node, ShapeError};

pub(crate) use callee_shape::{
    can_inline_arguments_in_place, requires_empty_stack_on_entry, unsupported_shape,
};
pub(crate) use parameters::{Binding, Parameter, Parameters};

/// Why a body could not be inlined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InlineError {
    /// The operand stack could not be followed through the body.
    Stack(ShapeError),
    /// The body returns with different return opcodes (not valid bytecode for one method).
    MixedReturns,
    /// A parameter null check is not the `aload; ldc; invokestatic` triple kotlinc writes.
    MalformedNullCheck,
    /// An `iinc` names a parameter that lives in a caller local of another kind.
    IncrementOfCallerValue,
    /// A reified marker did not have kotlinc's exact operation/name/call shape.
    MalformedReifiedMarker,
    /// The selected call did not publish the substitution named by a reified marker.
    MissingReifiedArgument(String),
}

/// Rewrite `callee` into the code that replaces a call to it: kotlinc's `MethodInliner.doInline`
/// for a root call, followed by `IrInlineCodegen`'s leading `nop`.
///
/// The result starts with that `nop` and ends with the label every `return` now jumps to. Its
/// locals are the caller's: parameters are where `parameters` binds them and the body's own locals
/// sit above `frame_base` plus the temporaries. Line numbers are still the callee's own lines (the
/// call site maps them into the caller's source map); an `@InlineOnly` body has none, and no local
/// variables either.
pub(in crate::jvm) fn inline(
    callee: &MethodNode,
    parameters: &Parameters,
    inline_only: bool,
    frame_base: u16,
    reified_arguments: &crate::jvm::inline::ReifiedArguments,
) -> Result<MethodNode, InlineError> {
    let mut node = callee.clone();
    reified::specialize(&mut node, reified_arguments)?;
    let mut node = preparation::prepare(&node, inline_only);
    preparation::remove_fake_variable_initializations(&mut node);
    returns::normalize_local_returns(&mut node)?;
    preparation::remove_dead_code(&mut node)?;
    local_sorter::sort(&mut node, parameters.args_size());
    preparation::remove_closure_assertions(&mut node)?;
    let mut node = parameters.remap(&node, frame_base)?;
    let end = node.new_label();
    node.nodes.push(Node::Label(end));
    returns::process_returns(&mut node, end);
    node.nodes.insert(0, Node::Insn(Insn::Op(NOP)));
    Ok(node)
}

const NOP: u8 = 0x00;
