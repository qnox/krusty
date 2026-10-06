//! kotlinc's `FixStackMethodTransformer` (`codegen/optimization/fixStack/`), the part that saves
//! the operand stack around a call bracketed by `InlineMarker.beforeInlineCall`/`afterInlineCall`:
//! whatever is on the stack at the opening marker is stored into fresh locals, and reloaded under
//! the call's result at the closing one. A suspension point needs this, because resuming enters the
//! method with an empty stack.
//!
//! The try/catch part is driven by codegen's save-stack pseudo-instructions around a `try` entered
//! with values on the stack; it is not ported, and neither are the break/continue and fake-`ifeq`
//! pseudo-instructions, because krusty's emitter never produces them. Protected ranges and their
//! handlers otherwise need nothing from FixStack: its analysis follows the exception edges.

use super::analysis::{
    analyze_with, AnalyzerError, AnalyzerOptions, At, BasicInterpreter, BasicValue, Executor,
    Frame, Interpreter, Value,
};
use super::insn_list::{EditableMethod, NodeId};
use super::opcodes::*;
use crate::jvm::bytecode_passes::coroutines::markers::{
    has_reserved_inline_locals, is_after_inline_marker, is_before_inline_marker, is_inline_marker,
};
use crate::jvm::method_node::{Insn, Node};

/// kotlinc's `FixStackValue`: a stack word's kind, as loads, stores and pops need it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FixStackValue {
    Int,
    Long,
    Float,
    Double,
    Object,
    Uninitialized,
}

impl Value for FixStackValue {
    fn size(&self) -> usize {
        match self {
            FixStackValue::Long | FixStackValue::Double => 2,
            _ => 1,
        }
    }
}

impl FixStackValue {
    fn of(value: &BasicValue) -> FixStackValue {
        match value {
            BasicValue::Uninitialized => FixStackValue::Uninitialized,
            BasicValue::Int
            | BasicValue::Boolean
            | BasicValue::Char
            | BasicValue::Byte
            | BasicValue::Short => FixStackValue::Int,
            BasicValue::Long => FixStackValue::Long,
            BasicValue::Float => FixStackValue::Float,
            BasicValue::Double => FixStackValue::Double,
            BasicValue::Reference(_) | BasicValue::Null => FixStackValue::Object,
        }
    }

    /// A basic value of this kind, for the basic interpreter to compute a result from.
    fn representative(self) -> BasicValue {
        match self {
            FixStackValue::Int => BasicValue::Int,
            FixStackValue::Long => BasicValue::Long,
            FixStackValue::Float => BasicValue::Float,
            FixStackValue::Double => BasicValue::Double,
            FixStackValue::Object => BasicValue::reference("Ljava/lang/Object;"),
            FixStackValue::Uninitialized => BasicValue::Uninitialized,
        }
    }

    fn load(self) -> u8 {
        match self {
            FixStackValue::Int => ILOAD,
            FixStackValue::Long => LLOAD,
            FixStackValue::Float => FLOAD,
            FixStackValue::Double => DLOAD,
            FixStackValue::Object | FixStackValue::Uninitialized => ALOAD,
        }
    }

    fn store(self) -> u8 {
        self.load() - ILOAD + ISTORE
    }
}

/// kotlinc's `FixStackInterpreter`: the basic interpreter's result, reduced to its kind.
struct FixStackInterpreter;

fn representatives(values: &[FixStackValue]) -> Vec<BasicValue> {
    values.iter().map(|value| value.representative()).collect()
}

fn kind(result: Option<BasicValue>) -> Option<FixStackValue> {
    result.as_ref().map(FixStackValue::of)
}

impl Interpreter for FixStackInterpreter {
    type V = FixStackValue;

    fn new_value(&mut self, ty: Option<&str>) -> Option<FixStackValue> {
        kind(BasicInterpreter.new_value(ty))
    }

    fn new_operation(&mut self, at: &At) -> Result<FixStackValue, AnalyzerError> {
        Ok(FixStackValue::of(&BasicInterpreter.new_operation(at)?))
    }

    fn copy_operation(
        &mut self,
        at: &At,
        value: &FixStackValue,
    ) -> Result<FixStackValue, AnalyzerError> {
        Ok(match at.insn {
            Insn::Var { op: ILOAD, .. } => FixStackValue::Int,
            Insn::Var { op: LLOAD, .. } => FixStackValue::Long,
            Insn::Var { op: FLOAD, .. } => FixStackValue::Float,
            Insn::Var { op: DLOAD, .. } => FixStackValue::Double,
            Insn::Var { op: ALOAD, .. } => FixStackValue::Object,
            _ => *value,
        })
    }

    fn unary_operation(
        &mut self,
        at: &At,
        value: &FixStackValue,
    ) -> Result<Option<FixStackValue>, AnalyzerError> {
        Ok(kind(
            BasicInterpreter.unary_operation(at, &value.representative())?,
        ))
    }

    fn binary_operation(
        &mut self,
        at: &At,
        first: &FixStackValue,
        second: &FixStackValue,
    ) -> Result<Option<FixStackValue>, AnalyzerError> {
        Ok(kind(BasicInterpreter.binary_operation(
            at,
            &first.representative(),
            &second.representative(),
        )?))
    }

    fn ternary_operation(
        &mut self,
        _at: &At,
        _first: &FixStackValue,
        _second: &FixStackValue,
        _third: &FixStackValue,
    ) -> Result<Option<FixStackValue>, AnalyzerError> {
        Ok(None)
    }

    fn nary_operation(
        &mut self,
        at: &At,
        values: &[FixStackValue],
    ) -> Result<Option<FixStackValue>, AnalyzerError> {
        Ok(kind(
            BasicInterpreter.nary_operation(at, &representatives(values))?,
        ))
    }

    fn return_operation(
        &mut self,
        _at: &At,
        _value: &FixStackValue,
        _expected: &FixStackValue,
    ) -> Result<(), AnalyzerError> {
        Ok(())
    }

    fn merge(&mut self, first: &FixStackValue, _second: &FixStackValue) -> FixStackValue {
        // The fast stack analyzer never merges: a node keeps the first frame that reaches it.
        *first
    }
}

/// `FixStackAnalyzer.FixStackFrame`: an opening marker saves the stack and clears it, the matching
/// closing marker puts the saved values back under the call's result.
struct FixStackFrames<'a> {
    /// The opening marker of each closing one, by node index.
    opening: &'a [Option<usize>],
    /// The stack each opening marker saved, by node index.
    spilled: Vec<Option<Vec<FixStackValue>>>,
}

impl Executor<FixStackInterpreter> for FixStackFrames<'_> {
    fn execute(
        &mut self,
        frame: &mut Frame<FixStackValue>,
        at: &At,
        interpreter: &mut FixStackInterpreter,
    ) -> Result<(), AnalyzerError> {
        let node = Node::Insn(at.insn.clone());
        if is_before_inline_marker(&node) {
            self.spilled[at.index] = Some(std::mem::take(&mut frame.stack));
        } else if is_after_inline_marker(&node) {
            let saved = self.opening[at.index]
                .and_then(|opening| self.spilled[opening].clone())
                .ok_or_else(|| AnalyzerError {
                    index: at.index,
                    message: "a closing inline marker whose opening one saved no stack".to_string(),
                })?;
            let result = frame.stack.pop();
            frame.stack = saved;
            frame.stack.extend(result);
        }
        frame.execute(at, interpreter)
    }
}

/// Why the stack could not be fixed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FixStackError {
    Analysis(AnalyzerError),
    /// A closing marker where more than the call's result is on the stack.
    ResultShape(usize),
    /// An uninitialized value on the stack an opening marker saves.
    UninitializedValue(usize),
    /// A reserved opening marker is not followed by the inline-frame local whose slot bounds the
    /// reservation, or that reservation overlaps another saved stack.
    ReservedLocalLayout(usize),
}

/// A saved stack: its values bottom-first and the first local they are stored from
/// (`SavedStackDescriptor`).
#[derive(Clone, Debug)]
struct SavedStack {
    values: Vec<FixStackValue>,
    first_local: u16,
}

impl SavedStack {
    fn first_unused_local(&self) -> u16 {
        self.first_local
            + self
                .values
                .iter()
                .map(|value| value.size() as u16)
                .sum::<u16>()
    }
}

/// A change to the body, made once the analysis that decided it is no longer read.
type DeferredAction = Box<dyn FnOnce(&mut EditableMethod)>;

/// The first word reserved for a stack spill. A reserved opening marker is emitted immediately
/// before the inline-frame marker's `iconst_0; istore`: the frame map placed that local after one
/// word per word on the operand stack, so subtracting the spill width recovers the reservation.
fn reserved_first_local(method: &EditableMethod, opening: NodeId, words: u16) -> Option<u16> {
    let mut cursor = method.insns.next(opening);
    let mut saw_zero = false;
    while let Some(id) = cursor {
        match method.insns.node(id) {
            Node::Insn(Insn::Op(ICONST_0)) if !saw_zero => saw_zero = true,
            Node::Insn(Insn::Var { op: ISTORE, slot }) if saw_zero => {
                return slot.checked_sub(words);
            }
            Node::Insn(_) => return None,
            Node::Label(_) | Node::Line { .. } => {}
        }
        cursor = method.insns.next(id);
    }
    None
}

/// `FixStackMethodTransformer.transform`, for the inline-call markers.
pub(crate) fn fix_stack(method: &mut EditableMethod, owner: &str) -> Result<(), FixStackError> {
    let ids = method.insns.ids();
    // FixStackContext: pair each closing marker with its opening one.
    let mut open = Vec::new();
    let mut pairs: Vec<(NodeId, NodeId)> = Vec::new();
    for &id in &ids {
        let node = method.insns.node(id);
        if is_before_inline_marker(node) {
            open.push(id);
        } else if is_after_inline_marker(node) {
            let opening = open.pop().ok_or_else(|| {
                FixStackError::Analysis(AnalyzerError {
                    index: method.insns.index_of(id),
                    message: "a closing inline marker without an opening one".to_string(),
                })
            })?;
            pairs.push((opening, id));
        }
    }
    if pairs.is_empty() && open.is_empty() {
        return Ok(());
    }
    if !open.is_empty() {
        // Inconsistent markers are dropped rather than acted on, as kotlinc does.
        for id in ids {
            if is_inline_marker(method.insns.node(id), None) {
                method.insns.remove(id);
            }
        }
        return Ok(());
    }
    let snapshot = method.snapshot();
    let mut opening = vec![None; snapshot.nodes.len()];
    for (before, after) in &pairs {
        opening[method.insns.index_of(*after)] = Some(method.insns.index_of(*before));
    }
    let mut frames_hook = FixStackFrames {
        opening: &opening,
        spilled: vec![None; snapshot.nodes.len()],
    };
    let frames = analyze_with(
        &snapshot,
        owner,
        &mut FixStackInterpreter,
        &mut frames_hook,
        AnalyzerOptions {
            prune_exception_edges: false,
            fast_handlers: true,
            fast_merge: true,
            ..AnalyzerOptions::default()
        },
    )
    .map_err(FixStackError::Analysis)?;

    // LocalVariablesManager: a saved stack takes locals above every stack still saved.
    let initial_max_locals = method.method.max_locals;
    let mut live: Vec<(NodeId, SavedStack)> = Vec::new();
    let live_first_unused = |live: &[(NodeId, SavedStack)]| {
        live.iter()
            // An enclosing bracket whose opening stack was empty owns no local range. In
            // particular, the suspend-call bracket around an inline argument may be live while
            // the argument's reserved bracket uses locals below `initial_max_locals`.
            .filter(|(_, saved)| !saved.values.is_empty())
            .map(|(_, saved)| saved.first_unused_local())
            .max()
            .unwrap_or(0)
    };
    let first_unused =
        |live: &[(NodeId, SavedStack)]| initial_max_locals.max(live_first_unused(live));
    let mut actions: Vec<DeferredAction> = Vec::new();
    for id in ids {
        let node = method.insns.node(id).clone();
        let index = method.insns.index_of(id);
        if is_before_inline_marker(&node) {
            let values = frames_hook.spilled[index].clone().unwrap_or_default();
            let live_marker = frames_hook.spilled[index].is_some();
            if values.contains(&FixStackValue::Uninitialized) {
                return Err(FixStackError::UninitializedValue(index));
            }
            let words = values.iter().map(|value| value.size() as u16).sum::<u16>();
            let first_local = if has_reserved_inline_locals(&node) {
                let Some(reserved) = reserved_first_local(method, id, words) else {
                    return Err(FixStackError::ReservedLocalLayout(index));
                };
                if reserved < live_first_unused(&live)
                    || reserved.checked_add(words).is_none()
                    || reserved + words > initial_max_locals
                {
                    return Err(FixStackError::ReservedLocalLayout(index));
                }
                reserved
            } else {
                first_unused(&live)
            };
            let saved = SavedStack {
                values,
                first_local,
            };
            method.method.max_locals = method.method.max_locals.max(saved.first_unused_local());
            live.push((id, saved.clone()));
            actions.push(Box::new(move |method| {
                if live_marker {
                    let mut local = saved.first_unused_local();
                    let mut stores = Vec::new();
                    for value in saved.values.iter().rev() {
                        local -= value.size() as u16;
                        stores.push(Node::Insn(Insn::Var {
                            op: value.store(),
                            slot: local,
                        }));
                    }
                    method.insns.insert_before(id, stores);
                }
                method.insns.remove(id);
            }));
        } else if is_after_inline_marker(&node) {
            let (before, _) = pairs
                .iter()
                .find(|(_, after)| *after == id)
                .expect("every closing marker was paired");
            let position = live
                .iter()
                .position(|(opening, _)| opening == before)
                .expect("an opening marker's stack is saved until its closing one");
            let saved = live[position].1.clone();
            let stack = frames[index].as_ref().map(|frame| frame.stack.clone());
            match stack {
                Some(stack) if !saved.values.is_empty() => match stack.as_slice() {
                    [result] => {
                        let result = *result;
                        let result_local = first_unused(&live);
                        method.method.max_locals = method
                            .method
                            .max_locals
                            .max(result_local + result.size() as u16);
                        actions.push(Box::new(move |method| {
                            let mut nodes = vec![Node::Insn(Insn::Var {
                                op: result.store(),
                                slot: result_local,
                            })];
                            nodes.extend(loads(&saved));
                            nodes.push(Node::Insn(Insn::Var {
                                op: result.load(),
                                slot: result_local,
                            }));
                            method.insns.insert_before(id, nodes);
                            method.insns.remove(id);
                        }));
                    }
                    [] => actions.push(Box::new(move |method| {
                        method.insns.insert_before(id, loads(&saved));
                        method.insns.remove(id);
                    })),
                    _ => return Err(FixStackError::ResultShape(index)),
                },
                _ => actions.push(Box::new(move |method| method.insns.remove(id))),
            }
            live.remove(position);
        }
    }
    for action in actions {
        action(method);
    }
    Ok(())
}

fn loads(saved: &SavedStack) -> Vec<Node> {
    let mut local = saved.first_local;
    saved
        .values
        .iter()
        .map(|value| {
            let node = Node::Insn(Insn::Var {
                op: value.load(),
                slot: local,
            });
            local += value.size() as u16;
            node
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::method_node::MethodNode;

    fn marker(name: &str) -> Node {
        Node::Insn(Insn::Method {
            op: INVOKESTATIC,
            owner: "kotlin/jvm/internal/InlineMarker".to_string(),
            name: name.to_string(),
            desc: "()V".to_string(),
            interface: false,
        })
    }

    #[test]
    fn a_reserved_spill_precedes_the_inline_frame_local() {
        let mut method = MethodNode::new(0x0008, "f", "()I");
        method.max_locals = 2;
        method.nodes = vec![
            Node::Insn(Insn::Op(ICONST_1)),
            marker("beforeInlineCallWithReservedLocals"),
            Node::Insn(Insn::Op(ICONST_0)),
            Node::Insn(Insn::Var {
                op: ISTORE,
                slot: 1,
            }),
            marker("afterInlineCall"),
            Node::Insn(Insn::Op(IRETURN)),
        ];
        let mut editable = EditableMethod::new(method);

        fix_stack(&mut editable, "T").expect("the reserved bracket is valid");
        let finished = editable.finish();

        assert_eq!(finished.max_locals, 2);
        assert_eq!(
            finished.nodes,
            vec![
                Node::Insn(Insn::Op(ICONST_1)),
                Node::Insn(Insn::Var {
                    op: ISTORE,
                    slot: 0,
                }),
                Node::Insn(Insn::Op(ICONST_0)),
                Node::Insn(Insn::Var {
                    op: ISTORE,
                    slot: 1,
                }),
                Node::Insn(Insn::Var { op: ILOAD, slot: 0 }),
                Node::Insn(Insn::Op(IRETURN)),
            ]
        );
    }

    #[test]
    fn an_empty_enclosing_bracket_does_not_occupy_the_reserved_local_range() {
        let mut method = MethodNode::new(0x0008, "f", "()V");
        method.max_locals = 2;
        method.nodes = vec![
            marker("beforeInlineCall"),
            Node::Insn(Insn::Op(ICONST_1)),
            marker("beforeInlineCallWithReservedLocals"),
            Node::Insn(Insn::Op(ICONST_0)),
            Node::Insn(Insn::Var {
                op: ISTORE,
                slot: 1,
            }),
            marker("afterInlineCall"),
            Node::Insn(Insn::Op(POP)),
            marker("afterInlineCall"),
            Node::Insn(Insn::Op(RETURN)),
        ];
        let mut editable = EditableMethod::new(method);

        fix_stack(&mut editable, "T")
            .expect("an empty outer save does not overlap the inner reserved spill");
        let finished = editable.finish();

        assert_eq!(finished.max_locals, 2);
        assert_eq!(
            finished.nodes,
            vec![
                Node::Insn(Insn::Op(ICONST_1)),
                Node::Insn(Insn::Var {
                    op: ISTORE,
                    slot: 0,
                }),
                Node::Insn(Insn::Op(ICONST_0)),
                Node::Insn(Insn::Var {
                    op: ISTORE,
                    slot: 1,
                }),
                Node::Insn(Insn::Var { op: ILOAD, slot: 0 }),
                Node::Insn(Insn::Op(POP)),
                Node::Insn(Insn::Op(RETURN)),
            ]
        );
    }
}
