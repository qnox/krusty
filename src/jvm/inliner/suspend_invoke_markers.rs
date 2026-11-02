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

/// Surround each `FunctionN.invoke` at `invokes` in `node` with suspend markers, and bracket it from
/// the first instruction of its function value's read as an inline call. Each invokes a function
/// value the call passes for an inline suspend parameter without inlining it, read from the inlined
/// body's own parameter (kotlinc's `markNoinlineLambdaIfSuspend` on a parameter's invoke).
pub(super) fn surround_invokes(
    node: &mut MethodNode,
    invokes: &[usize],
) -> Result<(), InlineError> {
    if invokes.is_empty() {
        return Ok(());
    }
    let receivers = invoke_receivers(node, None)?;
    let marked = receivers
        .into_iter()
        .filter(|&(invoke, _)| invokes.contains(&invoke))
        .filter_map(|(invoke, receiver)| Some((receiver?.start, invoke)))
        .collect();
    surround(node, marked);
    Ok(())
}

/// Surround each `FunctionN.invoke` in `node` whose function value is read from one of `fields` of
/// `owner` with suspend markers, and bracket it from the first instruction of that read as an
/// inline call. The fields hold function values of inline suspend parameters the call does not
/// inline (kotlinc's `NonInlineArgumentForInlineSuspendParameter.OTHER`), which a regenerated
/// object's methods read as their captured values.
pub(super) fn surround_field_invokes(
    node: &mut MethodNode,
    owner: &str,
    fields: &[String],
) -> Result<(), InlineError> {
    if fields.is_empty() {
        return Ok(());
    }
    let receivers = invoke_receivers(node, Some(owner))?;
    let marked = receivers
        .into_iter()
        .filter_map(|(invoke, receiver)| {
            let load = receiver?;
            let field = load.field?;
            fields.contains(&field).then_some((load.start, invoke))
        })
        .collect();
    surround(node, marked);
    Ok(())
}

/// The function value's read of each reachable `FunctionN.invoke` in `node`, by the invoke's index:
/// kotlinc's `CapturedLambdaInterpreter`. With `owner`, a read of one of its fields names the field.
fn invoke_receivers(
    node: &MethodNode,
    owner: Option<&str>,
) -> Result<Vec<(usize, Option<LambdaLoad>)>, InlineError> {
    let frames = analyze(
        node,
        "fake",
        &mut CapturedLambdaInterpreter {
            owner,
            nodes: &node.nodes,
        },
    )
    .map_err(|error| InlineError::Analysis(error.message))?;
    let mut receivers = Vec::new();
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
            .and_then(|index| match &frame.stack[index] {
                CapturedValue::Load(load) => Some(load.clone()),
                CapturedValue::Basic(_) => None,
            });
        receivers.push((at, receiver));
    }
    Ok(receivers)
}

/// Insert the markers around each `(start, invoke)` of `marked`.
fn surround(node: &mut MethodNode, marked: Vec<(usize, usize)>) {
    if marked.is_empty() {
        return;
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
}

/// A read of a function value (kotlinc's `PossibleLambdaLoad`): the instruction it starts at and,
/// for a read of one of the analyzed owner's fields, that field.
#[derive(Clone, Debug, PartialEq)]
struct LambdaLoad {
    start: usize,
    field: Option<String>,
    value: BasicValue,
}

/// A value of the analyzed body: a plain value, or a read of a function value.
#[derive(Clone, Debug, PartialEq)]
enum CapturedValue {
    Basic(BasicValue),
    Load(LambdaLoad),
}

impl CapturedValue {
    fn basic(&self) -> &BasicValue {
        match self {
            CapturedValue::Basic(value) | CapturedValue::Load(LambdaLoad { value, .. }) => value,
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

/// kotlinc's `CapturedLambdaInterpreter`. Every `aload` starts a read there; a `getfield` of a
/// captured function value starts one at the instruction before its chain of `getfield`s. An
/// `aload` of a local that holds a field's read keeps naming that field.
struct CapturedLambdaInterpreter<'a> {
    owner: Option<&'a str>,
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
        Ok(match at.insn {
            Insn::Var { op: ALOAD, .. } => CapturedValue::Load(LambdaLoad {
                start: at.index,
                field: match value {
                    CapturedValue::Load(load) => load.field.clone(),
                    CapturedValue::Basic(_) => None,
                },
                value: value.basic().clone(),
            }),
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
                    desc,
                },
                Some(result),
            ) if is_captured_function_field(name, desc) => Some(CapturedValue::Load(LambdaLoad {
                start: self.chain_start(at.index),
                field: (self.owner == Some(owner.as_str())).then(|| name.clone()),
                value: result,
            })),
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

/// kotlinc's `fieldLoad` for a `getfield`: a captured value (`isCapturedFieldName`) of a
/// `FunctionN` type.
fn is_captured_function_field(name: &str, desc: &str) -> bool {
    let function = desc
        .strip_prefix("Lkotlin/jvm/functions/Function")
        .and_then(|rest| rest.strip_suffix(';'))
        .is_some_and(|arity| !arity.is_empty() && arity.bytes().all(|b| b.is_ascii_digit()));
    function && (name.starts_with('$') || name == "this$0" || name == "receiver$0")
}
