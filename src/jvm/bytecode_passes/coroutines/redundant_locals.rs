//! kotlinc's `RedundantLocalsEliminationMethodTransformer`: before a state machine is built, drop
//! the code no path reaches, the local-variable entries left with nothing to describe, and each
//! `GETSTATIC kotlin/Unit.INSTANCE` whose only uses are a `pop` or a store into a slot no local
//! variable names — so that such a `Unit` is never spilled.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::super::analysis::{
    analyze, is_meaningful, node_opcode, AnalyzerError, At, BasicInterpreter, BasicValue,
    Interpreter, Value,
};
use super::super::insn_list::EditableMethod;
use super::super::opcodes::*;
use super::suspension_points::{in_any, SuspensionPoint};
use super::CoroutineError;
use crate::jvm::method_node::{Insn, Node};

/// ASM's plain `BasicValue` kinds, or a `Unit` instance with the nodes that may have produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum UnitSourceValue {
    Uninitialized,
    Int,
    Float,
    Long,
    Double,
    Reference,
    Unit(BTreeSet<usize>),
}

impl Value for UnitSourceValue {
    fn size(&self) -> usize {
        match self {
            UnitSourceValue::Long | UnitSourceValue::Double => 2,
            _ => 1,
        }
    }
}

impl UnitSourceValue {
    fn of(value: &BasicValue) -> UnitSourceValue {
        match value {
            BasicValue::Uninitialized => UnitSourceValue::Uninitialized,
            BasicValue::Int
            | BasicValue::Boolean
            | BasicValue::Char
            | BasicValue::Byte
            | BasicValue::Short => UnitSourceValue::Int,
            BasicValue::Long => UnitSourceValue::Long,
            BasicValue::Float => UnitSourceValue::Float,
            BasicValue::Double => UnitSourceValue::Double,
            BasicValue::Reference(_) | BasicValue::Null => UnitSourceValue::Reference,
        }
    }

    fn representative(&self) -> BasicValue {
        match self {
            UnitSourceValue::Uninitialized => BasicValue::Uninitialized,
            UnitSourceValue::Int => BasicValue::Int,
            UnitSourceValue::Float => BasicValue::Float,
            UnitSourceValue::Long => BasicValue::Long,
            UnitSourceValue::Double => BasicValue::Double,
            UnitSourceValue::Reference | UnitSourceValue::Unit(_) => {
                BasicValue::Reference("Ljava/lang/Object;".to_string())
            }
        }
    }
}

fn kind(value: Option<BasicValue>) -> Option<UnitSourceValue> {
    value.as_ref().map(UnitSourceValue::of)
}

/// kotlinc's `UnitSourceInterpreter`.
struct UnitSourceInterpreter<'a> {
    /// The slots some local variable names.
    named_slots: HashSet<u16>,
    nodes: &'a [Node],
    /// `Unit` instances some use other than `pop` or an unnamed store may see.
    unspillable: BTreeSet<usize>,
    /// Each `Unit` instance's `pop` and unnamed-store uses.
    usages: BTreeMap<usize, BTreeSet<usize>>,
}

impl UnitSourceInterpreter<'_> {
    fn mark_unspillable(&mut self, value: &UnitSourceValue) {
        if let UnitSourceValue::Unit(sources) = value {
            self.unspillable.extend(sources.iter().copied());
        }
    }

    fn collect_usage(&mut self, usage: usize, sources: &BTreeSet<usize>) {
        for source in sources {
            if !self.unspillable.contains(source) {
                self.usages.entry(*source).or_default().insert(usage);
            }
        }
    }
}

fn is_unit_instance(node: &Node) -> bool {
    matches!(node, Node::Insn(Insn::Field { op: GETSTATIC, owner, name, .. })
        if owner == "kotlin/Unit" && name == "INSTANCE")
}

impl Interpreter for UnitSourceInterpreter<'_> {
    type V = UnitSourceValue;

    fn new_value(&mut self, ty: Option<&str>) -> Option<UnitSourceValue> {
        kind(BasicInterpreter.new_value(ty))
    }

    fn new_operation(&mut self, at: &At) -> Result<UnitSourceValue, AnalyzerError> {
        if is_unit_instance(&self.nodes[at.index]) {
            return Ok(UnitSourceValue::Unit(BTreeSet::from([at.index])));
        }
        Ok(UnitSourceValue::of(&BasicInterpreter.new_operation(at)?))
    }

    fn copy_operation(
        &mut self,
        at: &At,
        value: &UnitSourceValue,
    ) -> Result<UnitSourceValue, AnalyzerError> {
        if let UnitSourceValue::Unit(sources) = value {
            if let Insn::Var { op: ASTORE, slot } = at.insn {
                if !self.named_slots.contains(slot) {
                    self.collect_usage(at.index, sources);
                    return Ok(value.clone());
                }
            }
            self.unspillable.extend(sources.iter().copied());
        }
        Ok(value.clone())
    }

    fn unary_operation(
        &mut self,
        at: &At,
        value: &UnitSourceValue,
    ) -> Result<Option<UnitSourceValue>, AnalyzerError> {
        self.mark_unspillable(value);
        Ok(kind(
            BasicInterpreter.unary_operation(at, &value.representative())?,
        ))
    }

    fn binary_operation(
        &mut self,
        at: &At,
        first: &UnitSourceValue,
        second: &UnitSourceValue,
    ) -> Result<Option<UnitSourceValue>, AnalyzerError> {
        self.mark_unspillable(first);
        self.mark_unspillable(second);
        Ok(kind(BasicInterpreter.binary_operation(
            at,
            &first.representative(),
            &second.representative(),
        )?))
    }

    fn ternary_operation(
        &mut self,
        _at: &At,
        first: &UnitSourceValue,
        second: &UnitSourceValue,
        third: &UnitSourceValue,
    ) -> Result<Option<UnitSourceValue>, AnalyzerError> {
        self.mark_unspillable(first);
        self.mark_unspillable(second);
        self.mark_unspillable(third);
        Ok(None)
    }

    fn nary_operation(
        &mut self,
        at: &At,
        values: &[UnitSourceValue],
    ) -> Result<Option<UnitSourceValue>, AnalyzerError> {
        for value in values {
            self.mark_unspillable(value);
        }
        let values: Vec<BasicValue> = values.iter().map(UnitSourceValue::representative).collect();
        Ok(kind(BasicInterpreter.nary_operation(at, &values)?))
    }

    fn return_operation(
        &mut self,
        _at: &At,
        _value: &UnitSourceValue,
        _expected: &UnitSourceValue,
    ) -> Result<(), AnalyzerError> {
        Ok(())
    }

    fn merge(&mut self, first: &UnitSourceValue, second: &UnitSourceValue) -> UnitSourceValue {
        if let (UnitSourceValue::Unit(a), UnitSourceValue::Unit(b)) = (first, second) {
            let merged = UnitSourceValue::Unit(a.union(b).copied().collect());
            if let UnitSourceValue::Unit(sources) = &merged {
                if sources
                    .iter()
                    .any(|source| self.unspillable.contains(source))
                {
                    self.mark_unspillable(&merged);
                }
            }
            return merged;
        }
        // Merging with anything else may make the unit visible: be conservative.
        self.mark_unspillable(first);
        self.mark_unspillable(second);
        if first == second {
            first.clone()
        } else {
            UnitSourceValue::Uninitialized
        }
    }
}

/// `RedundantLocalsEliminationMethodTransformer.transform`.
pub(crate) fn eliminate_redundant_locals(
    method: &mut EditableMethod,
    owner: &str,
    points: &[SuspensionPoint],
) -> Result<(), CoroutineError> {
    let snapshot = method.snapshot();
    let mut interpreter = UnitSourceInterpreter {
        named_slots: method
            .method
            .local_variables
            .iter()
            .map(|local| local.slot)
            .collect(),
        nodes: &snapshot.nodes,
        unspillable: BTreeSet::new(),
        usages: BTreeMap::new(),
    };
    let frames = analyze(&snapshot, owner, &mut interpreter).map_err(CoroutineError::Analysis)?;
    // The analyzer does not see `pop`s; count them as uses here.
    for (index, frame) in frames.iter().enumerate() {
        if let (Some(frame), Some(POP)) = (frame, node_opcode(&snapshot.nodes[index])) {
            if let Some(UnitSourceValue::Unit(sources)) = frame.stack.last() {
                let sources = sources.clone();
                interpreter.collect_usage(index, &sources);
            }
        }
    }
    let ids = method.insns.ids();
    let unreachable: HashSet<usize> = (0..ids.len())
        .filter(|&index| frames[index].is_none())
        .collect();

    // A local variable whose whole range is unreachable or has no instruction goes.
    let label_index = |label| method.label_index(label);
    let redundant: Vec<bool> = method
        .method
        .local_variables
        .iter()
        .map(|local| {
            (label_index(local.start)..label_index(local.end))
                .all(|index| !is_meaningful(&snapshot.nodes[index]) || unreachable.contains(&index))
        })
        .collect();
    let mut keep = redundant.iter().map(|redundant| !redundant);
    method
        .method
        .local_variables
        .retain(|_| keep.next().unwrap_or(true));

    let mut remove: BTreeSet<usize> = unreachable
        .iter()
        .copied()
        .filter(|&index| !matches!(snapshot.nodes[index], Node::Label(_)))
        .collect();
    for (unit, uses) in &interpreter.usages {
        if !interpreter.unspillable.contains(unit) && !in_any(points, &method.insns, ids[*unit]) {
            remove.insert(*unit);
            remove.extend(uses.iter().copied());
        }
    }
    for index in remove {
        method.insns.remove(ids[index]);
    }
    Ok(())
}
