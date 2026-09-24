//! kotlinc's `UninitializedStoresProcessor` (`processUninitializedStores.kt`): an object whose
//! `new` was saved to a local — as FixStack does around a suspend call among its constructor's
//! arguments — would be spilled uninitialized, which the verifier rejects. The `new`/`dup` move
//! down to the constructor call, after the arguments, which go through fresh locals instead.

use std::collections::{BTreeMap, BTreeSet};

use super::super::analysis::{
    analyze_with, node_opcode, AnalyzerError, AnalyzerOptions, At, BasicInterpreter, BasicValue,
    Executor, Frame, Interpreter, Value,
};
use super::super::descriptors;
use super::super::insn_list::EditableMethod;
use super::super::opcodes::*;
use super::CoroutineError;
use crate::jvm::method_node::{Insn, Node};

/// A basic value, or the not yet constructed object a `new` at this node index pushed.
#[derive(Clone, Debug, PartialEq, Eq)]
enum NewValue {
    Basic(BasicValue),
    Uninitialized { new: usize, class: String },
}

impl Value for NewValue {
    fn size(&self) -> usize {
        match self {
            NewValue::Basic(value) => value.size(),
            NewValue::Uninitialized { .. } => 1,
        }
    }
}

impl NewValue {
    fn basic(&self) -> BasicValue {
        match self {
            NewValue::Basic(value) => value.clone(),
            NewValue::Uninitialized { class, .. } => {
                BasicValue::Reference(descriptors::of_internal_name(class))
            }
        }
    }
}

fn basic(value: Option<BasicValue>) -> Option<NewValue> {
    value.map(NewValue::Basic)
}

/// `UninitializedNewValueMarkerInterpreter`.
#[derive(Default)]
struct NewValueInterpreter {
    /// Each `new`'s copies, stores, loads and pops, by node index.
    usages: BTreeMap<usize, BTreeSet<usize>>,
}

impl Interpreter for NewValueInterpreter {
    type V = NewValue;

    fn new_value(&mut self, ty: Option<&str>) -> Option<NewValue> {
        basic(BasicInterpreter.new_value(ty))
    }

    fn new_operation(&mut self, at: &At) -> Result<NewValue, AnalyzerError> {
        if let Insn::Type { op: NEW, class } = at.insn {
            self.usages.entry(at.index).or_default();
            return Ok(NewValue::Uninitialized {
                new: at.index,
                class: class.clone(),
            });
        }
        Ok(NewValue::Basic(BasicInterpreter.new_operation(at)?))
    }

    fn copy_operation(&mut self, at: &At, value: &NewValue) -> Result<NewValue, AnalyzerError> {
        if let NewValue::Uninitialized { new, .. } = value {
            if !matches!(
                at.insn,
                Insn::Op(DUP)
                    | Insn::Var {
                        op: ASTORE | ALOAD,
                        ..
                    }
            ) {
                return Err(AnalyzerError {
                    index: at.index,
                    message: "an uninitialized object copied other than by dup, astore or aload"
                        .to_string(),
                });
            }
            self.usages.entry(*new).or_default().insert(at.index);
            return Ok(value.clone());
        }
        Ok(value.clone())
    }

    fn unary_operation(
        &mut self,
        at: &At,
        value: &NewValue,
    ) -> Result<Option<NewValue>, AnalyzerError> {
        Ok(basic(BasicInterpreter.unary_operation(at, &value.basic())?))
    }

    fn binary_operation(
        &mut self,
        at: &At,
        first: &NewValue,
        second: &NewValue,
    ) -> Result<Option<NewValue>, AnalyzerError> {
        Ok(basic(BasicInterpreter.binary_operation(
            at,
            &first.basic(),
            &second.basic(),
        )?))
    }

    fn ternary_operation(
        &mut self,
        _at: &At,
        _first: &NewValue,
        _second: &NewValue,
        _third: &NewValue,
    ) -> Result<Option<NewValue>, AnalyzerError> {
        Ok(None)
    }

    fn nary_operation(
        &mut self,
        at: &At,
        values: &[NewValue],
    ) -> Result<Option<NewValue>, AnalyzerError> {
        let values: Vec<BasicValue> = values.iter().map(NewValue::basic).collect();
        Ok(basic(BasicInterpreter.nary_operation(at, &values)?))
    }

    fn return_operation(
        &mut self,
        _at: &At,
        _value: &NewValue,
        _expected: &NewValue,
    ) -> Result<(), AnalyzerError> {
        Ok(())
    }

    fn merge(&mut self, first: &NewValue, second: &NewValue) -> NewValue {
        if first == second {
            return first.clone();
        }
        let uninitialized = NewValue::Basic(BasicValue::Uninitialized);
        if *first == uninitialized || *second == uninitialized {
            return uninitialized;
        }
        match (first, second) {
            (NewValue::Uninitialized { new: a, .. }, NewValue::Uninitialized { new: b, .. })
                if a == b =>
            {
                first.clone()
            }
            (NewValue::Uninitialized { .. }, _) | (_, NewValue::Uninitialized { .. }) => {
                uninitialized
            }
            _ => NewValue::Basic(BasicInterpreter.merge(&first.basic(), &second.basic())),
        }
    }
}

/// The receiver arguments' count of a constructor call, when `insn` is one.
fn constructor_arguments(insn: &Insn) -> Option<usize> {
    match insn {
        Insn::Method { name, desc, .. } if name == "<init>" => {
            descriptors::argument_types(desc).map(|arguments| arguments.len())
        }
        _ => None,
    }
}

/// `getUninitializedValueForConstructorCall`: the object a constructor call initializes, when it
/// came from a `new` in this method.
fn uninitialized_receiver(
    frame: &Frame<NewValue>,
    insn: &Insn,
    special_method: bool,
    index: usize,
) -> Result<Option<(usize, String)>, AnalyzerError> {
    let Some(arguments) = constructor_arguments(insn) else {
        return Ok(None);
    };
    // `peek(arguments + 1)`: the value under the receiver the call consumes, which stays behind as
    // the constructed object.
    let peek = |offset: usize| {
        frame
            .stack
            .len()
            .checked_sub(offset + 1)
            .and_then(|at| frame.stack.get(at))
    };
    match peek(arguments + 1) {
        Some(NewValue::Uninitialized { new, class }) => {
            if !matches!(peek(arguments), Some(NewValue::Uninitialized { .. })) {
                return Err(AnalyzerError {
                    index,
                    message: "a constructed object not duplicated right after its `new`"
                        .to_string(),
                });
            }
            Ok(Some((*new, class.clone())))
        }
        _ if special_method => Ok(None),
        _ => Err(AnalyzerError {
            index,
            message: "a constructor call whose receiver no `new` produced".to_string(),
        }),
    }
}

/// `UninitializedNewValueFrame`: once its constructor has run, the object is an ordinary value.
struct NewValueFrames {
    special_method: bool,
}

impl Executor<NewValueInterpreter> for NewValueFrames {
    fn execute(
        &mut self,
        frame: &mut Frame<NewValue>,
        at: &At,
        interpreter: &mut NewValueInterpreter,
    ) -> Result<(), AnalyzerError> {
        let initializes = uninitialized_receiver(frame, at.insn, self.special_method, at.index)?;
        frame.execute(at, interpreter)?;
        if let Some((_, class)) = initializes {
            frame.stack.pop();
            frame.stack.push(NewValue::Basic(BasicValue::Reference(
                descriptors::of_internal_name(&class),
            )));
        }
        Ok(())
    }
}

/// `UninitializedStoresProcessor.run`.
pub(crate) fn process_uninitialized_stores(
    method: &mut EditableMethod,
) -> Result<(), CoroutineError> {
    let has_new = method
        .insns
        .ids()
        .into_iter()
        .any(|id| node_opcode(method.insns.node(id)) == Some(NEW));
    if !has_new {
        return Ok(());
    }
    let special_method = method.method.name == "<init>" || method.method.name == "<clinit>";
    let snapshot = method.snapshot();
    let mut interpreter = NewValueInterpreter::default();
    let frames = analyze_with(
        &snapshot,
        "fake",
        &mut interpreter,
        &mut NewValueFrames { special_method },
        AnalyzerOptions {
            prune_exception_edges: true,
            ..AnalyzerOptions::default()
        },
    )
    .map_err(CoroutineError::Analysis)?;
    // `analyzePopInstructions`: a `pop` of the object is one of its removable uses.
    for (index, frame) in frames.iter().enumerate() {
        let Some(frame) = frame else { continue };
        match node_opcode(&snapshot.nodes[index]) {
            Some(POP) => {
                if let Some(NewValue::Uninitialized { new, .. }) = frame.stack.last() {
                    interpreter.usages.entry(*new).or_default().insert(index);
                }
            }
            Some(POP2) => {
                let top = frame.stack.iter().rev().take(2);
                if top
                    .clone()
                    .any(|value| matches!(value, NewValue::Uninitialized { .. }))
                {
                    return Err(CoroutineError::Unsupported(
                        "a pop2 of an uninitialized object",
                    ));
                }
            }
            _ => {}
        }
    }

    let ids = method.insns.ids();
    for (index, frame) in frames.iter().enumerate() {
        let Some(frame) = frame else { continue };
        let Node::Insn(insn) = &snapshot.nodes[index] else {
            continue;
        };
        let Some((new, class)) = uninitialized_receiver(frame, insn, special_method, index)
            .map_err(CoroutineError::Analysis)?
        else {
            continue;
        };
        let usages = &interpreter.usages[&new];
        if usages.len() <= 1 {
            // Only duplicated, never stored.
            continue;
        }
        for usage in usages {
            method.insns.remove(ids[*usage]);
        }
        method.insns.remove(ids[new]);

        let arguments = constructor_arguments(insn).expect("a constructor call");
        let call = ids[index];
        let mut next_slot = method.method.max_locals;
        let mut stored = Vec::new();
        for offset in 0..arguments {
            let value = frame.stack[frame.stack.len() - 1 - offset].basic();
            let descriptor = value
                .descriptor()
                .ok_or(CoroutineError::Unsupported(
                    "an uninitialized constructor argument",
                ))?
                .to_string();
            method.insns.insert_before(
                call,
                vec![Node::Insn(Insn::Var {
                    op: descriptors::typed_opcode(&descriptor, ISTORE),
                    slot: next_slot,
                })],
            );
            next_slot += descriptors::size(&descriptor) as u16;
            stored.push(descriptor);
        }
        method.method.max_locals = method.method.max_locals.max(next_slot);
        method.insns.insert_before(
            call,
            vec![
                Node::Insn(Insn::Type {
                    op: NEW,
                    class: class.clone(),
                }),
                Node::Insn(Insn::Op(DUP)),
            ],
        );
        for descriptor in stored.iter().rev() {
            next_slot -= descriptors::size(descriptor) as u16;
            method.insns.insert_before(
                call,
                vec![Node::Insn(Insn::Var {
                    op: descriptors::typed_opcode(descriptor, ILOAD),
                    slot: next_slot,
                })],
            );
        }
    }
    Ok(())
}
