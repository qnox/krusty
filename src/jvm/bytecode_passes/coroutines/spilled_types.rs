//! kotlinc's `SpilledVariableFieldTypesAnalysis.kt`: the frames that decide each spilled local's
//! field type. An `iload` whose value flows into a `boolean`, `byte`, `char` or `short` position is
//! first coerced to that type, so the local it came from is typed by its use.

use std::collections::{BTreeMap, BTreeSet};

use super::super::analysis::{
    analyze, AnalyzerError, At, BasicInterpreter, BasicValue, Frame, Interpreter, Value,
};
use super::super::descriptors;
use super::super::insn_list::EditableMethod;
use super::super::opcodes::*;
use super::CoroutineError;
use crate::jvm::method_node::{Insn, Node};

/// A basic value, or an `int` some `iload`s produced (`IloadedValue`).
#[derive(Clone, Debug, PartialEq, Eq)]
enum CoerceValue {
    Basic(BasicValue),
    Iloaded(BTreeSet<usize>),
}

impl Value for CoerceValue {
    fn size(&self) -> usize {
        match self {
            CoerceValue::Basic(value) => value.size(),
            CoerceValue::Iloaded(_) => 1,
        }
    }
}

impl CoerceValue {
    fn basic(&self) -> BasicValue {
        match self {
            CoerceValue::Basic(value) => value.clone(),
            CoerceValue::Iloaded(_) => BasicValue::Int,
        }
    }

    fn descriptor(&self) -> Option<String> {
        match self {
            CoerceValue::Basic(value) => value.descriptor().map(str::to_string),
            CoerceValue::Iloaded(_) => Some("I".to_string()),
        }
    }
}

/// kotlinc's `IntLikeCoerceInterpreter`.
#[derive(Default)]
struct IntLikeCoerceInterpreter {
    /// The `iload`s to coerce, by node index, with the int-like descriptor to coerce to.
    needs_coercion: BTreeMap<usize, &'static str>,
}

impl IntLikeCoerceInterpreter {
    fn coerce(&mut self, loads: &BTreeSet<usize>, descriptor: &'static str) {
        for load in loads {
            self.needs_coercion.insert(*load, descriptor);
        }
    }

    fn coerce_to(&mut self, value: &CoerceValue, descriptor: &str) {
        if let (CoerceValue::Iloaded(loads), Some(int_like)) =
            (value, descriptors::int_like(descriptor))
        {
            self.coerce(loads, int_like);
        }
    }
}

fn basic(value: Option<BasicValue>) -> Option<CoerceValue> {
    value.map(CoerceValue::Basic)
}

impl Interpreter for IntLikeCoerceInterpreter {
    type V = CoerceValue;

    fn new_value(&mut self, ty: Option<&str>) -> Option<CoerceValue> {
        basic(BasicInterpreter.new_value(ty))
    }

    fn new_operation(&mut self, at: &At) -> Result<CoerceValue, AnalyzerError> {
        Ok(CoerceValue::Basic(BasicInterpreter.new_operation(at)?))
    }

    fn copy_operation(
        &mut self,
        at: &At,
        value: &CoerceValue,
    ) -> Result<CoerceValue, AnalyzerError> {
        if let Insn::Var { op: ILOAD, .. } = at.insn {
            return Ok(CoerceValue::Iloaded(BTreeSet::from([at.index])));
        }
        // `BasicValue(value.type)`: a copy keeps the type alone, so `null` becomes `Object`.
        Ok(CoerceValue::Basic(match value.basic() {
            BasicValue::Null => BasicValue::Reference("Ljava/lang/Object;".to_string()),
            other => other,
        }))
    }

    fn unary_operation(
        &mut self,
        at: &At,
        value: &CoerceValue,
    ) -> Result<Option<CoerceValue>, AnalyzerError> {
        if let Insn::Field {
            op: PUTSTATIC,
            desc,
            ..
        } = at.insn
        {
            self.coerce_to(value, desc);
        }
        Ok(basic(BasicInterpreter.unary_operation(at, &value.basic())?))
    }

    fn binary_operation(
        &mut self,
        at: &At,
        first: &CoerceValue,
        second: &CoerceValue,
    ) -> Result<Option<CoerceValue>, AnalyzerError> {
        if let Insn::Field {
            op: PUTFIELD, desc, ..
        } = at.insn
        {
            self.coerce_to(second, desc);
        }
        Ok(basic(BasicInterpreter.binary_operation(
            at,
            &first.basic(),
            &second.basic(),
        )?))
    }

    fn ternary_operation(
        &mut self,
        at: &At,
        array: &CoerceValue,
        _index: &CoerceValue,
        value: &CoerceValue,
    ) -> Result<Option<CoerceValue>, AnalyzerError> {
        if let CoerceValue::Iloaded(loads) = value {
            let loads = loads.clone();
            match at.insn {
                Insn::Op(BASTORE) => {
                    let element = if array.descriptor().as_deref() == Some("[Z") {
                        "Z"
                    } else {
                        "B"
                    };
                    self.coerce(&loads, element);
                }
                Insn::Op(CASTORE) => self.coerce(&loads, "C"),
                Insn::Op(SASTORE) => self.coerce(&loads, "S"),
                _ => {}
            }
        }
        Ok(None)
    }

    fn nary_operation(
        &mut self,
        at: &At,
        values: &[CoerceValue],
    ) -> Result<Option<CoerceValue>, AnalyzerError> {
        let (descriptor, receiver) = match at.insn {
            Insn::InvokeDynamic { desc, .. } => (Some(desc), false),
            Insn::Method {
                op: INVOKESTATIC,
                desc,
                ..
            } => (Some(desc), false),
            Insn::Method { desc, .. } => (Some(desc), true),
            _ => (None, false),
        };
        if let Some(arguments) = descriptor.and_then(|desc| descriptors::argument_types(desc)) {
            let offset = usize::from(receiver);
            for (index, argument) in arguments.iter().enumerate() {
                if let Some(value) = values.get(index + offset) {
                    self.coerce_to(value, argument);
                }
            }
        }
        let values: Vec<BasicValue> = values.iter().map(CoerceValue::basic).collect();
        Ok(basic(BasicInterpreter.nary_operation(at, &values)?))
    }

    fn return_operation(
        &mut self,
        _at: &At,
        _value: &CoerceValue,
        _expected: &CoerceValue,
    ) -> Result<(), AnalyzerError> {
        Ok(())
    }

    fn merge(&mut self, first: &CoerceValue, second: &CoerceValue) -> CoerceValue {
        match (first, second) {
            (CoerceValue::Iloaded(a), CoerceValue::Iloaded(b)) => {
                let loads: BTreeSet<usize> = a.union(b).copied().collect();
                if let Some(descriptor) = loads
                    .iter()
                    .find_map(|load| self.needs_coercion.get(load).copied())
                {
                    self.coerce(a, descriptor);
                    self.coerce(b, descriptor);
                }
                CoerceValue::Iloaded(loads)
            }
            _ if first.descriptor() == second.descriptor() => match second {
                CoerceValue::Iloaded(_) => second.clone(),
                CoerceValue::Basic(_) => first.clone(),
            },
            _ => CoerceValue::Basic(BasicInterpreter.merge(&first.basic(), &second.basic())),
        }
    }
}

/// The instructions coercing an `int` on the stack to `descriptor` (`coerceInt`).
fn coercion(method: &mut EditableMethod, descriptor: &str) -> Vec<Node> {
    match descriptor {
        "Z" => {
            let zero = method.method.new_label();
            let done = method.method.new_label();
            vec![
                Node::Insn(Insn::Jump {
                    op: IFEQ,
                    target: zero,
                }),
                Node::Insn(Insn::Op(ICONST_1)),
                Node::Insn(Insn::Jump {
                    op: GOTO,
                    target: done,
                }),
                Node::Label(zero),
                Node::Insn(Insn::Op(ICONST_0)),
                Node::Label(done),
            ]
        }
        "B" => vec![Node::Insn(Insn::Op(I2B))],
        "C" => vec![Node::Insn(Insn::Op(I2C))],
        "S" => vec![Node::Insn(Insn::Op(I2S))],
        _ => Vec::new(),
    }
}

/// `performSpilledVariableFieldTypesAnalysis`: coerce the `iload`s whose use fixes an int-like
/// type, then the frames of the coerced body.
pub(crate) fn spilled_variable_field_types(
    method: &mut EditableMethod,
    owner: &str,
) -> Result<Vec<Option<Frame<BasicValue>>>, CoroutineError> {
    let snapshot = method.snapshot();
    let mut interpreter = IntLikeCoerceInterpreter::default();
    analyze(&snapshot, owner, &mut interpreter).map_err(CoroutineError::Analysis)?;
    let ids = method.insns.ids();
    for (load, descriptor) in interpreter.needs_coercion {
        let nodes = coercion(method, descriptor);
        method.insns.insert_after(Some(ids[load]), nodes);
    }
    let snapshot = method.snapshot();
    analyze(&snapshot, owner, &mut BasicInterpreter).map_err(CoroutineError::Analysis)
}
