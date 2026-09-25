//! The values kotlinc's boxing analysis tracks (`codegen/optimization/boxing/BoxedBasicValue.kt`,
//! `ProgressionIteratorBasicValue.kt`, `RedundantBoxedValuesCollection.java`): a box made at one
//! boxing instruction, the iterator of a primitive progression, and the set of boxes still
//! removable.

use std::collections::BTreeSet;

use super::super::analysis::{BasicValue, Value};
use super::super::descriptors;

/// A box's identity: the index of its [`BoxedValue`] in the analysis's list.
pub(super) type BoxId = usize;

/// kotlinc's value for boxing analysis: a plain `StrictBasicValue`, a box, or a progression's
/// iterator.
#[derive(Clone, Debug)]
pub(super) enum BoxingValue {
    Basic(BasicValue),
    /// `CleanBoxedValue`, or its `TaintedBoxedValue` (a box a local slot also held something else
    /// in). kotlinc compares boxes by identity; there is one clean and one tainted value per box.
    Boxed {
        id: BoxId,
        tainted: bool,
        ty: String,
    },
    /// `ProgressionIteratorBasicValue`, made by the `iterator()` call at `call`.
    Iterator(ProgressionIterator),
}

impl BoxingValue {
    /// The value's JVM type as a descriptor (ASM's `BasicValue.getType`), `None` when uninitialized.
    pub(super) fn descriptor(&self) -> Option<&str> {
        match self {
            BoxingValue::Basic(value) => value.descriptor(),
            BoxingValue::Boxed { ty, .. } => Some(ty),
            BoxingValue::Iterator(iterator) => Some(iterator.descriptor()),
        }
    }

    /// The value as the plain interpreter sees it.
    pub(super) fn basic(&self) -> BasicValue {
        match self {
            BoxingValue::Basic(value) => value.clone(),
            BoxingValue::Boxed { ty, .. } => BasicValue::Reference(ty.clone()),
            BoxingValue::Iterator(iterator) => {
                BasicValue::Reference(iterator.descriptor().to_string())
            }
        }
    }

    pub(super) fn boxed(&self) -> Option<BoxId> {
        match self {
            BoxingValue::Boxed { id, .. } => Some(*id),
            _ => None,
        }
    }

    pub(super) fn is_uninitialized(&self) -> bool {
        matches!(self, BoxingValue::Basic(BasicValue::Uninitialized))
    }
}

impl PartialEq for BoxingValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (BoxingValue::Basic(a), BoxingValue::Basic(b)) => a == b,
            (
                BoxingValue::Boxed { id, tainted, .. },
                BoxingValue::Boxed {
                    id: other_id,
                    tainted: other_tainted,
                    ..
                },
            ) => id == other_id && tainted == other_tainted,
            // kotlinc compares iterators by type, not by the call that made them.
            (BoxingValue::Iterator(a), BoxingValue::Iterator(b)) => a.element == b.element,
            _ => false,
        }
    }
}

impl Value for BoxingValue {
    fn size(&self) -> usize {
        match self {
            BoxingValue::Basic(value) => value.size(),
            _ => 1,
        }
    }
}

/// The iterator of a `CharRange`/`IntRange`/`LongRange` or its progression, whose `next()` boxes
/// an element its own `nextInt()` (or `nextChar()`, `nextLong()`) returns unboxed.
#[derive(Clone, Debug)]
pub(super) struct ProgressionIterator {
    /// The `iterator()` call's node index.
    pub(super) call: usize,
    /// `Char`, `Int` or `Long`.
    pub(super) element: &'static str,
}

impl ProgressionIterator {
    /// `byProgressionClassType`: the iterator of a progression class's values, by its descriptor.
    pub(super) fn of_progression(call: usize, descriptor: &str) -> Option<ProgressionIterator> {
        let element = match descriptor {
            "Lkotlin/ranges/CharRange;" | "Lkotlin/ranges/CharProgression;" => "Char",
            "Lkotlin/ranges/IntRange;" | "Lkotlin/ranges/IntProgression;" => "Int",
            "Lkotlin/ranges/LongRange;" | "Lkotlin/ranges/LongProgression;" => "Long",
            _ => return None,
        };
        Some(ProgressionIterator { call, element })
    }

    pub(super) fn descriptor(&self) -> &'static str {
        match self.element {
            "Char" => "Lkotlin/collections/CharIterator;",
            "Int" => "Lkotlin/collections/IntIterator;",
            _ => "Lkotlin/collections/LongIterator;",
        }
    }

    pub(super) fn internal_name(&self) -> &'static str {
        descriptors::internal_name(self.descriptor())
    }

    pub(super) fn primitive(&self) -> &'static str {
        match self.element {
            "Char" => "C",
            "Int" => "I",
            _ => "J",
        }
    }

    /// The wrapper `next()` boxes an element to.
    pub(super) fn boxed_element(&self) -> &'static str {
        match self.element {
            "Char" => "Ljava/lang/Character;",
            "Int" => "Ljava/lang/Integer;",
            _ => "Ljava/lang/Long;",
        }
    }

    pub(super) fn next_method_name(&self) -> String {
        format!("next{}", self.element)
    }
}

/// kotlinc's `BoxedValueDescriptor`: one boxing and everything done with the box it makes.
#[derive(Clone, Debug)]
pub(super) struct BoxedValue {
    /// The box's type, a descriptor.
    pub(super) boxed_type: String,
    /// The unboxed type, a descriptor.
    pub(super) unboxed_type: String,
    /// The boxing call's node index.
    pub(super) boxing: usize,
    pub(super) progression_iterator: Option<ProgressionIterator>,
    /// The value is a value class's (or a `KClass`'s), not a primitive's.
    pub(super) is_value_class_value: bool,
    /// Node indices of the instructions that use the box and are rewritten with it, in the order
    /// they were recorded.
    associated: Vec<usize>,
    /// Unboxings to another primitive type: node index and the type they unbox to.
    unboxing_with_cast: Vec<(usize, String)>,
    /// Slots the box is stored to.
    pub(super) variables: BTreeSet<u16>,
    merged_with: BTreeSet<BoxId>,
    pub(super) safe_to_remove: bool,
}

impl BoxedValue {
    pub(super) fn new(
        boxed_type: String,
        unboxed_type: String,
        boxing: usize,
        progression_iterator: Option<ProgressionIterator>,
    ) -> BoxedValue {
        BoxedValue {
            is_value_class_value: !is_primitive_wrapper(&boxed_type),
            boxed_type,
            unboxed_type,
            boxing,
            progression_iterator,
            associated: Vec::new(),
            unboxing_with_cast: Vec::new(),
            variables: BTreeSet::new(),
            merged_with: BTreeSet::new(),
            safe_to_remove: true,
        }
    }

    pub(super) fn add_insn(&mut self, at: usize) {
        if !self.associated.contains(&at) {
            self.associated.push(at);
        }
    }

    pub(super) fn add_unboxing_with_cast(&mut self, at: usize, ty: String) {
        if !self
            .unboxing_with_cast
            .iter()
            .any(|entry| *entry == (at, ty.clone()))
        {
            self.unboxing_with_cast.push((at, ty));
        }
    }

    /// The associated instructions in method order (`sortAssociatedInsns`).
    pub(super) fn associated_insns(&self) -> Vec<usize> {
        let mut sorted = self.associated.clone();
        sorted.sort_unstable();
        sorted
    }

    /// The unboxings with a cast in method order (`sortUnboxingWithCastInsns`).
    pub(super) fn unboxings_with_cast(&self) -> Vec<(usize, String)> {
        let mut sorted = self.unboxing_with_cast.clone();
        sorted.sort_by_key(|(at, _)| *at);
        sorted
    }
}

/// `AsmUtil.isBoxedPrimitiveType`: a primitive's wrapper class.
fn is_primitive_wrapper(descriptor: &str) -> bool {
    unboxed_primitive(descriptor).is_some()
}

/// `AsmUtil.unboxPrimitiveTypeOrNull`: the primitive a wrapper class boxes.
pub(super) fn unboxed_primitive(descriptor: &str) -> Option<&'static str> {
    Some(match descriptor {
        "Ljava/lang/Boolean;" => "Z",
        "Ljava/lang/Character;" => "C",
        "Ljava/lang/Byte;" => "B",
        "Ljava/lang/Short;" => "S",
        "Ljava/lang/Integer;" => "I",
        "Ljava/lang/Float;" => "F",
        "Ljava/lang/Long;" => "J",
        "Ljava/lang/Double;" => "D",
        _ => return None,
    })
}

/// kotlinc's `RedundantBoxedValuesCollection`: the boxes still safe to remove, and the boxes each
/// one met at a merge, which stop being removable together.
#[derive(Debug, Default)]
pub(super) struct Candidates {
    pub(super) values: Vec<BoxedValue>,
    safe: BTreeSet<BoxId>,
}

impl Candidates {
    pub(super) fn add(&mut self, value: BoxedValue) -> BoxId {
        let id = self.values.len();
        self.values.push(value);
        self.safe.insert(id);
        id
    }

    pub(super) fn remove(&mut self, id: BoxId) {
        if !self.safe.remove(&id) {
            return;
        }
        self.values[id].safe_to_remove = false;
        let merged: Vec<BoxId> = self.values[id].merged_with.iter().copied().collect();
        for other in merged {
            self.remove(other);
        }
    }

    pub(super) fn merge(&mut self, v: BoxId, w: BoxId) {
        self.values[v].merged_with.insert(w);
        self.values[w].merged_with.insert(v);
        let (v_safe, w_safe) = (self.values[v].safe_to_remove, self.values[w].safe_to_remove);
        if v_safe && !w_safe {
            self.remove(v);
        }
        if !v_safe && w_safe {
            self.remove(w);
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.safe.is_empty()
    }

    /// The removable boxes, in the order they were made.
    pub(super) fn safe(&self) -> Vec<BoxId> {
        self.safe.iter().copied().collect()
    }
}
