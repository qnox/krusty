//! kotlinc's `NullabilityInterpreter` (`codegen/optimization/nullCheck/`) over its
//! `OptimizationBasicInterpreter`: every value is the basic interpreter's, except that a value
//! known not to be `null` or known to be `null` says so.
//!
//! A value is known not to be `null` when it was just made: `new`, a new array, a reference `ldc`,
//! `Unit.INSTANCE`, a boxing call, a progression's iterator or that iterator's `next()`. A `checkcast`
//! keeps its operand's value (type included), except the placeholder cast of a reified `as?`, which
//! may yield `null`. `aconst_null` is known `null` unless it is the placeholder of a reified
//! `typeOf`. The assumptions the transformer injects (see `assumptions`) add one more source: the
//! `AS_NOT_NULL` pseudo instruction, whose result is its operand, known not to be `null`.

use std::collections::BTreeSet;

use super::super::analysis::{
    merge_references, opcode, AnalyzerError, At, BasicInterpreter, BasicValue, Interpreter, Value,
};
use super::super::opcodes::*;
use super::super::redundant_boxing::{self, ValueClasses};
use crate::jvm::method_node::Insn;

const OBJECT: &str = "Ljava/lang/Object;";

/// kotlinc's value of the nullability analysis.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum NullValue {
    /// A plain `StrictBasicValue`: whether it is `null` is not known.
    Basic(BasicValue),
    /// `NotNullBasicValue`, by descriptor.
    NotNull(String),
    /// `NullBasicValue`: typed `Object`, and `null`.
    Null,
    /// `ProgressionIteratorBasicValue`, by the iterator's descriptor.
    ProgressionIterator(&'static str),
}

/// kotlinc's `Nullability` of a value (`getNullability`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Nullability {
    Null,
    NotNull,
    Nullable,
}

impl NullValue {
    /// The value's JVM type as a descriptor, `None` for an uninitialized slot.
    pub(super) fn descriptor(&self) -> Option<&str> {
        match self {
            NullValue::Basic(value) => value.descriptor(),
            NullValue::NotNull(descriptor) => Some(descriptor),
            NullValue::Null => Some(OBJECT),
            NullValue::ProgressionIterator(descriptor) => Some(descriptor),
        }
    }

    pub(super) fn nullability(&self) -> Nullability {
        match self {
            NullValue::Null => Nullability::Null,
            NullValue::NotNull(_) | NullValue::ProgressionIterator(_) => Nullability::NotNull,
            NullValue::Basic(_) => Nullability::Nullable,
        }
    }

    /// The value as the basic interpreter sees it: a reference of its type.
    fn basic(&self) -> BasicValue {
        match self {
            NullValue::Basic(value) => value.clone(),
            _ => BasicValue::Reference(
                self.descriptor()
                    .expect("a known-nullability value has a type")
                    .to_string(),
            ),
        }
    }

    fn is_reference(&self) -> bool {
        match self {
            NullValue::Basic(value) => value.is_reference(),
            _ => true,
        }
    }
}

impl Value for NullValue {
    fn size(&self) -> usize {
        match self {
            NullValue::Basic(value) => value.size(),
            _ => 1,
        }
    }
}

/// The instructions whose meaning the nullability analysis takes from where they stand rather
/// than from the instruction itself, by node index of the analyzed method.
#[derive(Default)]
pub(super) struct Placements {
    /// `aconst_null`s right after a reified `typeOf` marker: a placeholder, not a `null`.
    pub(super) type_of_placeholders: BTreeSet<usize>,
    /// `checkcast`s right after a reified `as?` marker: their result may be `null`.
    pub(super) reified_safe_casts: BTreeSet<usize>,
    /// The `AS_NOT_NULL` pseudo instructions the transformer injected.
    pub(super) as_not_null: BTreeSet<usize>,
}

/// kotlinc's `NullabilityInterpreter`.
pub(super) struct NullabilityInterpreter<'a> {
    basic: BasicInterpreter,
    value_classes: &'a dyn ValueClasses,
    placements: Placements,
}

impl<'a> NullabilityInterpreter<'a> {
    pub(super) fn new(value_classes: &'a dyn ValueClasses, placements: Placements) -> Self {
        NullabilityInterpreter {
            basic: BasicInterpreter,
            value_classes,
            placements,
        }
    }
}

fn not_null(value: &BasicValue) -> NullValue {
    NullValue::NotNull(value.descriptor().unwrap_or(OBJECT).to_string())
}

/// `isUnitInstance`: `getstatic kotlin/Unit.INSTANCE`.
fn is_unit_instance(insn: &Insn) -> bool {
    matches!(insn, Insn::Field { op: GETSTATIC, owner, name, .. }
        if owner == "kotlin/Unit" && name == "INSTANCE")
}

/// `isNextMethodCallOfProgressionIterator`: an interface call of `next` on a progression's iterator.
fn is_next_of_progression_iterator(insn: &Insn, values: &[NullValue]) -> bool {
    matches!(values.first(), Some(NullValue::ProgressionIterator(_)))
        && matches!(insn, Insn::Method { op: INVOKEINTERFACE, name, .. } if name == "next")
}

impl Interpreter for NullabilityInterpreter<'_> {
    type V = NullValue;

    fn new_value(&mut self, ty: Option<&str>) -> Option<NullValue> {
        self.basic.new_value(ty).map(NullValue::Basic)
    }

    fn new_operation(&mut self, at: &At) -> Result<NullValue, AnalyzerError> {
        let value = self.basic.new_operation(at)?;
        Ok(match opcode(at.insn) {
            ACONST_NULL if !self.placements.type_of_placeholders.contains(&at.index) => {
                NullValue::Null
            }
            NEW => not_null(&value),
            LDC if value.is_reference() => not_null(&value),
            _ if is_unit_instance(at.insn) => not_null(&value),
            _ => NullValue::Basic(value),
        })
    }

    fn copy_operation(&mut self, _at: &At, value: &NullValue) -> Result<NullValue, AnalyzerError> {
        Ok(value.clone())
    }

    fn unary_operation(
        &mut self,
        at: &At,
        value: &NullValue,
    ) -> Result<Option<NullValue>, AnalyzerError> {
        let result = self.basic.unary_operation(at, &value.basic())?;
        Ok(match opcode(at.insn) {
            NEWARRAY | ANEWARRAY => result.as_ref().map(not_null),
            CHECKCAST if self.placements.reified_safe_casts.contains(&at.index) => result
                .and_then(|cast| cast.descriptor().and_then(BasicValue::of_descriptor))
                .map(NullValue::Basic),
            CHECKCAST => Some(value.clone()),
            _ => result.map(NullValue::Basic),
        })
    }

    fn binary_operation(
        &mut self,
        at: &At,
        first: &NullValue,
        second: &NullValue,
    ) -> Result<Option<NullValue>, AnalyzerError> {
        Ok(self
            .basic
            .binary_operation(at, &first.basic(), &second.basic())?
            .map(NullValue::Basic))
    }

    fn ternary_operation(
        &mut self,
        _at: &At,
        _first: &NullValue,
        _second: &NullValue,
        _third: &NullValue,
    ) -> Result<Option<NullValue>, AnalyzerError> {
        Ok(None)
    }

    fn nary_operation(
        &mut self,
        at: &At,
        values: &[NullValue],
    ) -> Result<Option<NullValue>, AnalyzerError> {
        let basic: Vec<BasicValue> = values.iter().map(NullValue::basic).collect();
        let Some(result) = self.basic.nary_operation(at, &basic)? else {
            return Ok(None);
        };
        if redundant_boxing::is_boxing(at.insn, self.value_classes) {
            return Ok(Some(not_null(&result)));
        }
        if redundant_boxing::is_iterator_call(at.insn) {
            if let Some(iterator) = values
                .first()
                .and_then(NullValue::descriptor)
                .and_then(redundant_boxing::progression_iterator)
            {
                return Ok(Some(NullValue::ProgressionIterator(iterator)));
            }
        }
        if is_next_of_progression_iterator(at.insn, values) {
            return Ok(Some(not_null(&result)));
        }
        if self.placements.as_not_null.contains(&at.index) {
            let operand = values
                .first()
                .and_then(NullValue::descriptor)
                .ok_or_else(|| AnalyzerError {
                    index: at.index,
                    message: "a non-null assumption about a slot holding no value".to_string(),
                })?;
            return Ok(Some(NullValue::NotNull(operand.to_string())));
        }
        Ok(Some(NullValue::Basic(result)))
    }

    fn return_operation(
        &mut self,
        _at: &At,
        _value: &NullValue,
        _expected: &NullValue,
    ) -> Result<(), AnalyzerError> {
        Ok(())
    }

    fn merge(&mut self, v: &NullValue, w: &NullValue) -> NullValue {
        use NullValue::{Null, ProgressionIterator};
        match (v, w) {
            (Null, Null) => Null,
            (Null, _) | (_, Null) => NullValue::Basic(BasicValue::Reference(OBJECT.to_string())),
            (ProgressionIterator(a), ProgressionIterator(b)) if a == b => v.clone(),
            (ProgressionIterator(_) | NullValue::NotNull(_), ProgressionIterator(_))
            | (ProgressionIterator(_), NullValue::NotNull(_)) => {
                NullValue::NotNull(OBJECT.to_string())
            }
            (NullValue::NotNull(a), NullValue::NotNull(b)) => {
                if a == b {
                    v.clone()
                } else {
                    NullValue::NotNull(OBJECT.to_string())
                }
            }
            (NullValue::Basic(a), NullValue::Basic(b)) => NullValue::Basic(self.basic.merge(a, b)),
            _ => NullValue::Basic(merge_unequal(v, w)),
        }
    }
}

/// `OptimizationBasicInterpreter.merge` of a value of known nullability and a value of another
/// kind: never equal, so two references meet at their common type even when it is the same type.
fn merge_unequal(v: &NullValue, w: &NullValue) -> BasicValue {
    let uninitialized =
        |value: &NullValue| matches!(value, NullValue::Basic(BasicValue::Uninitialized));
    if uninitialized(v) || uninitialized(w) || !v.is_reference() || !w.is_reference() {
        return BasicValue::Uninitialized;
    }
    let (Some(v_type), Some(w_type)) = (v.descriptor(), w.descriptor()) else {
        return BasicValue::Uninitialized;
    };
    match (v, w) {
        // `newValue` of the other's type: a plain value of it.
        (NullValue::Basic(BasicValue::Null), _) => {
            BasicValue::of_descriptor(w_type).unwrap_or(BasicValue::Uninitialized)
        }
        (_, NullValue::Basic(BasicValue::Null)) => {
            BasicValue::of_descriptor(v_type).unwrap_or(BasicValue::Uninitialized)
        }
        _ => merge_references(w_type, v_type),
    }
}
