//! kotlinc's `RedundantBoxingInterpreter` over its `BoxingInterpreter`
//! (`codegen/optimization/boxing/`): the basic interpreter, with a box for each boxing and a record
//! of everything done with it. A box stays removable while every use of it is one the rewrite can
//! redo on the unboxed value (a load, store, `pop`, `dup`, a safe cast, an unboxing or a
//! same-typed comparison), and while it only meets boxes of its own type where edges join.

use std::collections::{BTreeSet, HashMap};

use super::super::analysis::{AnalyzerError, At, BasicInterpreter, BasicValue, Interpreter};
use super::super::descriptors;
use super::super::opcodes::*;
use super::recognizers::{self, ValueClasses};
use super::values::{BoxId, BoxedValue, BoxingValue, Candidates, ProgressionIterator};
use crate::jvm::bytecode_passes::analysis::opcode;
use crate::jvm::method_node::Insn;

/// The operations a box may take part in and still be removed (`PERMITTED_OPERATIONS_OPCODES`).
const PERMITTED: [u8; 6] = [ASTORE, ALOAD, POP, DUP, CHECKCAST, INSTANCEOF];

pub(super) struct BoxingInterpreter<'a> {
    value_classes: &'a dyn ValueClasses,
    basic: BasicInterpreter,
    /// The box each boxing instruction makes, by node index (`boxingPlaces`).
    boxing_places: HashMap<usize, BoxId>,
    /// The progression iterator each `iterator()` call makes, by node index.
    progression_iterators: HashMap<usize, ProgressionIterator>,
    /// `iterator()` calls that were also reached with something other than a progression.
    pub(super) tainted_iterators: BTreeSet<usize>,
    pub(super) candidates: Candidates,
}

fn error(at: &At, message: String) -> AnalyzerError {
    AnalyzerError {
        index: at.index,
        message,
    }
}

impl<'a> BoxingInterpreter<'a> {
    pub(super) fn new(value_classes: &'a dyn ValueClasses) -> Self {
        BoxingInterpreter {
            value_classes,
            basic: BasicInterpreter,
            boxing_places: HashMap::new(),
            progression_iterators: HashMap::new(),
            tainted_iterators: BTreeSet::new(),
            candidates: Candidates::default(),
        }
    }

    /// `createNewBoxing`: the one box the instruction at `at` makes.
    fn new_boxing(
        &mut self,
        at: &At,
        boxed_type: &str,
        iterator: Option<ProgressionIterator>,
    ) -> Result<BoxingValue, AnalyzerError> {
        let id = match self.boxing_places.get(&at.index) {
            Some(id) => *id,
            None => {
                let unboxed = recognizers::unboxed_type(boxed_type, self.value_classes)
                    .ok_or_else(|| error(at, format!("{boxed_type} does not box a value")))?;
                let id = self.candidates.add(BoxedValue::new(
                    boxed_type.to_string(),
                    unboxed,
                    at.index,
                    iterator,
                ));
                self.boxing_places.insert(at.index, id);
                id
            }
        };
        Ok(BoxingValue::Boxed {
            id,
            tainted: false,
            ty: boxed_type.to_string(),
        })
    }

    /// `checkUsedValue`: a box whose slot also held something else cannot be removed once used.
    fn check_used(&mut self, value: &BoxingValue) {
        if let BoxingValue::Boxed {
            id, tainted: true, ..
        } = value
        {
            self.candidates.remove(*id);
        }
    }

    /// `processOperationWithBoxedValue`.
    fn process_operation(&mut self, value: &BoxingValue, at: usize, op: u8) {
        let Some(id) = value.boxed() else {
            return;
        };
        self.check_used(value);
        if PERMITTED.contains(&op) {
            self.add_associated(id, at);
        } else {
            self.candidates.remove(id);
        }
    }

    /// kotlinc's `processPopInstruction`, which it applies after the analysis: `pop` is not an
    /// interpreter operation.
    pub(super) fn process_pop(&mut self, value: &BoxingValue, at: usize, op: u8) {
        self.process_operation(value, at, op);
    }

    /// `addAssociatedInsn`: only a box still removable collects instructions.
    fn add_associated(&mut self, id: BoxId, at: usize) {
        let value = &mut self.candidates.values[id];
        if value.safe_to_remove {
            value.add_insn(at);
        }
    }

    fn mark_boxed_arguments(&mut self, values: &[BoxingValue]) {
        for value in values {
            if let Some(id) = value.boxed() {
                self.candidates.remove(id);
            }
        }
    }

    /// `isSafeCast` (with `AvoidWrongOptimizationOfTypeOperatorsOnValueClasses`, on by default).
    fn is_safe_cast(&self, id: BoxId, target: &str) -> bool {
        let value = &self.candidates.values[id];
        match target {
            "java/lang/Object" => true,
            "java/lang/Number" => {
                !value.is_value_class_value
                    && matches!(
                        value.unboxed_type.as_str(),
                        "B" | "S" | "I" | "F" | "J" | "D"
                    )
            }
            "java/lang/Comparable" => !value.is_value_class_value,
            _ => descriptors::internal_name(&value.boxed_type) == target,
        }
    }

    /// `isExactValue`: a value whose `checkcast` is known to succeed keeps its identity.
    fn is_exact(value: &BoxingValue) -> bool {
        match value {
            BoxingValue::Iterator(_) => true,
            BoxingValue::Boxed { tainted, .. } => !tainted,
            BoxingValue::Basic(basic) => basic
                .descriptor()
                .is_some_and(recognizers::is_progression_class),
        }
    }

    /// Two boxes of the same primitive, neither a value class's (`areSameTypedPrimitiveBoxedValues`).
    fn same_typed_primitive_boxes(&self, values: &[BoxingValue]) -> Option<(BoxId, BoxId)> {
        let [first, second] = values else {
            return None;
        };
        let (first, second) = (first.boxed()?, second.boxed()?);
        let (a, b) = (
            &self.candidates.values[first],
            &self.candidates.values[second],
        );
        (!a.is_value_class_value && !b.is_value_class_value && a.unboxed_type == b.unboxed_type)
            .then_some((first, second))
    }

    /// `onAreEqual` and `onCompareTo`: the two boxes go or stay together, and the comparison is
    /// rewritten once, with the first.
    fn on_comparison(&mut self, at: usize, first: BoxId, second: BoxId) {
        self.candidates.merge(first, second);
        self.candidates.values[first].add_insn(at);
    }

    /// `mergeBoxedHazardous`: a box meeting a different value. In a local it is only tainted; on
    /// the stack neither side can be removed.
    fn merge_hazardous(
        &mut self,
        boxed: &BoxingValue,
        other: &BoxingValue,
        is_local: bool,
    ) -> BoxingValue {
        let BoxingValue::Boxed { id, ty, .. } = boxed else {
            unreachable!("only a box merges hazardously");
        };
        if is_local {
            return BoxingValue::Boxed {
                id: *id,
                tainted: true,
                ty: ty.clone(),
            };
        }
        self.candidates.remove(*id);
        if let Some(other) = other.boxed() {
            self.candidates.remove(other);
        }
        boxed.clone()
    }

    fn merge_values(&mut self, v: &BoxingValue, w: &BoxingValue, is_local: bool) -> BoxingValue {
        if v.is_uninitialized() || w.is_uninitialized() {
            return BoxingValue::Basic(BasicValue::Uninitialized);
        }
        match (v, w) {
            (
                BoxingValue::Boxed {
                    id: v_id,
                    tainted: v_tainted,
                    ty: v_ty,
                },
                BoxingValue::Boxed {
                    id: w_id,
                    tainted: w_tainted,
                    ty: w_ty,
                },
            ) => {
                self.candidates.merge(*v_id, *w_id);
                if *v_tainted {
                    v.clone()
                } else if *w_tainted {
                    w.clone()
                } else if v_ty != w_ty {
                    self.merge_hazardous(v, w, is_local)
                } else {
                    v.clone()
                }
            }
            (BoxingValue::Boxed { .. }, _) => self.merge_hazardous(v, w, is_local),
            (_, BoxingValue::Boxed { .. }) => self.merge_hazardous(w, v, is_local),
            _ if v == w => v.clone(),
            // `OptimizationBasicInterpreter.merge` of an iterator and a plain reference: unequal
            // values, so their types merge to `Object` even where they are the same.
            (BoxingValue::Iterator(_), BoxingValue::Basic(other))
            | (BoxingValue::Basic(other), BoxingValue::Iterator(_))
                if *other != BasicValue::Null =>
            {
                BoxingValue::Basic(BasicValue::Reference("Ljava/lang/Object;".to_string()))
            }
            _ => BoxingValue::Basic(self.basic.merge(&v.basic(), &w.basic())),
        }
    }
}

fn basic_result(value: Option<BasicValue>) -> Option<BoxingValue> {
    value.map(BoxingValue::Basic)
}

impl Interpreter for BoxingInterpreter<'_> {
    type V = BoxingValue;

    fn new_value(&mut self, ty: Option<&str>) -> Option<BoxingValue> {
        basic_result(self.basic.new_value(ty))
    }

    fn new_operation(&mut self, at: &At) -> Result<BoxingValue, AnalyzerError> {
        self.basic.new_operation(at).map(BoxingValue::Basic)
    }

    fn copy_operation(
        &mut self,
        at: &At,
        value: &BoxingValue,
    ) -> Result<BoxingValue, AnalyzerError> {
        let op = opcode(at.insn);
        if let (Some(id), Insn::Var { op: ASTORE, slot }) = (value.boxed(), at.insn) {
            self.candidates.values[id].variables.insert(*slot);
        }
        self.process_operation(value, at.index, op);
        Ok(value.clone())
    }

    fn unary_operation(
        &mut self,
        at: &At,
        value: &BoxingValue,
    ) -> Result<Option<BoxingValue>, AnalyzerError> {
        let op = opcode(at.insn);
        if let (
            Some(id),
            Insn::Type {
                op: CHECKCAST | INSTANCEOF,
                class,
            },
        ) = (value.boxed(), at.insn)
        {
            if !self.is_safe_cast(id, class) {
                self.candidates.remove(id);
            }
        }
        self.process_operation(value, at.index, op);

        self.check_used(value);
        let cast_to_progression = matches!(
            at.insn,
            Insn::Type { op: CHECKCAST, class } if matches!(
                class.as_str(),
                "kotlin/ranges/CharProgression"
                    | "kotlin/ranges/IntProgression"
                    | "kotlin/ranges/LongProgression"
            )
        );
        if op == CHECKCAST && Self::is_exact(value) && !cast_to_progression {
            return Ok(Some(value.clone()));
        }
        Ok(basic_result(
            self.basic.unary_operation(at, &value.basic())?,
        ))
    }

    fn binary_operation(
        &mut self,
        at: &At,
        first: &BoxingValue,
        second: &BoxingValue,
    ) -> Result<Option<BoxingValue>, AnalyzerError> {
        let op = opcode(at.insn);
        self.process_operation(first, at.index, op);
        self.process_operation(second, at.index, op);
        Ok(basic_result(self.basic.binary_operation(
            at,
            &first.basic(),
            &second.basic(),
        )?))
    }

    fn ternary_operation(
        &mut self,
        at: &At,
        first: &BoxingValue,
        second: &BoxingValue,
        third: &BoxingValue,
    ) -> Result<Option<BoxingValue>, AnalyzerError> {
        // In valid code only an `aastore` takes a box, as the value it stores.
        self.process_operation(third, at.index, opcode(at.insn));
        Ok(basic_result(self.basic.ternary_operation(
            at,
            &first.basic(),
            &second.basic(),
            &third.basic(),
        )?))
    }

    fn nary_operation(
        &mut self,
        at: &At,
        values: &[BoxingValue],
    ) -> Result<Option<BoxingValue>, AnalyzerError> {
        for value in values {
            self.check_used(value);
        }
        let basics: Vec<BasicValue> = values.iter().map(BoxingValue::basic).collect();
        let value = self.basic.nary_operation(at, &basics)?;
        let Some(first) = values.first() else {
            return Ok(basic_result(value));
        };
        let result_type = value
            .as_ref()
            .and_then(BasicValue::descriptor)
            .map(str::to_string);

        if recognizers::is_boxing(at.insn, self.value_classes) {
            // A chain of boxings (`Integer.valueOf` then a value class's `box-impl`) keeps the
            // inner one.
            self.mark_boxed_arguments(values);
            let boxed_type =
                result_type.ok_or_else(|| error(at, "a boxing returns nothing".to_string()))?;
            return self.new_boxing(at, &boxed_type, None).map(Some);
        }
        if let (true, Some(id)) = (
            recognizers::is_unboxing(at.insn, self.value_classes),
            first.boxed(),
        ) {
            let result_type =
                result_type.ok_or_else(|| error(at, "an unboxing returns nothing".to_string()))?;
            if self.candidates.values[id].unboxed_type == result_type {
                self.add_associated(id, at.index);
            } else {
                self.candidates.values[id].add_unboxing_with_cast(at.index, result_type);
            }
            return Ok(basic_result(value));
        }
        if recognizers::is_iterator_call(at.insn) {
            self.mark_boxed_arguments(values);
            let iterator = first
                .descriptor()
                .filter(|descriptor| recognizers::is_progression_class(descriptor))
                .and_then(|descriptor| ProgressionIterator::of_progression(at.index, descriptor));
            return Ok(Some(match iterator {
                Some(iterator) => BoxingValue::Iterator(
                    self.progression_iterators
                        .entry(at.index)
                        .or_insert(iterator)
                        .clone(),
                ),
                None => {
                    if self.progression_iterators.contains_key(&at.index) {
                        self.tainted_iterators.insert(at.index);
                    }
                    return Ok(basic_result(value));
                }
            }));
        }
        if let (BoxingValue::Iterator(iterator), true) =
            (first, recognizers::is_interface_next(at.insn))
        {
            let iterator = iterator.clone();
            return self
                .new_boxing(at, iterator.boxed_element(), Some(iterator))
                .map(Some);
        }
        if recognizers::is_are_equal(at.insn) {
            if let Some((a, b)) = self.same_typed_primitive_boxes(values) {
                let unboxed = &self.candidates.values[a].unboxed_type;
                // `canValuesBeUnboxedForAreEqual`: floating-point and `Class` values compare with
                // `equals`.
                if !matches!(unboxed.as_str(), "D" | "F" | "Ljava/lang/Class;") {
                    self.on_comparison(at.index, a, b);
                    return Ok(basic_result(value));
                }
            }
        }
        if recognizers::is_comparable_compare_to(at.insn) {
            if let Some((a, b)) = self.same_typed_primitive_boxes(values) {
                self.on_comparison(at.index, a, b);
                return Ok(basic_result(value));
            }
        }
        self.mark_boxed_arguments(values);
        Ok(basic_result(value))
    }

    fn return_operation(
        &mut self,
        _at: &At,
        _value: &BoxingValue,
        _expected: &BoxingValue,
    ) -> Result<(), AnalyzerError> {
        Ok(())
    }

    fn merge(&mut self, first: &BoxingValue, second: &BoxingValue) -> BoxingValue {
        self.merge_values(first, second, false)
    }

    fn merge_local(&mut self, first: &BoxingValue, second: &BoxingValue) -> BoxingValue {
        self.merge_values(first, second, true)
    }
}
