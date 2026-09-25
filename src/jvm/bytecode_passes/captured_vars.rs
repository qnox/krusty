//! kotlinc's `CapturedVarsOptimizationMethodTransformer` (`codegen/optimization/`), the first of
//! its optimizations: a `kotlin.jvm.internal.Ref` a method creates and only ever loads, stores,
//! `dup`s, `pop`s and reads or writes the `element` of becomes a local of the element's type. A
//! `var` captured by a lambda that was inlined is such a `Ref`: once the lambda's body sits in the
//! method, nothing else sees the box.
//!
//! The analysis is kotlinc's `ReferenceTrackingInterpreter`: each `new` of a `Ref` makes a tracked
//! value. Where two edges join, a tracked value meets only itself; meeting anything else taints it,
//! and a tainted value that is then used rules its `Ref`s out.

use std::collections::BTreeSet;

use super::analysis::{
    analyze, AnalyzerError, At, BasicInterpreter, BasicValue, Frame, Interpreter, Value,
};
use super::descriptors;
use super::insn_list::{EditableMethod, NodeId};
use super::opcodes::*;
use crate::jvm::method_node::{Insn, MethodNode, Node};

const ACONST_NULL: u8 = 0x01;
const LCONST_0: u8 = 0x09;
const FCONST_0: u8 = 0x0b;
const DCONST_0: u8 = 0x0e;
const ISTORE: u8 = 0x36;

const REF_ELEMENT_FIELD: &str = "element";
const SHARED_VAR_PREFIX: &str = "kotlin/jvm/internal/Ref$";

/// `REF_TYPE_TO_ELEMENT_TYPE`: the descriptor of the value each `Ref` class holds.
fn element_type(ref_class: &str) -> Option<&'static str> {
    Some(match ref_class.strip_prefix(SHARED_VAR_PREFIX)? {
        "ObjectRef" => "Ljava/lang/Object;",
        "BooleanRef" => "Z",
        "CharRef" => "C",
        "ByteRef" => "B",
        "ShortRef" => "S",
        "IntRef" => "I",
        "FloatRef" => "F",
        "LongRef" => "J",
        "DoubleRef" => "D",
        _ => return None,
    })
}

/// `AsmTypes.isSharedVarType`, for a local variable's descriptor.
fn is_shared_var_descriptor(descriptor: &str) -> bool {
    descriptor
        .strip_prefix('L')
        .is_some_and(|class| class.starts_with(SHARED_VAR_PREFIX))
}

/// `AsmUtil.defaultValueOpcode`.
fn default_value_opcode(descriptor: &str) -> u8 {
    match descriptor.as_bytes()[0] {
        b'J' => LCONST_0,
        b'F' => FCONST_0,
        b'D' => DCONST_0,
        b'L' | b'[' => ACONST_NULL,
        _ => ICONST_0,
    }
}

fn words(descriptor: &str) -> u16 {
    if matches!(descriptor, "J" | "D") {
        2
    } else {
        1
    }
}

/// One `Ref` the method creates (`CapturedVarDescriptor`), by node index where it names nodes.
struct CapturedVar {
    new_insn: usize,
    element: &'static str,
    hazard: bool,
    init_call: Option<usize>,
    /// The local variable entry that holds the `Ref`, by its position in the table.
    local_var: Option<usize>,
    slot: u16,
    /// The loads, stores, `dup`s and `pop`s that only move the `Ref` around, in the order the
    /// analysis met them.
    wrappers: Vec<usize>,
    get_fields: Vec<usize>,
    put_fields: Vec<usize>,
}

impl CapturedVar {
    fn can_rewrite(&self) -> bool {
        !self.hazard && self.init_call.is_some()
    }
}

fn add_once(list: &mut Vec<usize>, index: usize) {
    if !list.contains(&index) {
        list.push(index);
    }
}

/// A value of the analysis: a plain value, or one that stands for one or more `Ref`s.
#[derive(Clone, Debug, PartialEq)]
enum TrackedValue {
    Basic(BasicValue),
    /// `ProperTrackedReferenceValue`: exactly the `Ref` made by one `new`.
    Proper(usize, BasicValue),
    /// `TaintedTrackedReferenceValue`: a `Ref` met by another value where edges join.
    Tainted(BTreeSet<usize>, BasicValue),
}

impl TrackedValue {
    fn basic(&self) -> &BasicValue {
        match self {
            TrackedValue::Basic(value)
            | TrackedValue::Proper(_, value)
            | TrackedValue::Tainted(_, value) => value,
        }
    }

    fn descriptors(&self) -> BTreeSet<usize> {
        match self {
            TrackedValue::Basic(_) => BTreeSet::new(),
            TrackedValue::Proper(var, _) => BTreeSet::from([*var]),
            TrackedValue::Tainted(vars, _) => vars.clone(),
        }
    }

    fn proper(&self) -> Option<usize> {
        match self {
            TrackedValue::Proper(var, _) => Some(*var),
            _ => None,
        }
    }
}

impl Value for TrackedValue {
    fn size(&self) -> usize {
        self.basic().size()
    }
}

struct CapturedVarsInterpreter<'a> {
    basic: BasicInterpreter,
    vars: &'a mut [CapturedVar],
    var_of_new: &'a dyn Fn(usize) -> Option<usize>,
}

fn plain(value: Option<BasicValue>) -> Option<TrackedValue> {
    value.map(TrackedValue::Basic)
}

impl CapturedVarsInterpreter<'_> {
    /// `checkRefValuesUsages`: a tainted value rules its `Ref`s out, and each tracked operand's use
    /// is recorded (`processRefValueUsage`).
    fn check_usages(&mut self, at: &At, values: &[&TrackedValue]) {
        for value in values {
            if let TrackedValue::Tainted(vars, _) = value {
                for &var in vars {
                    self.vars[var].hazard = true;
                }
            }
        }
        for (position, value) in values.iter().enumerate() {
            for var in value.descriptors() {
                self.record_usage(at, var, position);
            }
        }
    }

    fn record_usage(&mut self, at: &At, var: usize, position: usize) {
        let index = at.index;
        let var = &mut self.vars[var];
        match at.insn {
            Insn::Op(DUP)
            | Insn::Var {
                op: ALOAD | ASTORE, ..
            } => add_once(&mut var.wrappers, index),
            Insn::Field {
                op: GETFIELD, name, ..
            } if name == REF_ELEMENT_FIELD && position == 0 => add_once(&mut var.get_fields, index),
            Insn::Field {
                op: PUTFIELD, name, ..
            } if name == REF_ELEMENT_FIELD && position == 0 => add_once(&mut var.put_fields, index),
            Insn::Method {
                op: INVOKESPECIAL,
                name,
                ..
            } if name == "<init>" && position == 0 => match var.init_call {
                Some(call) if call != index => var.hazard = true,
                _ => var.init_call = Some(index),
            },
            _ => var.hazard = true,
        }
    }
}

/// `getMergedValueType`.
fn merged_type(first: &BasicValue, second: &BasicValue) -> BasicValue {
    if first == second && *first != BasicValue::Uninitialized {
        first.clone()
    } else {
        BasicValue::Reference("Ljava/lang/Object;".to_string())
    }
}

impl Interpreter for CapturedVarsInterpreter<'_> {
    type V = TrackedValue;

    fn new_value(&mut self, ty: Option<&str>) -> Option<TrackedValue> {
        plain(self.basic.new_value(ty))
    }

    fn new_parameter_value(&mut self, slot: usize, ty: &str) -> TrackedValue {
        TrackedValue::Basic(self.basic.new_parameter_value(slot, ty))
    }

    fn new_exception_value(&mut self, catch_internal_name: &str) -> TrackedValue {
        TrackedValue::Basic(self.basic.new_exception_value(catch_internal_name))
    }

    fn new_operation(&mut self, at: &At) -> Result<TrackedValue, AnalyzerError> {
        let value = self.basic.new_operation(at)?;
        Ok(match (self.var_of_new)(at.index) {
            Some(var) => TrackedValue::Proper(var, value),
            None => TrackedValue::Basic(value),
        })
    }

    fn copy_operation(
        &mut self,
        at: &At,
        value: &TrackedValue,
    ) -> Result<TrackedValue, AnalyzerError> {
        if matches!(value, TrackedValue::Basic(_)) {
            return Ok(TrackedValue::Basic(
                self.basic.copy_operation(at, value.basic())?,
            ));
        }
        self.check_usages(at, &[value]);
        Ok(value.clone())
    }

    fn unary_operation(
        &mut self,
        at: &At,
        value: &TrackedValue,
    ) -> Result<Option<TrackedValue>, AnalyzerError> {
        self.check_usages(at, &[value]);
        Ok(plain(self.basic.unary_operation(at, value.basic())?))
    }

    fn binary_operation(
        &mut self,
        at: &At,
        first: &TrackedValue,
        second: &TrackedValue,
    ) -> Result<Option<TrackedValue>, AnalyzerError> {
        self.check_usages(at, &[first, second]);
        Ok(plain(self.basic.binary_operation(
            at,
            first.basic(),
            second.basic(),
        )?))
    }

    fn ternary_operation(
        &mut self,
        at: &At,
        first: &TrackedValue,
        second: &TrackedValue,
        third: &TrackedValue,
    ) -> Result<Option<TrackedValue>, AnalyzerError> {
        self.check_usages(at, &[first, second, third]);
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
        values: &[TrackedValue],
    ) -> Result<Option<TrackedValue>, AnalyzerError> {
        self.check_usages(at, &values.iter().collect::<Vec<_>>());
        let basics: Vec<BasicValue> = values.iter().map(|value| value.basic().clone()).collect();
        Ok(plain(self.basic.nary_operation(at, &basics)?))
    }

    fn return_operation(
        &mut self,
        at: &At,
        value: &TrackedValue,
        expected: &TrackedValue,
    ) -> Result<(), AnalyzerError> {
        self.basic
            .return_operation(at, value.basic(), expected.basic())
    }

    /// `ReferenceTrackingInterpreter.merge`.
    fn merge(&mut self, first: &TrackedValue, second: &TrackedValue) -> TrackedValue {
        match (first, second) {
            (TrackedValue::Proper(a, _), TrackedValue::Proper(b, _)) if a == b => first.clone(),
            (TrackedValue::Basic(a), TrackedValue::Basic(b)) => {
                TrackedValue::Basic(self.basic.merge(a, b))
            }
            _ => {
                let mut vars = first.descriptors();
                vars.extend(second.descriptors());
                TrackedValue::Tainted(vars, merged_type(first.basic(), second.basic()))
            }
        }
    }
}

/// Replace every `Ref` of `method`, a member of `owner`, that never escapes by a local of its
/// element's type; `true` when any was.
pub(crate) fn eliminate(method: &mut MethodNode, owner: &str) -> Result<bool, AnalyzerError> {
    // `createRefValues`.
    let mut vars: Vec<CapturedVar> = method
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(index, node)| match node {
            Node::Insn(Insn::Type { op: NEW, class }) => Some(CapturedVar {
                new_insn: index,
                element: element_type(class)?,
                hazard: false,
                init_call: None,
                local_var: None,
                slot: 0,
                wrappers: Vec::new(),
                get_fields: Vec::new(),
                put_fields: Vec::new(),
            }),
            _ => None,
        })
        .collect();
    if vars.is_empty() {
        return Ok(false);
    }
    let news: Vec<usize> = vars.iter().map(|var| var.new_insn).collect();
    let var_of_new = |index: usize| news.iter().position(|&at| at == index);
    let frames = {
        let mut interpreter = CapturedVarsInterpreter {
            basic: BasicInterpreter,
            vars: &mut vars,
            var_of_new: &var_of_new,
        };
        analyze(method, owner, &mut interpreter)?
    };
    track_pops(method, &frames, &mut vars);
    assign_local_vars(method, &frames, &mut vars);
    if !vars.iter().any(CapturedVar::can_rewrite) {
        return Ok(false);
    }

    let mut editable = EditableMethod::new(std::mem::replace(method, MethodNode::new(0, "", "")));
    let ids = editable.insns.ids();
    for var in vars.iter().filter(|var| var.can_rewrite()) {
        rewrite(&mut editable, &ids, var);
    }
    *method = editable.finish();
    Ok(true)
}

/// `trackPops`: a `pop` of a `Ref` moves it; a `pop2` of two words that are `Ref`s rules them out.
fn track_pops(
    method: &MethodNode,
    frames: &[Option<Frame<TrackedValue>>],
    vars: &mut [CapturedVar],
) {
    for (index, node) in method.nodes.iter().enumerate() {
        let Some(frame) = &frames[index] else {
            continue;
        };
        let top = |depth: usize| {
            frame
                .stack
                .len()
                .checked_sub(depth + 1)
                .map(|at| &frame.stack[at])
        };
        match node {
            Node::Insn(Insn::Op(POP)) => {
                if let Some(var) = top(0).and_then(TrackedValue::proper) {
                    add_once(&mut vars[var].wrappers, index);
                }
            }
            Node::Insn(Insn::Op(POP2)) => {
                if top(0).is_some_and(|value| value.size() == 1) {
                    for depth in [0, 1] {
                        if let Some(var) = top(depth).and_then(TrackedValue::proper) {
                            vars[var].hazard = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// `assignLocalVars`: the local variable entry each `Ref` lives in, whose slot its element takes
/// over; a `Ref` without one, or whose element needs two words, gets a new slot.
fn assign_local_vars(
    method: &mut MethodNode,
    frames: &[Option<Frame<TrackedValue>>],
    vars: &mut [CapturedVar],
) {
    let positions = crate::jvm::method_node::LabelPositions::of(method);
    for (entry, local) in method.local_variables.iter().enumerate() {
        if !is_shared_var_descriptor(&local.desc) {
            continue;
        }
        let Some(frame) = &frames[positions.at(local.start)] else {
            continue;
        };
        let Some(var) = frame
            .locals
            .get(usize::from(local.slot))
            .and_then(TrackedValue::proper)
        else {
            continue;
        };
        let var = &mut vars[var];
        if var.hazard {
            continue;
        }
        if var.local_var.is_none() {
            var.local_var = Some(entry);
        } else {
            var.hazard = true;
        }
    }
    for var in vars.iter_mut().filter(|var| !var.hazard) {
        match var.local_var {
            Some(entry) if words(var.element) == 1 => var.slot = method.local_variables[entry].slot,
            _ => {
                var.slot = method.max_locals;
                method.max_locals += words(var.element);
            }
        }
    }
}

/// kotlinc's `removeOrReplaceByNop`: an instruction alone on its line becomes a `nop`, so the line
/// keeps an instruction to stop at.
fn remove_or_replace_by_nop(editable: &mut EditableMethod, id: NodeId) {
    let insns = &editable.insns;
    let alone = insns
        .prev(id)
        .is_some_and(|prev| matches!(insns.node(prev), Node::Line { .. }))
        && insns.next(id).is_some_and(|next| {
            matches!(insns.node(next), Node::Label(_))
                && insns
                    .next(next)
                    .is_some_and(|after| matches!(insns.node(after), Node::Line { .. }))
        });
    if alone {
        editable.insns.set(id, Node::Insn(Insn::Op(NOP)));
    } else {
        editable.insns.remove(id);
    }
}

/// `rewriteRefValue`.
fn rewrite(editable: &mut EditableMethod, ids: &[NodeId], var: &CapturedVar) {
    let load = descriptors::typed_opcode(var.element, ILOAD);
    let store = descriptors::typed_opcode(var.element, ISTORE);
    let default_value = Node::Insn(Insn::Op(default_value_opcode(var.element)));
    let new_insn = ids[var.new_insn];

    if let Some(entry) = var.local_var {
        let local = editable.method.local_variables[entry].clone();
        let label_id = |label| {
            editable.insns.ids().into_iter().find(
                |&id| matches!(editable.insns.node(id), Node::Label(placed) if *placed == label),
            )
        };
        let (start, end) = (label_id(local.start), label_id(local.end));
        let start_index = start.map(|id| editable.insns.index_of(id));
        if !var
            .put_fields
            .iter()
            .any(|&put| start_index.is_some_and(|start| editable.insns.index_of(ids[put]) < start))
        {
            // The local must hold a value before its range begins.
            editable.insns.insert_before(
                new_insn,
                vec![
                    default_value.clone(),
                    Node::Insn(Insn::Var {
                        op: store,
                        slot: var.slot,
                    }),
                ],
            );
        }
        // `findCleanInstructions`: the `aconst_null; astore` that end the `Ref`'s scope.
        let mut clean = Vec::new();
        let mut cursor = start;
        while let Some(id) = cursor.filter(|&id| Some(id) != end) {
            if matches!(editable.insns.node(id), Node::Insn(Insn::Var { op: ASTORE, slot }) if *slot == local.slot)
                && editable.insns.prev(id).is_some_and(|prev| {
                    matches!(editable.insns.node(prev), Node::Insn(Insn::Op(ACONST_NULL)))
                })
            {
                clean.push(id);
            }
            cursor = editable.insns.next(id);
        }
        for id in clean {
            let prev = editable
                .insns
                .prev(id)
                .expect("a clean store follows its null");
            if store == ASTORE {
                editable.insns.set(prev, default_value.clone());
            } else {
                editable.insns.remove(prev);
                editable.insns.remove(id);
            }
        }
        let local = &mut editable.method.local_variables[entry];
        local.slot = var.slot;
        local.desc = var.element.to_string();
    }

    editable.insns.remove(new_insn);
    editable.insns.remove(
        ids[var
            .init_call
            .expect("a rewritten Ref has its constructor call")],
    );
    for &wrapper in &var.wrappers {
        remove_or_replace_by_nop(editable, ids[wrapper]);
    }
    for &get in &var.get_fields {
        editable.insns.set(
            ids[get],
            Node::Insn(Insn::Var {
                op: load,
                slot: var.slot,
            }),
        );
    }
    for &put in &var.put_fields {
        editable.insns.set(
            ids[put],
            Node::Insn(Insn::Var {
                op: store,
                slot: var.slot,
            }),
        );
    }
}

#[cfg(test)]
mod tests;
