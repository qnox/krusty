//! kotlinc's `ConstantPropagationInterpreter` and `IConstValue`: the basic interpreter
//! ([`BasicInterpreter`], kotlinc's `OptimizationBasicInterpreter`), except that an `int` pushed by
//! a constant instruction keeps its value. Loads, stores and `dup`s copy a value as it is, so the
//! constant travels through locals; every other instruction makes a plain `int` of it.

use super::super::analysis::{AnalyzerError, At, BasicInterpreter, BasicValue, Interpreter, Value};
use super::super::opcodes::*;
use crate::jvm::method_node::{Constant, Insn};

/// A [`BasicValue`], or an `int` whose value is known.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum ConstValue {
    /// `IConstValue`: an `int` known to hold this value.
    Int(i32),
    Basic(BasicValue),
}

impl ConstValue {
    /// The value as the basic interpreter sees it: a known `int` is an `int`.
    fn basic(&self) -> BasicValue {
        match self {
            ConstValue::Int(_) => BasicValue::Int,
            ConstValue::Basic(value) => value.clone(),
        }
    }

    /// The known `int` value, if any.
    pub(super) fn known(&self) -> Option<i32> {
        match self {
            ConstValue::Int(value) => Some(*value),
            ConstValue::Basic(_) => None,
        }
    }
}

impl Value for ConstValue {
    fn size(&self) -> usize {
        match self {
            ConstValue::Int(_) => 1,
            ConstValue::Basic(value) => value.size(),
        }
    }
}

/// The `int` a constant instruction pushes: kotlinc's `isIntConst` (`iconst_m1`…`iconst_5`,
/// `bipush`, `sipush`, and `ldc` of an `Integer`).
pub(super) fn int_constant(insn: &Insn) -> Option<i32> {
    match insn {
        Insn::Op(op @ ICONST_M1..=ICONST_5) => Some(i32::from(*op) - i32::from(ICONST_0)),
        Insn::Int {
            op: BIPUSH | SIPUSH,
            operand,
        } => Some(*operand),
        Insn::Ldc(Constant::Int(value)) => Some(*value),
        _ => None,
    }
}

/// kotlinc's `ConstantPropagationInterpreter`.
pub(super) struct ConstantPropagationInterpreter;

fn basic(value: Option<BasicValue>) -> Option<ConstValue> {
    value.map(ConstValue::Basic)
}

impl Interpreter for ConstantPropagationInterpreter {
    type V = ConstValue;

    fn new_value(&mut self, ty: Option<&str>) -> Option<ConstValue> {
        basic(BasicInterpreter.new_value(ty))
    }

    fn new_operation(&mut self, at: &At) -> Result<ConstValue, AnalyzerError> {
        match int_constant(at.insn) {
            Some(value) => Ok(ConstValue::Int(value)),
            None => BasicInterpreter.new_operation(at).map(ConstValue::Basic),
        }
    }

    fn copy_operation(
        &mut self,
        _at: &At,
        value: &ConstValue,
    ) -> Result<ConstValue, AnalyzerError> {
        Ok(value.clone())
    }

    fn unary_operation(
        &mut self,
        at: &At,
        value: &ConstValue,
    ) -> Result<Option<ConstValue>, AnalyzerError> {
        BasicInterpreter
            .unary_operation(at, &value.basic())
            .map(basic)
    }

    fn binary_operation(
        &mut self,
        at: &At,
        first: &ConstValue,
        second: &ConstValue,
    ) -> Result<Option<ConstValue>, AnalyzerError> {
        BasicInterpreter
            .binary_operation(at, &first.basic(), &second.basic())
            .map(basic)
    }

    fn ternary_operation(
        &mut self,
        at: &At,
        first: &ConstValue,
        second: &ConstValue,
        third: &ConstValue,
    ) -> Result<Option<ConstValue>, AnalyzerError> {
        BasicInterpreter
            .ternary_operation(at, &first.basic(), &second.basic(), &third.basic())
            .map(basic)
    }

    fn nary_operation(
        &mut self,
        at: &At,
        values: &[ConstValue],
    ) -> Result<Option<ConstValue>, AnalyzerError> {
        let values: Vec<BasicValue> = values.iter().map(ConstValue::basic).collect();
        BasicInterpreter.nary_operation(at, &values).map(basic)
    }

    fn return_operation(
        &mut self,
        at: &At,
        value: &ConstValue,
        expected: &ConstValue,
    ) -> Result<(), AnalyzerError> {
        BasicInterpreter.return_operation(at, &value.basic(), &expected.basic())
    }

    /// Two equal known `int`s stay known; any other meet is the basic interpreter's, where a known
    /// `int` is an `int`.
    fn merge(&mut self, first: &ConstValue, second: &ConstValue) -> ConstValue {
        match (first, second) {
            (ConstValue::Int(a), ConstValue::Int(b)) if a == b => first.clone(),
            _ => ConstValue::Basic(BasicInterpreter.merge(&first.basic(), &second.basic())),
        }
    }
}
