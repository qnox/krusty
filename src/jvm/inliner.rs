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

mod anonymous_object;
mod callee_shape;
mod functional_arguments;
mod lambda_expansion;
mod local_sorter;
mod name_generator;
mod object_regeneration;
mod parameters;
mod preparation;
mod reified;
mod returns;
mod try_blocks;

#[cfg(test)]
mod tests;

use crate::jvm::method_node::{Insn, MethodNode, Node, ShapeError};

pub(crate) use anonymous_object::{regenerate, CallSite, Regeneration, RegenerationError};
pub(crate) use callee_shape::{
    can_inline_arguments_in_place, requires_empty_stack_on_entry, unsupported_shape,
    ObjectRegeneration, UnsupportedShape,
};
pub(crate) use lambda_expansion::{Lambda, SourceLines};
pub(crate) use name_generator::ClassNameGenerators;
pub(crate) use object_regeneration::AnonymousObjects;
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
    /// The body's data flow could not be followed (`markPlacesForInlineAndRemoveInlinable`).
    Analysis(String),
    /// The body reads an inline lambda parameter as a value rather than invoking it.
    LambdaParameterAccess,
    /// An `invoke` passes a lambda more or fewer arguments than it takes.
    LambdaArity,
    /// An anonymous object's `new` and constructor calls do not pair up.
    UnpairedAnonymousObject,
    /// An anonymous object the body constructs could not be regenerated for the call site.
    Regeneration(RegenerationError),
}

/// What one call's inlining reports to its caller (kotlinc's `InliningContext`): where the body's
/// lines are mapped and where the anonymous objects it constructs are regenerated.
pub(in crate::jvm) struct InliningContext<'a> {
    pub lines: &'a mut dyn SourceLines,
    pub objects: &'a mut dyn AnonymousObjects,
}

/// Rewrite `callee` into the code that replaces a call to it: kotlinc's `MethodInliner.doInline`
/// for a root call, followed by `IrInlineCodegen`'s leading `nop`.
///
/// The result starts with that `nop` and ends with the label every `return` now jumps to. Its
/// locals are the caller's: parameters are where `parameters` binds them and the body's own locals
/// sit above `frame_base` plus the temporaries. Each `invoke` of one of `lambdas` is replaced by
/// that lambda's body. The body's line numbers go through `call.lines`, the lambdas' stay as they
/// are; an `@InlineOnly` body has no lines of its own, and no local variables either.
pub(in crate::jvm) fn inline(
    callee: &MethodNode,
    parameters: &Parameters,
    lambdas: &[Lambda],
    inline_only: bool,
    frame_base: u16,
    reified_arguments: &crate::jvm::reified_arguments::ReifiedArguments,
    call: InliningContext<'_>,
) -> Result<MethodNode, InlineError> {
    let mut node = callee.clone();
    reified::specialize(&mut node, reified_arguments)?;
    let mut node = preparation::prepare(&node, inline_only, parameters);
    try_blocks::move_try_starts_to_their_first_instruction(&mut node)?;
    preparation::remove_fake_variable_initializations(&mut node);
    returns::normalize_local_returns(&mut node)?;
    object_regeneration::regenerate_objects(&mut node, call.objects)?;
    let invokes = functional_arguments::mark_places(&mut node, parameters)?;
    let context = lambda_expansion::Context {
        parameters,
        lambdas,
        inline_only,
    };
    let mut node = lambda_expansion::expand(&node, &context, invokes, call.lines)?;
    preparation::remove_closure_assertions(&mut node)?;
    let mut node = parameters.remap(&node, frame_base)?;
    let end = node.new_label();
    node.nodes.push(Node::Label(end));
    returns::process_returns(&mut node, end);
    node.nodes.insert(0, Node::Insn(Insn::Op(NOP)));
    Ok(node)
}

const NOP: u8 = 0x00;
