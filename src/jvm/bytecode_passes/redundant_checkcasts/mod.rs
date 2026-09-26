//! kotlinc's `RedundantCheckCastEliminationMethodTransformer`, over a [`MethodNode`]: a
//! `checkcast` goes when the value it casts already has exactly the requested type, or is `null`.
//!
//! The values are kotlinc's: the method runs through `FastMethodAnalyzer` with exception edges
//! pruned and `OptimizationBasicInterpreter` (see [`BasicInterpreter`]), which keeps a reference's
//! exact class and `null` as a value of its own. Exact type equality is the whole test, so no
//! hierarchy query is needed. One refinement is the transformer's own: an `aload` of a local inside
//! that local's range, at an instruction the method's entry does not reach without following a
//! handler (catch code, for one), loads the type the local-variable table declares.
//!
//! Like kotlinc, the pass keeps every cast of a method with a reified-operation marker, whose
//! casts the inliner still rewrites, and every cast to a multi-dimensional array type.

use std::collections::HashMap;

use super::analysis::{
    analyze_with, AnalyzerError, AnalyzerOptions, At, BasicInterpreter, BasicValue, Interpreter,
    PlainFrames,
};
use super::dead_code;
use super::descriptors;
use super::opcodes::{ALOAD, CHECKCAST, INVOKESTATIC};
use crate::jvm::method_node::{Insn, LabelPositions, MethodNode, Node};

/// Whether `insn` is kotlinc's `Intrinsics.reifiedOperationMarker` call (`ReifiedTypeInliner`'s
/// `isOperationReifiedMarker`): the static, non-interface `(ILjava/lang/String;)V` method of that
/// name on `Intrinsics`. Another call spelled the same is ordinary bytecode.
pub(super) fn is_reified_marker(insn: &Insn) -> bool {
    matches!(
        insn,
        Insn::Method { op: INVOKESTATIC, owner, name, desc, interface: false }
            if owner == "kotlin/jvm/internal/Intrinsics"
                && name == "reifiedOperationMarker"
                && desc == "(ILjava/lang/String;)V"
    )
}

/// The transformer's interpreter: [`BasicInterpreter`], with the `aload`s at the node positions of
/// `adjusted` loading the value given there.
struct CastInterpreter {
    basic: BasicInterpreter,
    adjusted: HashMap<usize, BasicValue>,
}

impl Interpreter for CastInterpreter {
    type V = BasicValue;

    fn new_value(&mut self, ty: Option<&str>) -> Option<BasicValue> {
        self.basic.new_value(ty)
    }

    fn new_operation(&mut self, at: &At) -> Result<BasicValue, AnalyzerError> {
        self.basic.new_operation(at)
    }

    fn copy_operation(&mut self, at: &At, value: &BasicValue) -> Result<BasicValue, AnalyzerError> {
        match self.adjusted.get(&at.index) {
            Some(adjusted) => Ok(adjusted.clone()),
            None => self.basic.copy_operation(at, value),
        }
    }

    fn unary_operation(
        &mut self,
        at: &At,
        value: &BasicValue,
    ) -> Result<Option<BasicValue>, AnalyzerError> {
        self.basic.unary_operation(at, value)
    }

    fn binary_operation(
        &mut self,
        at: &At,
        first: &BasicValue,
        second: &BasicValue,
    ) -> Result<Option<BasicValue>, AnalyzerError> {
        self.basic.binary_operation(at, first, second)
    }

    fn ternary_operation(
        &mut self,
        at: &At,
        first: &BasicValue,
        second: &BasicValue,
        third: &BasicValue,
    ) -> Result<Option<BasicValue>, AnalyzerError> {
        self.basic.ternary_operation(at, first, second, third)
    }

    fn nary_operation(
        &mut self,
        at: &At,
        values: &[BasicValue],
    ) -> Result<Option<BasicValue>, AnalyzerError> {
        self.basic.nary_operation(at, values)
    }

    fn return_operation(
        &mut self,
        at: &At,
        value: &BasicValue,
        expected: &BasicValue,
    ) -> Result<(), AnalyzerError> {
        self.basic.return_operation(at, value, expected)
    }

    fn merge(&mut self, first: &BasicValue, second: &BasicValue) -> BasicValue {
        self.basic.merge(first, second)
    }
}

/// `getTypeAdjustmentForALoadInstructions`: per `aload` of a local inside that local's range that
/// the entry does not reach without a handler, the local's declared type. A later local of the
/// table wins over an earlier one. `None` for a body the liveness analysis does not model.
fn aload_adjustments(method: &MethodNode) -> Option<HashMap<usize, BasicValue>> {
    let live = dead_code::live_instructions(method, false)?;
    let positions = LabelPositions::of(method);
    let mut adjusted = HashMap::new();
    for local in &method.local_variables {
        let range = positions.at(local.start)..positions.at(local.end);
        for (at, (node, live)) in method.nodes.iter().zip(&live).enumerate() {
            let loads_local = matches!(
                node,
                Node::Insn(Insn::Var { op: ALOAD, slot }) if *slot == local.slot
            );
            if range.contains(&at) && loads_local && !live {
                adjusted.insert(at, BasicValue::of_descriptor(&local.desc)?);
            }
        }
    }
    Some(adjusted)
}

/// Whether a cast to `class` (an internal name, or an array descriptor) of `value` is redundant:
/// `value` is `null` or of exactly that type, and the type is not a multi-dimensional array.
fn is_redundant(class: &str, value: &BasicValue) -> bool {
    let target = descriptors::of_internal_name(class);
    let trivial = *value == BasicValue::Null || value.descriptor() == Some(target.as_str());
    trivial && !target.starts_with("[[")
}

/// Run the pass over `method`, a member of `owner`; `false` when no cast goes.
pub(crate) fn eliminate(method: &mut MethodNode, owner: &str) -> Result<bool, AnalyzerError> {
    let has_cast = method
        .instructions()
        .any(|insn| matches!(insn, Insn::Type { op: CHECKCAST, .. }));
    if !has_cast || method.instructions().any(is_reified_marker) {
        return Ok(false);
    }
    let Some(adjusted) = aload_adjustments(method) else {
        return Err(AnalyzerError {
            index: 0,
            message: "subroutines are not supported".to_string(),
        });
    };
    let mut interpreter = CastInterpreter {
        basic: BasicInterpreter,
        adjusted,
    };
    let options = AnalyzerOptions {
        prune_exception_edges: true,
        ..AnalyzerOptions::default()
    };
    let frames = analyze_with(method, owner, &mut interpreter, &mut PlainFrames, options)?;
    let redundant: Vec<bool> = method
        .nodes
        .iter()
        .zip(&frames)
        .map(|(node, frame)| match (node, frame) {
            (
                Node::Insn(Insn::Type {
                    op: CHECKCAST,
                    class,
                }),
                Some(frame),
            ) => frame
                .stack
                .last()
                .is_some_and(|top| is_redundant(class, top)),
            _ => false,
        })
        .collect();
    if !redundant.contains(&true) {
        return Ok(false);
    }
    let mut at = 0;
    method.nodes.retain(|_| {
        at += 1;
        !redundant[at - 1]
    });
    Ok(true)
}

#[cfg(test)]
mod tests;
