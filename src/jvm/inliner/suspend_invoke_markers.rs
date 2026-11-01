//! kotlinc's `surroundInvokesWithSuspendMarkersIfNeeded`: an `invoke` of a function value that an
//! inline suspend parameter took, and that the call does not inline, is a suspension point of the
//! code it is inlined into. A `$$forInline` body leaves such an invoke unmarked, since its compiler
//! cannot know whether the call inlines the lambda, so the inliner marks it once it knows it does
//! not.
//!
//! The invoke is surrounded by suspend markers, and the code from the first instruction that loads
//! its function value to the invoke is bracketed as an inline call, so FixStack saves the operand
//! stack around the whole of it.

use crate::jvm::bytecode_passes::analysis::{
    analyze, AnalyzerError, At, BasicInterpreter, BasicValue, Interpreter, Value,
};
use crate::jvm::bytecode_passes::coroutines::markers::{inline_call_marker, mark, SuspendMarker};
use crate::jvm::bytecode_passes::descriptors;
use crate::jvm::bytecode_passes::opcodes::*;
use crate::jvm::method_node::{Insn, MethodNode, Node};

use super::functional_arguments::is_invoke_on_lambda;
use super::InlineError;

/// Surround each `FunctionN.invoke` in `node` whose function value is read from one of `fields` of
/// `owner` with suspend markers, and bracket it from the first instruction of that read as an
/// inline call. The fields hold function values of inline suspend parameters the call does not
/// inline (kotlinc's `NonInlineArgumentForInlineSuspendParameter.OTHER`).
pub(super) fn surround_field_invokes(
    node: &mut MethodNode,
    owner: &str,
    fields: &[String],
) -> Result<(), InlineError> {
    if fields.is_empty() {
        return Ok(());
    }
    let frames = analyze(
        node,
        "fake",
        &mut CapturedLambdaInterpreter {
            owner,
            fields,
            nodes: &node.nodes,
        },
    )
    .map_err(|error| InlineError::Analysis(error.message))?;
    // Each marked invoke, with the instruction its function value's read starts at.
    let mut marked: Vec<(usize, usize)> = Vec::new();
    for (at, entry) in node.nodes.iter().enumerate() {
        let Node::Insn(insn @ Insn::Method { desc, .. }) = entry else {
            continue;
        };
        if !is_invoke_on_lambda(insn) {
            continue;
        }
        // Unreachable code is dead, whatever it invokes.
        let Some(frame) = frames.get(at) else {
            continue;
        };
        let arguments = descriptors::argument_types(desc)
            .ok_or_else(|| InlineError::Analysis(format!("a malformed descriptor {desc}")))?
            .len();
        let receiver = frame
            .stack
            .len()
            .checked_sub(arguments + 1)
            .map(|index| &frame.stack[index]);
        if let Some(CapturedValue::Load { start, .. }) = receiver {
            marked.push((*start, at));
        }
    }
    if marked.is_empty() {
        return Ok(());
    }
    let mut nodes = Vec::with_capacity(node.nodes.len() + marked.len() * 6);
    for (at, entry) in std::mem::take(&mut node.nodes).into_iter().enumerate() {
        for _ in marked.iter().filter(|&&(start, _)| start == at) {
            nodes.push(inline_call_marker(true));
        }
        let invoke = marked.iter().any(|&(_, invoke)| invoke == at);
        if invoke {
            nodes.extend(mark(SuspendMarker::BeforeSuspend));
        }
        nodes.push(entry);
        if invoke {
            nodes.extend(mark(SuspendMarker::AfterSuspend));
            nodes.push(inline_call_marker(false));
        }
    }
    node.nodes = nodes;
    Ok(())
}

/// A value of the analyzed body: a plain value, or a function value read from one of the marked
/// fields, with the instruction its read starts at (kotlinc's `PossibleLambdaLoad`).
#[derive(Clone, Debug, PartialEq)]
enum CapturedValue {
    Basic(BasicValue),
    Load { start: usize, value: BasicValue },
}

impl CapturedValue {
    fn basic(&self) -> &BasicValue {
        match self {
            CapturedValue::Basic(value) | CapturedValue::Load { value, .. } => value,
        }
    }
}

impl Value for CapturedValue {
    fn size(&self) -> usize {
        self.basic().size()
    }
}

fn plain(value: Option<BasicValue>) -> Option<CapturedValue> {
    value.map(CapturedValue::Basic)
}

/// kotlinc's `CapturedLambdaInterpreter`, following the marked fields' function values. A read
/// of one starts at the instruction before its chain of `getfield`s; an `aload` of a local that
/// holds one starts a read of its own there.
struct CapturedLambdaInterpreter<'a> {
    owner: &'a str,
    fields: &'a [String],
    nodes: &'a [Node],
}

impl CapturedLambdaInterpreter<'_> {
    /// The node a read ending in the `getfield` at `at` starts at: kotlinc walks back over the
    /// chain of `getfield`s to the node before it.
    fn chain_start(&self, at: usize) -> usize {
        let mut start = at;
        while start > 0
            && matches!(
                self.nodes[start],
                Node::Insn(Insn::Field { op: GETFIELD, .. })
            )
        {
            start -= 1;
        }
        start
    }
}

impl Interpreter for CapturedLambdaInterpreter<'_> {
    type V = CapturedValue;

    fn new_value(&mut self, ty: Option<&str>) -> Option<CapturedValue> {
        plain(BasicInterpreter.new_value(ty))
    }

    fn new_operation(&mut self, at: &At) -> Result<CapturedValue, AnalyzerError> {
        BasicInterpreter.new_operation(at).map(CapturedValue::Basic)
    }

    fn copy_operation(
        &mut self,
        at: &At,
        value: &CapturedValue,
    ) -> Result<CapturedValue, AnalyzerError> {
        Ok(match (at.insn, value) {
            (Insn::Var { op: ALOAD, .. }, CapturedValue::Load { value, .. }) => {
                CapturedValue::Load {
                    start: at.index,
                    value: value.clone(),
                }
            }
            _ => value.clone(),
        })
    }

    fn unary_operation(
        &mut self,
        at: &At,
        value: &CapturedValue,
    ) -> Result<Option<CapturedValue>, AnalyzerError> {
        let result = BasicInterpreter.unary_operation(at, value.basic())?;
        Ok(match (at.insn, result) {
            (
                Insn::Field {
                    op: GETFIELD,
                    owner,
                    name,
                    ..
                },
                Some(result),
            ) if owner == self.owner && self.fields.iter().any(|field| field == name) => {
                Some(CapturedValue::Load {
                    start: self.chain_start(at.index),
                    value: result,
                })
            }
            (_, result) => plain(result),
        })
    }

    fn binary_operation(
        &mut self,
        at: &At,
        first: &CapturedValue,
        second: &CapturedValue,
    ) -> Result<Option<CapturedValue>, AnalyzerError> {
        Ok(plain(BasicInterpreter.binary_operation(
            at,
            first.basic(),
            second.basic(),
        )?))
    }

    fn ternary_operation(
        &mut self,
        at: &At,
        first: &CapturedValue,
        second: &CapturedValue,
        third: &CapturedValue,
    ) -> Result<Option<CapturedValue>, AnalyzerError> {
        Ok(plain(BasicInterpreter.ternary_operation(
            at,
            first.basic(),
            second.basic(),
            third.basic(),
        )?))
    }

    fn nary_operation(
        &mut self,
        at: &At,
        values: &[CapturedValue],
    ) -> Result<Option<CapturedValue>, AnalyzerError> {
        let values: Vec<BasicValue> = values.iter().map(|value| value.basic().clone()).collect();
        Ok(plain(BasicInterpreter.nary_operation(at, &values)?))
    }

    fn return_operation(
        &mut self,
        _at: &At,
        _value: &CapturedValue,
        _expected: &CapturedValue,
    ) -> Result<(), AnalyzerError> {
        Ok(())
    }

    fn merge(&mut self, first: &CapturedValue, second: &CapturedValue) -> CapturedValue {
        if first == second {
            return first.clone();
        }
        CapturedValue::Basic(BasicInterpreter.merge(first.basic(), second.basic()))
    }
}
