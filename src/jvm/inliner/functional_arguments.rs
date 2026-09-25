//! Where an inlined body uses its inline lambdas: kotlinc's `FunctionalArgumentInterpreter` and
//! the part of `markPlacesForInlineAndRemoveInlinable` it drives.
//!
//! A lambda parameter is never a value of the inlined code. The analysis follows each lambda from
//! its parameter slot through loads, stores and `dup`s; every `invoke` whose receiver is one is
//! recorded, in order, for the lambda's body to replace, and the instructions that only moved the
//! lambda around are deleted. So is every instruction no path reaches.

use crate::jvm::bytecode_passes::analysis::{
    analyze_with, AnalyzerError, AnalyzerOptions, At, BasicInterpreter, BasicValue, Frame,
    Interpreter, PlainFrames, Value,
};
use crate::jvm::bytecode_passes::descriptors;
use crate::jvm::bytecode_passes::opcodes::*;
use crate::jvm::method_node::{Insn, MethodNode, Node};

use super::{InlineError, Parameters};

/// A value of the inlined body: a plain value, or one of the call's inline lambdas
/// (`FunctionalArgumentValue`, which keeps the plain value it stands for).
#[derive(Clone, Debug, PartialEq)]
enum ArgumentValue {
    Basic(BasicValue),
    Lambda(usize, BasicValue),
}

impl ArgumentValue {
    fn basic(&self) -> &BasicValue {
        match self {
            ArgumentValue::Basic(value) | ArgumentValue::Lambda(_, value) => value,
        }
    }

    fn lambda(&self) -> Option<usize> {
        match self {
            ArgumentValue::Lambda(lambda, _) => Some(*lambda),
            ArgumentValue::Basic(_) => None,
        }
    }
}

impl Value for ArgumentValue {
    fn size(&self) -> usize {
        self.basic().size()
    }
}

struct FunctionalArgumentInterpreter<'a> {
    parameters: &'a Parameters,
    basic: BasicInterpreter,
}

fn plain(value: Option<BasicValue>) -> Option<ArgumentValue> {
    value.map(ArgumentValue::Basic)
}

impl Interpreter for FunctionalArgumentInterpreter<'_> {
    type V = ArgumentValue;

    fn new_value(&mut self, ty: Option<&str>) -> Option<ArgumentValue> {
        plain(self.basic.new_value(ty))
    }

    fn new_parameter_value(&mut self, slot: usize, ty: &str) -> ArgumentValue {
        let value = self.basic.new_parameter_value(slot, ty);
        match self.parameters.lambda_at(slot) {
            Some(lambda) => ArgumentValue::Lambda(lambda, value),
            None => ArgumentValue::Basic(value),
        }
    }

    fn new_operation(&mut self, at: &At) -> Result<ArgumentValue, AnalyzerError> {
        self.basic.new_operation(at).map(ArgumentValue::Basic)
    }

    fn copy_operation(
        &mut self,
        _at: &At,
        value: &ArgumentValue,
    ) -> Result<ArgumentValue, AnalyzerError> {
        Ok(value.clone())
    }

    fn unary_operation(
        &mut self,
        at: &At,
        value: &ArgumentValue,
    ) -> Result<Option<ArgumentValue>, AnalyzerError> {
        Ok(plain(self.basic.unary_operation(at, value.basic())?))
    }

    fn binary_operation(
        &mut self,
        at: &At,
        first: &ArgumentValue,
        second: &ArgumentValue,
    ) -> Result<Option<ArgumentValue>, AnalyzerError> {
        Ok(plain(self.basic.binary_operation(
            at,
            first.basic(),
            second.basic(),
        )?))
    }

    fn ternary_operation(
        &mut self,
        at: &At,
        first: &ArgumentValue,
        second: &ArgumentValue,
        third: &ArgumentValue,
    ) -> Result<Option<ArgumentValue>, AnalyzerError> {
        Ok(plain(self.basic.ternary_operation(
            at,
            first.basic(),
            second.basic(),
            third.basic(),
        )?))
    }

    fn nary_operation(
        &mut self,
        at: &At,
        values: &[ArgumentValue],
    ) -> Result<Option<ArgumentValue>, AnalyzerError> {
        let values: Vec<BasicValue> = values.iter().map(|value| value.basic().clone()).collect();
        Ok(plain(self.basic.nary_operation(at, &values)?))
    }

    fn return_operation(
        &mut self,
        _at: &At,
        _value: &ArgumentValue,
        _expected: &ArgumentValue,
    ) -> Result<(), AnalyzerError> {
        Ok(())
    }

    fn merge(&mut self, first: &ArgumentValue, second: &ArgumentValue) -> ArgumentValue {
        match (first, second) {
            (ArgumentValue::Lambda(a, _), ArgumentValue::Lambda(b, _)) if a == b => first.clone(),
            _ => ArgumentValue::Basic(self.basic.merge(first.basic(), second.basic())),
        }
    }
}

/// `isInvokeOnLambda`: `invoke` of a `kotlin/jvm/functions/FunctionN`.
pub(super) fn is_invoke_on_lambda(insn: &Insn) -> bool {
    matches!(insn, Insn::Method { name, owner, .. }
        if name == "invoke"
            && owner
                .strip_prefix("kotlin/jvm/functions/Function")
                .is_some_and(|arity| !arity.is_empty() && arity.bytes().all(|b| b.is_ascii_digit())))
}

/// `isAloadBeforeCheckParameterIsNotNull`: the `aload` of a parameter null check, which
/// `removeClosureAssertions` removes with its check.
fn is_load_before_parameter_check(nodes: &[Node], at: usize) -> bool {
    let is_check = |node: Option<&Node>| {
        matches!(node, Some(Node::Insn(Insn::Method { op: INVOKESTATIC, owner, name, .. }))
            if owner == "kotlin/jvm/internal/Intrinsics"
                && matches!(name.as_str(), "checkParameterIsNotNull" | "checkNotNullParameter"))
    };
    matches!(nodes[at], Node::Insn(Insn::Var { op: ALOAD, .. }))
        && matches!(nodes.get(at + 1), Some(Node::Insn(Insn::Ldc(_))))
        && is_check(nodes.get(at + 2))
}

fn top(frame: Option<&Frame<ArgumentValue>>, depth: usize) -> Option<&ArgumentValue> {
    let stack = &frame?.stack;
    stack.len().checked_sub(depth + 1).map(|at| &stack[at])
}

/// `markPlacesForInlineAndRemoveInlinable` for a body whose anonymous objects, SAM wrappers and
/// reification markers were already turned away: analyze it with the call's lambdas in their
/// parameter slots, delete what only moves a lambda, its `pop`s and the unreachable code, and
/// return the lambda each `FunctionN.invoke` calls, in order (`None` for any other function value,
/// whose `invoke` stays a call).
pub(super) fn mark_places(
    node: &mut MethodNode,
    parameters: &Parameters,
) -> Result<Vec<Option<usize>>, InlineError> {
    let mut interpreter = FunctionalArgumentInterpreter {
        parameters,
        basic: BasicInterpreter,
    };
    let options = AnalyzerOptions {
        prune_exception_edges: true,
        ..AnalyzerOptions::default()
    };
    let frames = analyze_with(node, "fake", &mut interpreter, &mut PlainFrames, options)
        .map_err(|error| InlineError::Analysis(error.message))?;
    let lambda_after = |at: usize| top(frames.get(at + 1)?.as_ref(), 0)?.lambda();
    let lambda_before = |at: usize, depth: usize| top(frames[at].as_ref(), depth)?.lambda();

    let mut delete = vec![false; node.nodes.len()];
    // `markObsoleteInstruction`.
    for (at, entry) in node.nodes.iter().enumerate() {
        let Node::Insn(insn) = entry else {
            continue;
        };
        if is_load_before_parameter_check(&node.nodes, at) {
            continue;
        }
        let op = crate::jvm::bytecode_passes::analysis::opcode(insn);
        delete[at] = match op {
            GETFIELD | GETSTATIC | ALOAD => lambda_after(at).is_some(),
            PUTFIELD | PUTSTATIC | ASTORE => lambda_before(at, 0).is_some(),
            SWAP => lambda_before(at, 0).is_some() || lambda_before(at, 1).is_some(),
            _ => false,
        };
    }
    let mut invokes = Vec::new();
    for (at, entry) in node.nodes.iter().enumerate() {
        let Some(frame) = &frames[at] else {
            // Unreachable: the instruction and its line number go; a label stays, since a
            // handler's range may start there.
            if !matches!(entry, Node::Label(_)) {
                delete[at] = true;
            }
            continue;
        };
        match entry {
            Node::Insn(insn @ Insn::Method { desc, .. }) if is_invoke_on_lambda(insn) => {
                let arguments = descriptors::argument_types(desc)
                    .ok_or_else(|| InlineError::Analysis(format!("malformed descriptor {desc}")))?
                    .len();
                invokes.push(top(Some(frame), arguments).and_then(ArgumentValue::lambda));
            }
            Node::Insn(Insn::Op(POP)) => {
                if top(Some(frame), 0)
                    .and_then(ArgumentValue::lambda)
                    .is_some()
                {
                    delete[at] = true;
                }
            }
            _ => {}
        }
    }
    let mut at = 0;
    node.nodes.retain(|_| {
        at += 1;
        !delete[at - 1]
    });
    super::preparation::remove_empty_try_catch_blocks(node);
    Ok(invokes)
}
