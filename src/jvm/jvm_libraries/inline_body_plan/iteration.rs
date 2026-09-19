//! Exact bytecode recognition for declaration-owned iteration bodies.

use super::{exact_parameter_null_check, loaded_int_local, loaded_reference_local};
use super::{stored_int_local, stored_reference_local, InlineDependency, MethodTarget};
use crate::jvm::classreader::{ExcEntry, C};
use crate::jvm::inline::{self, BranchTarget, Insn};
use crate::libraries::{
    InlineBodyCall, InlineBodyPlan, InlineIterationIndex, InlineIterationTraversal, LibraryCallable,
};
use crate::types::{type_name, Ty};

use super::super::JvmLibraries;

pub(super) enum RecognizedIteration<'a> {
    Plain {
        lambda_parameter: usize,
        traversal: RecognizedTraversal<'a>,
    },
    UncheckedIndex {
        lambda_parameter: usize,
        traversal: RecognizedTraversal<'a>,
    },
    CheckedIndex {
        lambda_parameter: usize,
        overflow: MethodTarget<'a>,
        traversal: RecognizedTraversal<'a>,
    },
}

pub(super) enum RecognizedTraversal<'a> {
    Iterator {
        prepare: Vec<MethodTarget<'a>>,
        has_next: MethodTarget<'a>,
        next: MethodTarget<'a>,
    },
    Array,
    Counted {
        size: MethodTarget<'a>,
        get: MethodTarget<'a>,
    },
}

impl JvmLibraries {
    /// Decode iteration from the ordinary declaration body. Kotlin's optional `$$forInline`
    /// realization retains API-version compatibility branches which are not part of the body
    /// emitted by the current compiler; the ordinary body is the authoritative exact loop shape
    /// for this plan family.
    pub(super) fn inline_iteration_body_plan(
        &self,
        callable: &LibraryCallable,
        body_descriptor: &str,
        parameter_slots: &[u16],
        decode_unavailable: &mut bool,
    ) -> Option<InlineBodyPlan> {
        let owner = callable.owner.render();
        let body = self
            .cp
            .method_code(&owner, &callable.name, body_descriptor)?;
        let instructions = inline::disassemble(&body.code)?;
        let recognized = recognize(
            callable,
            &instructions,
            &body.source_cp,
            parameter_slots,
            &body.handlers,
        )?;
        let (lambda_parameter, index, traversal) = match recognized {
            RecognizedIteration::Plain {
                lambda_parameter,
                traversal,
            } => (lambda_parameter, None, traversal),
            RecognizedIteration::UncheckedIndex {
                lambda_parameter,
                traversal,
            } => (
                lambda_parameter,
                Some(InlineIterationIndex::Unchecked),
                traversal,
            ),
            RecognizedIteration::CheckedIndex {
                lambda_parameter,
                overflow,
                traversal,
            } => {
                let overflow = match self.inline_plan_static_top_level(overflow) {
                    InlineDependency::Found(overflow) => overflow,
                    InlineDependency::Rejected => return None,
                    InlineDependency::Unavailable => {
                        *decode_unavailable = true;
                        return None;
                    }
                };
                if overflow.context_count != 0
                    || overflow.suspend
                    || !overflow.params.is_empty()
                    || overflow.ret != Ty::Unit
                {
                    return None;
                }
                (
                    lambda_parameter,
                    Some(InlineIterationIndex::Checked {
                        overflow: Box::new(InlineBodyCall {
                            callable: Box::new(overflow),
                            receiver: None,
                            arguments: Vec::new(),
                        }),
                    }),
                    traversal,
                )
            }
        };
        let Some(traversal) = self.normalize_iteration_traversal(traversal) else {
            // A dependency may be temporarily unavailable while another provider record is being
            // assembled. Do not turn that state into a process-global negative plan cache entry.
            *decode_unavailable = true;
            return None;
        };
        Some(InlineBodyPlan::Iteration {
            lambda_parameter,
            index,
            traversal,
        })
    }

    pub(super) fn normalize_iteration_traversal(
        &self,
        traversal: RecognizedTraversal<'_>,
    ) -> Option<InlineIterationTraversal> {
        let member = |target| self.inline_plan_member(target);
        Some(match traversal {
            RecognizedTraversal::Iterator {
                prepare,
                has_next,
                next,
            } => {
                let prepare = prepare
                    .into_iter()
                    .map(member)
                    .collect::<Option<Vec<_>>>()?;
                let has_next = member(has_next)?;
                let next = member(next)?;
                if prepare.iter().any(|call| !call.params.is_empty())
                    || !has_next.params.is_empty()
                    || has_next.ret != Ty::Boolean
                    || !next.params.is_empty()
                {
                    return None;
                }
                InlineIterationTraversal::Iterator {
                    prepare,
                    has_next: Box::new(has_next),
                    next: Box::new(next),
                }
            }
            RecognizedTraversal::Array => InlineIterationTraversal::Array,
            RecognizedTraversal::Counted { size, get } => {
                let size = member(size)?;
                let get = member(get)?;
                if !size.params.is_empty()
                    || size.ret != Ty::Int
                    || get.params.as_slice() != [Ty::Int]
                {
                    return None;
                }
                InlineIterationTraversal::Counted {
                    size: Box::new(size),
                    get: Box::new(get),
                }
            }
        })
    }
}

pub(super) fn recognize_collection_transform_traversal<'a>(
    instructions: &[Insn],
    source_cp: &'a [C],
    invoke: usize,
) -> Option<RecognizedTraversal<'a>> {
    let calls = instructions
        .get(..invoke)?
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| {
            let target = inline::invoked_method(instruction, source_cp)?;
            let (parameters, result) = crate::jvm::names::parse_method_descriptor(target.2)?;
            (parameters.is_empty() && result != "V").then_some((index, target, result))
        })
        .collect::<Vec<_>>();
    let has_next_position = calls
        .iter()
        .enumerate()
        .filter_map(|(position, (_, _, result))| (*result == "Z").then_some(position))
        .collect::<Vec<_>>();
    let [has_next_position] = has_next_position.as_slice() else {
        return None;
    };
    let (before_has_next, rest) = calls.split_at(*has_next_position);
    let [(_, has_next, "Z"), (next_index, next, next_result)] = rest else {
        return None;
    };
    let prepare_start = before_has_next
        .iter()
        .rposition(|(_, _, result)| !result.starts_with('L'))
        .map_or(0, |position| position + 1);
    let prepare = &before_has_next[prepare_start..];
    if prepare.is_empty() || !next_result.starts_with('L') {
        return None;
    }
    let (prepare_index, _, _) = prepare.last()?;
    let has_next_index = calls.get(*has_next_position)?.0;
    let iterator_slot = instructions
        .get(prepare_index + 1)
        .and_then(stored_reference_local)?;
    if instructions
        .get(has_next_index.checked_sub(1)?)
        .and_then(loaded_reference_local)
        != Some(iterator_slot)
        || instructions
            .get(next_index.checked_sub(1)?)
            .and_then(loaded_reference_local)
            != Some(iterator_slot)
        || !matches!(
            instructions.get(has_next_index + 1),
            Some(Insn::Branch { op: 0x99, .. })
        )
    {
        return None;
    }
    let element_store = next_index
        + 1
        + usize::from(class_name(instructions.get(next_index + 1)?, source_cp, 0xc0).is_some());
    let element_slot = instructions
        .get(element_store)
        .and_then(stored_reference_local)?;
    if instructions
        .get(invoke.checked_sub(1)?)
        .and_then(loaded_reference_local)
        != Some(element_slot)
    {
        return None;
    }
    Some(RecognizedTraversal::Iterator {
        prepare: prepare.iter().map(|(_, target, _)| *target).collect(),
        has_next: *has_next,
        next: *next,
    })
}

pub(super) fn recognize<'a>(
    callable: &LibraryCallable,
    instructions: &[Insn],
    source_cp: &'a [C],
    parameter_slots: &[u16],
    handlers: &[ExcEntry],
) -> Option<RecognizedIteration<'a>> {
    if !handlers.is_empty()
        || callable.context_count != 0
        || callable.ret != Ty::Unit
        || callable.params.len() != 2
        || callable.physical_params.len() != 2
        || parameter_slots.len() != 2
        || !callable
            .generic_sig
            .as_deref()
            .is_some_and(|signature| signature.receiver.is_some())
    {
        return None;
    }
    let Ty::Fun(action) = callable.params[1] else {
        return None;
    };
    if action.suspend
        || action.context_count != 0
        || action.has_receiver
        || action.ret != Ty::Unit
        || !matches!(action.params.len(), 1 | 2)
        || action.params.len() == 2 && action.params[0] != Ty::Int
    {
        return None;
    }
    let invoke_sites = inline::function_invoke_sites(instructions, source_cp);
    let [invoke] = invoke_sites.as_slice() else {
        return None;
    };
    let receiver_slot = parameter_slots[0];
    let lambda_slot = parameter_slots[1];
    match action.params.len() {
        1 => {
            let traversal = if let Some(traversal) = recognize_plain_iterator(
                instructions,
                source_cp,
                *invoke,
                receiver_slot,
                lambda_slot,
                action.params[0],
            ) {
                Some(traversal)
            } else if recognize_array_plain(
                callable,
                action.params[0],
                instructions,
                source_cp,
                *invoke,
                receiver_slot,
                lambda_slot,
            ) {
                Some(RecognizedTraversal::Array)
            } else if char_sequence_action_matches(
                callable.params[0],
                callable.physical_params[0],
                action.params[0],
            ) && recognize_char_sequence_plain(
                instructions,
                source_cp,
                *invoke,
                receiver_slot,
                lambda_slot,
            ) {
                counted_traversal(instructions, source_cp, 12, 16)
            } else {
                None
            }?;
            Some(RecognizedIteration::Plain {
                lambda_parameter: 1,
                traversal,
            })
        }
        2 => {
            recognize_iterable_indexed(instructions, source_cp, *invoke, receiver_slot, lambda_slot)
                .filter(|_| {
                    iterable_action_matches(
                        callable.params[0],
                        callable.physical_params[0],
                        action.params[1],
                    )
                })
                .and_then(|overflow| {
                    Some((
                        overflow,
                        iterator_traversal(instructions, source_cp, &[11], 14, 17)?,
                    ))
                })
                .map(|(overflow, traversal)| RecognizedIteration::CheckedIndex {
                    lambda_parameter: 1,
                    overflow,
                    traversal,
                })
                .or_else(|| {
                    if recognize_array_indexed(
                        callable,
                        action.params[1],
                        instructions,
                        source_cp,
                        *invoke,
                        receiver_slot,
                        lambda_slot,
                    ) {
                        Some(RecognizedTraversal::Array)
                    } else if char_sequence_action_matches(
                        callable.params[0],
                        callable.physical_params[0],
                        action.params[1],
                    ) && recognize_char_sequence_indexed(
                        instructions,
                        source_cp,
                        *invoke,
                        receiver_slot,
                        lambda_slot,
                    ) {
                        counted_traversal(instructions, source_cp, 14, 18)
                    } else {
                        None
                    }
                    .map(|traversal| RecognizedIteration::UncheckedIndex {
                        lambda_parameter: 1,
                        traversal,
                    })
                })
        }
        _ => None,
    }
}

fn iterator_traversal<'a>(
    instructions: &[Insn],
    source_cp: &'a [C],
    prepare: &[usize],
    has_next: usize,
    next: usize,
) -> Option<RecognizedTraversal<'a>> {
    Some(RecognizedTraversal::Iterator {
        prepare: prepare
            .iter()
            .map(|index| inline::invoked_method(instructions.get(*index)?, source_cp))
            .collect::<Option<Vec<_>>>()?,
        has_next: inline::invoked_method(instructions.get(has_next)?, source_cp)?,
        next: inline::invoked_method(instructions.get(next)?, source_cp)?,
    })
}

fn counted_traversal<'a>(
    instructions: &[Insn],
    source_cp: &'a [C],
    size: usize,
    get: usize,
) -> Option<RecognizedTraversal<'a>> {
    Some(RecognizedTraversal::Counted {
        size: inline::invoked_method(instructions.get(size)?, source_cp)?,
        get: inline::invoked_method(instructions.get(get)?, source_cp)?,
    })
}

fn iterable_action_matches(receiver: Ty, physical_receiver: Ty, element: Ty) -> bool {
    matches!(receiver, Ty::Obj(name, args)
        if physical_receiver.obj_internal() == Some(name)
            && args.len() == 1
            && args[0].projection_read_ty() == element)
}

fn char_sequence_action_matches(receiver: Ty, physical_receiver: Ty, element: Ty) -> bool {
    matches!(receiver, Ty::Obj(name, arguments)
        if physical_receiver.obj_internal() == Some(name) && arguments.is_empty())
        && element == Ty::Char
}

fn exact_iteration_prefix(
    instructions: &[Insn],
    source_cp: &[C],
    receiver_slot: u16,
    lambda_slot: u16,
) -> bool {
    instructions.len() >= 6
        && exact_parameter_null_check(&instructions[0..3], source_cp, receiver_slot)
        && matches!(instructions[2], Insn::Plain { op: 0xb8, .. })
        && exact_parameter_null_check(&instructions[3..6], source_cp, lambda_slot)
        && matches!(instructions[5], Insn::Plain { op: 0xb8, .. })
}

fn plain(instruction: &Insn, op: u8) -> bool {
    matches!(instruction, Insn::Plain { op: actual, operands } if *actual == op && operands.is_empty())
}

fn call_is(
    instruction: &Insn,
    source_cp: &[C],
    op: u8,
    owner: &str,
    name: &str,
    descriptor: &str,
) -> bool {
    matches!(instruction, Insn::Plain { op: actual, .. } if *actual == op)
        && inline::invoked_method(instruction, source_cp).is_some_and(
            |(actual_owner, actual_name, actual_descriptor, interface)| {
                actual_owner == owner
                    && actual_name == name
                    && actual_descriptor == descriptor
                    && interface == (op == 0xb9)
            },
        )
}

pub(super) fn class_name<'a>(instruction: &Insn, source_cp: &'a [C], op: u8) -> Option<&'a str> {
    let Insn::Plain {
        op: actual,
        operands,
    } = instruction
    else {
        return None;
    };
    let [high, low] = operands.as_slice() else {
        return None;
    };
    let Some(C::Class(name_index)) = source_cp.get(u16::from_be_bytes([*high, *low]) as usize)
    else {
        return None;
    };
    (*actual == op)
        .then(|| source_cp.get(*name_index as usize))
        .flatten()
        .and_then(|constant| match constant {
            C::Utf8(name) => Some(name.as_str()),
            _ => None,
        })
}

fn internal_branch(instruction: &Insn, op: u8, target: usize) -> bool {
    matches!(
        instruction,
        Insn::Branch {
            op: actual,
            target: BranchTarget::Internal(actual_target),
        } if *actual == op && *actual_target == target
    )
}

fn iinc(instruction: &Insn, slot: u16, amount: i16) -> bool {
    match instruction {
        Insn::Plain { op: 0x84, operands } => {
            operands.as_slice() == [slot as u8, amount as i8 as u8]
        }
        Insn::Plain { op: 0xc4, operands } if operands.first() == Some(&0x84) => {
            let Some((&high, rest)) = operands.get(1).zip(operands.get(2..)) else {
                return false;
            };
            let Some((&low, amount_bytes)) = rest.split_first() else {
                return false;
            };
            let Some((&amount_high, amount_low)) = amount_bytes.split_first() else {
                return false;
            };
            let Some(&amount_low) = amount_low.first() else {
                return false;
            };
            u16::from_be_bytes([high, low]) == slot
                && i16::from_be_bytes([amount_high, amount_low]) == amount
        }
        _ => false,
    }
}

#[derive(Clone, Copy)]
enum LocalKind {
    Int,
    Long,
    Float,
    Double,
    Reference,
}

fn typed_local_access(instruction: &Insn, slot: u16, ordinary_op: u8, compact_start: u8) -> bool {
    let Insn::Plain { op, operands } = instruction else {
        return false;
    };
    (*op == ordinary_op && operands.as_slice() == [slot as u8])
        || (slot < 4 && *op == compact_start + slot as u8 && operands.is_empty())
        || (*op == 0xc4
            && operands.as_slice() == [ordinary_op, (slot >> 8) as u8, (slot & 0xff) as u8])
}

fn typed_local_load(instruction: &Insn, slot: u16, kind: LocalKind) -> bool {
    let (ordinary, compact) = match kind {
        LocalKind::Int => (0x15, 0x1a),
        LocalKind::Long => (0x16, 0x1e),
        LocalKind::Float => (0x17, 0x22),
        LocalKind::Double => (0x18, 0x26),
        LocalKind::Reference => (0x19, 0x2a),
    };
    typed_local_access(instruction, slot, ordinary, compact)
}

fn typed_local_store(instruction: &Insn, slot: u16, kind: LocalKind) -> bool {
    let (ordinary, compact) = match kind {
        LocalKind::Int => (0x36, 0x3b),
        LocalKind::Long => (0x37, 0x3f),
        LocalKind::Float => (0x38, 0x43),
        LocalKind::Double => (0x39, 0x47),
        LocalKind::Reference => (0x3a, 0x4b),
    };
    typed_local_access(instruction, slot, ordinary, compact)
}

#[derive(Clone, Copy)]
enum ArrayElement {
    Reference,
    Primitive {
        semantic: Ty,
        load_op: u8,
        local: LocalKind,
        box_owner: &'static str,
        box_descriptor: &'static str,
    },
}

impl ArrayElement {
    fn from_callable(callable: &LibraryCallable, indexed: bool) -> Option<Self> {
        let function = if indexed { "Function2" } else { "Function1" };
        let descriptor = callable.descriptor.as_str();
        let suffix = format!("Lkotlin/jvm/functions/{function};)V");
        let receiver = descriptor.strip_suffix(&suffix)?.strip_prefix('(')?;
        Some(match receiver {
            "[Ljava/lang/Object;" => Self::Reference,
            "[B" => Self::Primitive {
                semantic: Ty::Byte,
                load_op: 0x33,
                local: LocalKind::Int,
                box_owner: "java/lang/Byte",
                box_descriptor: "(B)Ljava/lang/Byte;",
            },
            "[S" => Self::Primitive {
                semantic: Ty::Short,
                load_op: 0x35,
                local: LocalKind::Int,
                box_owner: "java/lang/Short",
                box_descriptor: "(S)Ljava/lang/Short;",
            },
            "[I" => Self::Primitive {
                semantic: Ty::Int,
                load_op: 0x2e,
                local: LocalKind::Int,
                box_owner: "java/lang/Integer",
                box_descriptor: "(I)Ljava/lang/Integer;",
            },
            "[J" => Self::Primitive {
                semantic: Ty::Long,
                load_op: 0x2f,
                local: LocalKind::Long,
                box_owner: "java/lang/Long",
                box_descriptor: "(J)Ljava/lang/Long;",
            },
            "[F" => Self::Primitive {
                semantic: Ty::Float,
                load_op: 0x30,
                local: LocalKind::Float,
                box_owner: "java/lang/Float",
                box_descriptor: "(F)Ljava/lang/Float;",
            },
            "[D" => Self::Primitive {
                semantic: Ty::Double,
                load_op: 0x31,
                local: LocalKind::Double,
                box_owner: "java/lang/Double",
                box_descriptor: "(D)Ljava/lang/Double;",
            },
            "[Z" => Self::Primitive {
                semantic: Ty::Boolean,
                load_op: 0x33,
                local: LocalKind::Int,
                box_owner: "java/lang/Boolean",
                box_descriptor: "(Z)Ljava/lang/Boolean;",
            },
            "[C" => Self::Primitive {
                semantic: Ty::Char,
                load_op: 0x34,
                local: LocalKind::Int,
                box_owner: "java/lang/Character",
                box_descriptor: "(C)Ljava/lang/Character;",
            },
            _ => return None,
        })
    }

    fn semantic_matches(self, receiver: Ty, action_element: Ty) -> bool {
        receiver.array_read_elem() == Some(action_element)
            && match self {
                Self::Reference => receiver.is_reference_array(),
                Self::Primitive { semantic, .. } => {
                    !receiver.is_reference_array() && action_element == semantic
                }
            }
    }

    fn load_op(self) -> u8 {
        match self {
            Self::Reference => 0x32,
            Self::Primitive { load_op, .. } => load_op,
        }
    }

    fn local(self) -> LocalKind {
        match self {
            Self::Reference => LocalKind::Reference,
            Self::Primitive { local, .. } => local,
        }
    }

    fn exact_box(self, instruction: &Insn, source_cp: &[C]) -> bool {
        match self {
            Self::Reference => false,
            Self::Primitive {
                box_owner,
                box_descriptor,
                ..
            } => call_is(
                instruction,
                source_cp,
                0xb8,
                box_owner,
                "valueOf",
                box_descriptor,
            ),
        }
    }
}

fn recognize_array_plain(
    callable: &LibraryCallable,
    action_element: Ty,
    instructions: &[Insn],
    source_cp: &[C],
    invoke: usize,
    receiver_slot: u16,
    lambda_slot: u16,
) -> bool {
    let Some(element) = ArrayElement::from_callable(callable, false)
        .filter(|element| element.semantic_matches(callable.params[0], action_element))
    else {
        return false;
    };
    let boxed = matches!(element, ArrayElement::Primitive { .. });
    let expected_len = if boxed { 28 } else { 27 };
    let expected_invoke = if boxed { 23 } else { 22 };
    if instructions.len() != expected_len || invoke != expected_invoke {
        return false;
    }
    let marker_slot = 2;
    let cursor_slot = 3;
    let length_slot = 4;
    let element_slot = 5;
    let mut exact = exact_iteration_prefix(instructions, source_cp, receiver_slot, lambda_slot)
        && plain(&instructions[6], 0x03)
        && stored_int_local(&instructions[7]) == Some(marker_slot)
        && plain(&instructions[8], 0x03)
        && stored_int_local(&instructions[9]) == Some(cursor_slot)
        && loaded_reference_local(&instructions[10]) == Some(receiver_slot)
        && plain(&instructions[11], 0xbe)
        && stored_int_local(&instructions[12]) == Some(length_slot)
        && loaded_int_local(&instructions[13]) == Some(cursor_slot)
        && loaded_int_local(&instructions[14]) == Some(length_slot)
        && internal_branch(&instructions[15], 0xa2, expected_len - 1)
        && loaded_reference_local(&instructions[16]) == Some(receiver_slot)
        && loaded_int_local(&instructions[17]) == Some(cursor_slot)
        && plain(&instructions[18], element.load_op())
        && typed_local_store(&instructions[19], element_slot, element.local())
        && loaded_reference_local(&instructions[20]) == Some(lambda_slot)
        && typed_local_load(&instructions[21], element_slot, element.local());
    let mut cursor = 22;
    if boxed {
        exact &= element.exact_box(&instructions[cursor], source_cp);
        cursor += 1;
    }
    exact
        && call_is(
            &instructions[cursor],
            source_cp,
            0xb9,
            "kotlin/jvm/functions/Function1",
            "invoke",
            "(Ljava/lang/Object;)Ljava/lang/Object;",
        )
        && plain(&instructions[cursor + 1], 0x57)
        && iinc(&instructions[cursor + 2], cursor_slot, 1)
        && internal_branch(&instructions[cursor + 3], 0xa7, 13)
        && plain(&instructions[cursor + 4], 0xb1)
}

fn recognize_array_indexed(
    callable: &LibraryCallable,
    action_element: Ty,
    instructions: &[Insn],
    source_cp: &[C],
    invoke: usize,
    receiver_slot: u16,
    lambda_slot: u16,
) -> bool {
    let Some(element) = ArrayElement::from_callable(callable, true)
        .filter(|element| element.semantic_matches(callable.params[0], action_element))
    else {
        return false;
    };
    let boxed = matches!(element, ArrayElement::Primitive { .. });
    let expected_len = if boxed { 33 } else { 32 };
    let expected_invoke = if boxed { 28 } else { 27 };
    if instructions.len() != expected_len || invoke != expected_invoke {
        return false;
    }
    let marker_slot = 2;
    let action_index_slot = 3;
    let cursor_slot = 4;
    let length_slot = 5;
    let element_slot = 6;
    let mut exact = exact_iteration_prefix(instructions, source_cp, receiver_slot, lambda_slot)
        && plain(&instructions[6], 0x03)
        && stored_int_local(&instructions[7]) == Some(marker_slot)
        && plain(&instructions[8], 0x03)
        && stored_int_local(&instructions[9]) == Some(action_index_slot)
        && plain(&instructions[10], 0x03)
        && stored_int_local(&instructions[11]) == Some(cursor_slot)
        && loaded_reference_local(&instructions[12]) == Some(receiver_slot)
        && plain(&instructions[13], 0xbe)
        && stored_int_local(&instructions[14]) == Some(length_slot)
        && loaded_int_local(&instructions[15]) == Some(cursor_slot)
        && loaded_int_local(&instructions[16]) == Some(length_slot)
        && internal_branch(&instructions[17], 0xa2, expected_len - 1)
        && loaded_reference_local(&instructions[18]) == Some(receiver_slot)
        && loaded_int_local(&instructions[19]) == Some(cursor_slot)
        && plain(&instructions[20], element.load_op())
        && typed_local_store(&instructions[21], element_slot, element.local())
        && loaded_reference_local(&instructions[22]) == Some(lambda_slot)
        && loaded_int_local(&instructions[23]) == Some(action_index_slot)
        && iinc(&instructions[24], action_index_slot, 1)
        && call_is(
            &instructions[25],
            source_cp,
            0xb8,
            "java/lang/Integer",
            "valueOf",
            "(I)Ljava/lang/Integer;",
        )
        && typed_local_load(&instructions[26], element_slot, element.local());
    let mut cursor = 27;
    if boxed {
        exact &= element.exact_box(&instructions[cursor], source_cp);
        cursor += 1;
    }
    exact
        && call_is(
            &instructions[cursor],
            source_cp,
            0xb9,
            "kotlin/jvm/functions/Function2",
            "invoke",
            "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
        )
        && plain(&instructions[cursor + 1], 0x57)
        && iinc(&instructions[cursor + 2], cursor_slot, 1)
        && internal_branch(&instructions[cursor + 3], 0xa7, 15)
        && plain(&instructions[cursor + 4], 0xb1)
}

fn recognize_char_sequence_plain(
    instructions: &[Insn],
    source_cp: &[C],
    invoke: usize,
    receiver_slot: u16,
    lambda_slot: u16,
) -> bool {
    if instructions.len() != 26 || invoke != 21 {
        return false;
    }
    let marker_slot = 2;
    let cursor_slot = 3;
    let element_slot = 4;
    exact_iteration_prefix(instructions, source_cp, receiver_slot, lambda_slot)
        && plain(&instructions[6], 0x03)
        && stored_int_local(&instructions[7]) == Some(marker_slot)
        && plain(&instructions[8], 0x03)
        && stored_int_local(&instructions[9]) == Some(cursor_slot)
        && loaded_int_local(&instructions[10]) == Some(cursor_slot)
        && loaded_reference_local(&instructions[11]) == Some(receiver_slot)
        && call_is(
            &instructions[12],
            source_cp,
            0xb9,
            "java/lang/CharSequence",
            "length",
            "()I",
        )
        && internal_branch(&instructions[13], 0xa2, 25)
        && loaded_reference_local(&instructions[14]) == Some(receiver_slot)
        && loaded_int_local(&instructions[15]) == Some(cursor_slot)
        && call_is(
            &instructions[16],
            source_cp,
            0xb9,
            "java/lang/CharSequence",
            "charAt",
            "(I)C",
        )
        && stored_int_local(&instructions[17]) == Some(element_slot)
        && loaded_reference_local(&instructions[18]) == Some(lambda_slot)
        && loaded_int_local(&instructions[19]) == Some(element_slot)
        && call_is(
            &instructions[20],
            source_cp,
            0xb8,
            "java/lang/Character",
            "valueOf",
            "(C)Ljava/lang/Character;",
        )
        && call_is(
            &instructions[21],
            source_cp,
            0xb9,
            "kotlin/jvm/functions/Function1",
            "invoke",
            "(Ljava/lang/Object;)Ljava/lang/Object;",
        )
        && plain(&instructions[22], 0x57)
        && iinc(&instructions[23], cursor_slot, 1)
        && internal_branch(&instructions[24], 0xa7, 10)
        && plain(&instructions[25], 0xb1)
}

fn recognize_char_sequence_indexed(
    instructions: &[Insn],
    source_cp: &[C],
    invoke: usize,
    receiver_slot: u16,
    lambda_slot: u16,
) -> bool {
    if instructions.len() != 31 || invoke != 26 {
        return false;
    }
    let marker_slot = 2;
    let action_index_slot = 3;
    let cursor_slot = 4;
    let element_slot = 5;
    exact_iteration_prefix(instructions, source_cp, receiver_slot, lambda_slot)
        && plain(&instructions[6], 0x03)
        && stored_int_local(&instructions[7]) == Some(marker_slot)
        && plain(&instructions[8], 0x03)
        && stored_int_local(&instructions[9]) == Some(action_index_slot)
        && plain(&instructions[10], 0x03)
        && stored_int_local(&instructions[11]) == Some(cursor_slot)
        && loaded_int_local(&instructions[12]) == Some(cursor_slot)
        && loaded_reference_local(&instructions[13]) == Some(receiver_slot)
        && call_is(
            &instructions[14],
            source_cp,
            0xb9,
            "java/lang/CharSequence",
            "length",
            "()I",
        )
        && internal_branch(&instructions[15], 0xa2, 30)
        && loaded_reference_local(&instructions[16]) == Some(receiver_slot)
        && loaded_int_local(&instructions[17]) == Some(cursor_slot)
        && call_is(
            &instructions[18],
            source_cp,
            0xb9,
            "java/lang/CharSequence",
            "charAt",
            "(I)C",
        )
        && stored_int_local(&instructions[19]) == Some(element_slot)
        && loaded_reference_local(&instructions[20]) == Some(lambda_slot)
        && loaded_int_local(&instructions[21]) == Some(action_index_slot)
        && iinc(&instructions[22], action_index_slot, 1)
        && call_is(
            &instructions[23],
            source_cp,
            0xb8,
            "java/lang/Integer",
            "valueOf",
            "(I)Ljava/lang/Integer;",
        )
        && loaded_int_local(&instructions[24]) == Some(element_slot)
        && call_is(
            &instructions[25],
            source_cp,
            0xb8,
            "java/lang/Character",
            "valueOf",
            "(C)Ljava/lang/Character;",
        )
        && call_is(
            &instructions[26],
            source_cp,
            0xb9,
            "kotlin/jvm/functions/Function2",
            "invoke",
            "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
        )
        && plain(&instructions[27], 0x57)
        && iinc(&instructions[28], cursor_slot, 1)
        && internal_branch(&instructions[29], 0xa7, 12)
        && plain(&instructions[30], 0xb1)
}

fn zero_argument_instance_call<'a>(
    instruction: &Insn,
    source_cp: &'a [C],
) -> Option<(MethodTarget<'a>, Ty)> {
    let Insn::Plain { op, .. } = instruction else {
        return None;
    };
    let interface = match *op {
        0xb6 => false,
        0xb9 => true,
        _ => return None,
    };
    let target = inline::invoked_method(instruction, source_cp)?;
    if target.3 != interface {
        return None;
    }
    let (parameters, result) = super::super::parse_method_desc(target.2)?;
    parameters.is_empty().then_some((target, result))
}

fn erased_cast_matches_element(cast: Option<&str>, element: Ty) -> bool {
    cast.is_none_or(|cast| {
        let physical = type_name(cast);
        let semantic = crate::jvm::jvm_class_map::jvm_to_kotlin_builtin_metadata_name(physical)
            .unwrap_or(physical);
        element.non_null().obj_internal() == Some(semantic)
    })
}

/// Recognize the complete unary iterator-loop grammar without assigning meaning to any callable
/// spelling. A declaration may prepare its iterator through one or more zero-argument member calls;
/// every physical target is retained for metadata normalization and stable identity publication.
fn recognize_plain_iterator<'a>(
    instructions: &[Insn],
    source_cp: &'a [C],
    invoke: usize,
    receiver_slot: u16,
    lambda_slot: u16,
    element: Ty,
) -> Option<RecognizedTraversal<'a>> {
    if instructions.len() < 23
        || !exact_iteration_prefix(instructions, source_cp, receiver_slot, lambda_slot)
    {
        return None;
    }
    let marker_slot = lambda_slot.checked_add(1)?;
    let iterator_slot = marker_slot.checked_add(1)?;
    let element_slot = iterator_slot.checked_add(1)?;
    if !plain(instructions.get(6)?, 0x03)
        || stored_int_local(instructions.get(7)?) != Some(marker_slot)
        || loaded_reference_local(instructions.get(8)?) != Some(receiver_slot)
    {
        return None;
    }

    let mut cursor = 9;
    let mut prepare = Vec::new();
    while let Some((target, result)) = instructions
        .get(cursor)
        .and_then(|instruction| zero_argument_instance_call(instruction, source_cp))
    {
        if !result.is_reference() {
            return None;
        }
        prepare.push(target);
        cursor += 1;
    }
    if prepare.is_empty()
        || stored_reference_local(instructions.get(cursor)?) != Some(iterator_slot)
    {
        return None;
    }
    cursor += 1;

    let loop_head = cursor;
    if loaded_reference_local(instructions.get(cursor)?) != Some(iterator_slot) {
        return None;
    }
    cursor += 1;
    let (has_next, has_next_result) =
        zero_argument_instance_call(instructions.get(cursor)?, source_cp)?;
    if has_next_result != Ty::Boolean {
        return None;
    }
    cursor += 1;
    let return_index = instructions.len().checked_sub(1)?;
    if !internal_branch(instructions.get(cursor)?, 0x99, return_index) {
        return None;
    }
    cursor += 1;

    if loaded_reference_local(instructions.get(cursor)?) != Some(iterator_slot) {
        return None;
    }
    cursor += 1;
    let (next, next_result) = zero_argument_instance_call(instructions.get(cursor)?, source_cp)?;
    if !next_result.is_reference() {
        return None;
    }
    cursor += 1;
    let erased_cast = class_name(instructions.get(cursor)?, source_cp, 0xc0);
    if erased_cast.is_some() {
        cursor += 1;
    }
    if !erased_cast_matches_element(erased_cast, element)
        || stored_reference_local(instructions.get(cursor)?) != Some(element_slot)
    {
        return None;
    }
    cursor += 1;

    if loaded_reference_local(instructions.get(cursor)?) != Some(lambda_slot)
        || loaded_reference_local(instructions.get(cursor + 1)?) != Some(element_slot)
        || invoke != cursor + 2
        || !call_is(
            instructions.get(cursor + 2)?,
            source_cp,
            0xb9,
            "kotlin/jvm/functions/Function1",
            "invoke",
            "(Ljava/lang/Object;)Ljava/lang/Object;",
        )
        || !plain(instructions.get(cursor + 3)?, 0x57)
        || !internal_branch(instructions.get(cursor + 4)?, 0xa7, loop_head)
        || cursor + 5 != return_index
        || !plain(instructions.get(return_index)?, 0xb1)
    {
        return None;
    }

    Some(RecognizedTraversal::Iterator {
        prepare,
        has_next,
        next,
    })
}

fn recognize_iterable_indexed<'a>(
    instructions: &[Insn],
    source_cp: &'a [C],
    invoke: usize,
    receiver_slot: u16,
    lambda_slot: u16,
) -> Option<MethodTarget<'a>> {
    if instructions.len() != 33 || invoke != 29 {
        return None;
    }
    let marker_slot = 2;
    let action_index_slot = 3;
    let iterator_slot = 4;
    let element_slot = 5;
    let current_index_slot = 6;
    let overflow = inline::invoked_method(&instructions[25], source_cp)?;
    if !matches!(instructions[25], Insn::Plain { op: 0xb8, .. })
        || overflow.2 != "()V"
        || overflow.3
    {
        return None;
    }
    let exact = exact_iteration_prefix(instructions, source_cp, receiver_slot, lambda_slot)
        && plain(&instructions[6], 0x03)
        && stored_int_local(&instructions[7]) == Some(marker_slot)
        && plain(&instructions[8], 0x03)
        && stored_int_local(&instructions[9]) == Some(action_index_slot)
        && loaded_reference_local(&instructions[10]) == Some(receiver_slot)
        && call_is(
            &instructions[11],
            source_cp,
            0xb9,
            "java/lang/Iterable",
            "iterator",
            "()Ljava/util/Iterator;",
        )
        && stored_reference_local(&instructions[12]) == Some(iterator_slot)
        && loaded_reference_local(&instructions[13]) == Some(iterator_slot)
        && call_is(
            &instructions[14],
            source_cp,
            0xb9,
            "java/util/Iterator",
            "hasNext",
            "()Z",
        )
        && internal_branch(&instructions[15], 0x99, 32)
        && loaded_reference_local(&instructions[16]) == Some(iterator_slot)
        && call_is(
            &instructions[17],
            source_cp,
            0xb9,
            "java/util/Iterator",
            "next",
            "()Ljava/lang/Object;",
        )
        && stored_reference_local(&instructions[18]) == Some(element_slot)
        && loaded_reference_local(&instructions[19]) == Some(lambda_slot)
        && loaded_int_local(&instructions[20]) == Some(action_index_slot)
        && iinc(&instructions[21], action_index_slot, 1)
        && stored_int_local(&instructions[22]) == Some(current_index_slot)
        && loaded_int_local(&instructions[23]) == Some(current_index_slot)
        && internal_branch(&instructions[24], 0x9c, 26)
        && loaded_int_local(&instructions[26]) == Some(current_index_slot)
        && call_is(
            &instructions[27],
            source_cp,
            0xb8,
            "java/lang/Integer",
            "valueOf",
            "(I)Ljava/lang/Integer;",
        )
        && loaded_reference_local(&instructions[28]) == Some(element_slot)
        && call_is(
            &instructions[29],
            source_cp,
            0xb9,
            "kotlin/jvm/functions/Function2",
            "invoke",
            "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
        )
        && plain(&instructions[30], 0x57)
        && internal_branch(&instructions[31], 0xa7, 13)
        && plain(&instructions[32], 0xb1);
    exact.then_some(overflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::jvm_libraries::JvmLibraries;
    use crate::libraries::{
        InlineBodyCallReceiver, InlineBodyPlan, InlineIterationIndex, InlineIterationTraversal,
    };
    use crate::symbol_source::SymbolNamespace;
    use crate::types::type_name;

    #[derive(Clone, Copy)]
    enum ExpectedIndex {
        Plain,
        Unchecked,
    }

    #[derive(Clone, Copy)]
    enum ExpectedTraversal {
        Iterator(usize),
        Array,
        Counted,
    }

    pub(super) fn callable(
        libraries: &JvmLibraries,
        package: &str,
        name: &str,
        descriptor: &str,
    ) -> LibraryCallable {
        let symbols = libraries.symbols(SymbolNamespace::Package(type_name(package)), name);
        symbols
            .callables
            .functions()
            .iter()
            .find(|function| function.callable.descriptor == descriptor)
            .unwrap_or_else(|| panic!("missing {package}.{name}{descriptor}"))
            .callable
            .clone()
    }

    fn assert_plan(
        libraries: &JvmLibraries,
        package: &str,
        name: &str,
        descriptor: &str,
        expected: ExpectedIndex,
        expected_traversal: ExpectedTraversal,
    ) {
        let callable = callable(libraries, package, name, descriptor);
        let Some(InlineBodyPlan::Iteration {
            index, traversal, ..
        }) = callable.inline_body_plan.as_deref()
        else {
            panic!(
                "{package}.{name}{descriptor} did not publish an iteration plan: callable={callable:?}"
            )
        };
        match expected {
            ExpectedIndex::Plain => assert!(index.is_none(), "{package}.{name}{descriptor}"),
            ExpectedIndex::Unchecked => assert!(
                matches!(index, Some(InlineIterationIndex::Unchecked)),
                "{package}.{name}{descriptor}"
            ),
        }
        match (expected_traversal, traversal) {
            (
                ExpectedTraversal::Iterator(expected_prepare),
                InlineIterationTraversal::Iterator {
                    prepare,
                    has_next,
                    next,
                },
            ) => {
                assert_eq!(
                    prepare.len(),
                    expected_prepare,
                    "{package}.{name}{descriptor}"
                );
                assert!(
                    prepare
                        .iter()
                        .chain([has_next.as_ref(), next.as_ref()])
                        .all(|member| member.external_identity.is_some()),
                    "{package}.{name}{descriptor} must publish every iterator dependency"
                );
            }
            (ExpectedTraversal::Array, InlineIterationTraversal::Array)
            | (ExpectedTraversal::Counted, InlineIterationTraversal::Counted { .. }) => {}
            _ => {
                panic!("{package}.{name}{descriptor} published the wrong traversal: {traversal:?}")
            }
        }
    }

    #[test]
    fn collection_transforms_publish_exact_bytecode_owned_traversal_identities() {
        let stdlib = crate::toolchain::stdlib_jar()
            .expect("collection-transform provider test requires the repository Kotlin stdlib");
        let jdk = crate::toolchain::jdk_modules()
            .expect("collection-transform provider test requires the repository JDK modules");
        let libraries = JvmLibraries::new(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            vec![stdlib, jdk],
        )))
        .expect("JVM provider initialization");
        for (
            name,
            descriptor,
            prepare_members,
            factory_descriptor,
            capacity_descriptor,
            append_descriptor,
            expected_local_names,
        ) in [
            (
                "map",
                "(Ljava/lang/Iterable;Lkotlin/jvm/functions/Function1;)Ljava/util/List;",
                &[("iterator", None, "()Ljava/util/Iterator;")][..],
                "(I)V",
                Some("(Ljava/lang/Iterable;I)I"),
                "(Ljava/lang/Object;)Z",
                ("map", "mapTo", "destination", "item"),
            ),
            (
                "flatMap",
                "(Ljava/lang/Iterable;Lkotlin/jvm/functions/Function1;)Ljava/util/List;",
                &[("iterator", None, "()Ljava/util/Iterator;")][..],
                "()V",
                None,
                "(Ljava/util/Collection;Ljava/lang/Iterable;)Z",
                ("flatMap", "flatMapTo", "destination", "element"),
            ),
            (
                "map",
                "(Ljava/util/Map;Lkotlin/jvm/functions/Function1;)Ljava/util/List;",
                &[
                    ("entries", Some("entrySet"), "()Ljava/util/Set;"),
                    ("iterator", None, "()Ljava/util/Iterator;"),
                ][..],
                "(I)V",
                Some("()I"),
                "(Ljava/lang/Object;)Z",
                ("map", "mapTo", "destination", "item"),
            ),
        ] {
            let callable = callable(&libraries, "kotlin/collections", name, descriptor);
            let Some(InlineBodyPlan::CollectionTransform {
                traversal:
                    InlineIterationTraversal::Iterator {
                        prepare,
                        has_next,
                        next,
                    },
                factory,
                capacity,
                append,
                local_names,
                ..
            }) = callable.inline_body_plan.as_deref()
            else {
                panic!("{name}{descriptor} must publish its collection-transform identities")
            };
            assert_eq!(
                prepare
                    .iter()
                    .map(|member| {
                        (
                            member.name.as_str(),
                            member.physical_name.as_deref(),
                            member.descriptor.as_str(),
                        )
                    })
                    .collect::<Vec<_>>(),
                prepare_members
            );
            assert_eq!(
                (
                    has_next.name.as_str(),
                    has_next.descriptor.as_str(),
                    next.name.as_str(),
                    next.descriptor.as_str(),
                ),
                ("hasNext", "()Z", "next", "()Ljava/lang/Object;",)
            );
            assert_eq!(
                (factory.name.as_str(), factory.descriptor.as_str()),
                ("<init>", factory_descriptor)
            );
            assert_eq!(
                (
                    local_names.outer_receiver.as_ref(),
                    local_names.inner_receiver.as_ref(),
                    local_names.destination.as_ref(),
                    local_names.element.as_ref(),
                ),
                expected_local_names,
                "local names must come from the exact declaration body LVT"
            );
            let capacity_identity = match (capacity_descriptor, capacity) {
                (
                    Some(expected),
                    Some(crate::libraries::InlineCollectionCapacity::Member(member)),
                ) => {
                    assert_eq!(member.descriptor, expected);
                    member.external_identity
                }
                (
                    Some(expected),
                    Some(crate::libraries::InlineCollectionCapacity::Extension {
                        callable,
                        default,
                    }),
                ) => {
                    assert_eq!((callable.descriptor.as_str(), *default), (expected, 10));
                    callable.external_identity
                }
                (None, None) => None,
                _ => panic!("{name}{descriptor} published the wrong capacity dependency"),
            };
            let append_identity = match append {
                crate::libraries::InlineCollectionAppend::Member(member) => {
                    assert_eq!(member.descriptor, append_descriptor);
                    member.external_identity
                }
                crate::libraries::InlineCollectionAppend::Extension(callable) => {
                    assert_eq!(callable.descriptor, append_descriptor);
                    callable.external_identity
                }
            };
            let mut identities = prepare
                .iter()
                .map(|member| member.external_identity)
                .chain([
                    has_next.external_identity,
                    next.external_identity,
                    factory.external_identity,
                    append_identity,
                ])
                .collect::<Vec<_>>();
            if capacity_descriptor.is_some() {
                identities.push(capacity_identity);
            }
            assert!(identities.iter().all(Option::is_some));
            assert_eq!(
                identities
                    .iter()
                    .copied()
                    .flatten()
                    .collect::<std::collections::HashSet<_>>()
                    .len(),
                identities.len()
            );
        }
    }

    fn decoded_body(
        libraries: &JvmLibraries,
        callable: &LibraryCallable,
    ) -> (Vec<Insn>, Vec<C>, Vec<ExcEntry>) {
        let owner = callable.owner.render();
        let body = libraries
            .cp
            .method_code(&owner, &callable.name, &callable.descriptor)
            .unwrap_or_else(|| panic!("missing body for {}{}", callable.name, callable.descriptor));
        (
            inline::disassemble(&body.code).expect("valid stdlib iteration bytecode"),
            body.source_cp,
            body.handlers,
        )
    }

    fn recognize_body<'a>(
        callable: &LibraryCallable,
        instructions: &[Insn],
        source_cp: &'a [C],
        handlers: &[ExcEntry],
    ) -> Option<RecognizedIteration<'a>> {
        recognize(callable, instructions, source_cp, &[0, 1], handlers)
    }

    #[test]
    fn stdlib_map_array_and_text_shapes_publish_exact_plans() {
        // `toolchain::stdlib_jar()` is selected by the toolchain matrix, so this survey runs against
        // both the Kotlin 2.4.0 and 2.4.10 stdlibs in their respective CI jobs.
        let version = crate::toolchain::kotlin_version();
        if !matches!(version.as_str(), "2.4.0" | "2.4.10") {
            return;
        }
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = JvmLibraries::new(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            vec![stdlib],
        )))
        .expect("JVM provider initialization");
        assert_plan(
            &libraries,
            "kotlin/collections",
            "forEach",
            "(Ljava/util/Map;Lkotlin/jvm/functions/Function1;)V",
            ExpectedIndex::Plain,
            ExpectedTraversal::Iterator(2),
        );
        assert_plan(
            &libraries,
            "kotlin/collections",
            "forEach",
            "(Ljava/lang/Iterable;Lkotlin/jvm/functions/Function1;)V",
            ExpectedIndex::Plain,
            ExpectedTraversal::Iterator(1),
        );
        for receiver in [
            "[Ljava/lang/Object;",
            "[B",
            "[S",
            "[I",
            "[J",
            "[F",
            "[D",
            "[Z",
            "[C",
        ] {
            assert_plan(
                &libraries,
                "kotlin/collections",
                "forEach",
                &format!("({receiver}Lkotlin/jvm/functions/Function1;)V"),
                ExpectedIndex::Plain,
                ExpectedTraversal::Array,
            );
            assert_plan(
                &libraries,
                "kotlin/collections",
                "forEachIndexed",
                &format!("({receiver}Lkotlin/jvm/functions/Function2;)V"),
                ExpectedIndex::Unchecked,
                ExpectedTraversal::Array,
            );
        }
        assert_plan(
            &libraries,
            "kotlin/text",
            "forEach",
            "(Ljava/lang/CharSequence;Lkotlin/jvm/functions/Function1;)V",
            ExpectedIndex::Plain,
            ExpectedTraversal::Counted,
        );
        assert_plan(
            &libraries,
            "kotlin/text",
            "forEachIndexed",
            "(Ljava/lang/CharSequence;Lkotlin/jvm/functions/Function2;)V",
            ExpectedIndex::Unchecked,
            ExpectedTraversal::Counted,
        );

        let indexed_iterable = callable(
            &libraries,
            "kotlin/collections",
            "forEachIndexed",
            "(Ljava/lang/Iterable;Lkotlin/jvm/functions/Function2;)V",
        );
        let Some(InlineBodyPlan::Iteration {
            index: Some(InlineIterationIndex::Checked { overflow }),
            ..
        }) = indexed_iterable.inline_body_plan.as_deref()
        else {
            panic!("Iterable.forEachIndexed must publish a checked iteration plan")
        };
        assert!(overflow.receiver.is_none());
        assert!(overflow.arguments.is_empty());
        assert_eq!(
            overflow.callable.owner,
            type_name("kotlin/collections/CollectionsKt")
        );
        assert_eq!(overflow.callable.name, "throwIndexOverflow");
        assert_eq!(overflow.callable.descriptor, "()V");
        assert_eq!(overflow.callable.params, Vec::<Ty>::new());
        assert_eq!(overflow.callable.ret, Ty::Unit);
        assert!(!overflow.callable.suspend);
        assert!(overflow.callable.external_identity.is_some());
        assert!(!matches!(
            overflow.receiver,
            Some(InlineBodyCallReceiver::Dispatch(_))
        ));
    }

    #[test]
    fn text_iteration_plans_survive_jdk_member_enrichment() {
        let version = crate::toolchain::kotlin_version();
        if !matches!(version.as_str(), "2.4.0" | "2.4.10") {
            return;
        }
        let (Some(stdlib), Some(jdk)) = (
            crate::toolchain::stdlib_jar(),
            crate::toolchain::jdk_modules(),
        ) else {
            return;
        };
        let libraries = JvmLibraries::new(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            vec![stdlib, jdk],
        )))
        .expect("JVM provider initialization");
        assert_plan(
            &libraries,
            "kotlin/text",
            "forEach",
            "(Ljava/lang/CharSequence;Lkotlin/jvm/functions/Function1;)V",
            ExpectedIndex::Plain,
            ExpectedTraversal::Counted,
        );
        assert_plan(
            &libraries,
            "kotlin/text",
            "forEachIndexed",
            "(Ljava/lang/CharSequence;Lkotlin/jvm/functions/Function2;)V",
            ExpectedIndex::Unchecked,
            ExpectedTraversal::Counted,
        );
        let callable = callable(
            &libraries,
            "kotlin/text",
            "forEach",
            "(Ljava/lang/CharSequence;Lkotlin/jvm/functions/Function1;)V",
        );
        let Some(InlineBodyPlan::Iteration {
            traversal: InlineIterationTraversal::Counted { get, .. },
            ..
        }) = callable.inline_body_plan.as_deref()
        else {
            panic!("CharSequence.forEach must retain its counted traversal")
        };
        assert_eq!(get.name, "get");
        assert_eq!(get.physical_name.as_deref(), Some("charAt"));
    }

    #[test]
    fn plain_iterator_recognizer_uses_body_shape_and_rejects_a_wrong_erased_cast() {
        let version = crate::toolchain::kotlin_version();
        if !matches!(version.as_str(), "2.4.0" | "2.4.10") {
            return;
        }
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = JvmLibraries::new(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            vec![stdlib],
        )))
        .expect("JVM provider initialization");
        let mut declaration = callable(
            &libraries,
            "kotlin/collections",
            "forEach",
            "(Ljava/util/Map;Lkotlin/jvm/functions/Function1;)V",
        );
        let (instructions, mut source_cp, handlers) = decoded_body(&libraries, &declaration);

        // Recognition belongs to the exact declaration body, not the source spelling used to find
        // that declaration. Renaming only this test carrier must not change the decoded plan.
        declaration.name = "visitEveryElement".to_string();
        assert!(matches!(
            recognize_body(&declaration, &instructions, &source_cp, &handlers),
            Some(RecognizedIteration::Plain {
                traversal: RecognizedTraversal::Iterator { ref prepare, .. },
                ..
            }) if prepare.len() == 2
        ));

        let cast = instructions
            .iter()
            .position(|instruction| matches!(instruction, Insn::Plain { op: 0xc0, .. }))
            .expect("stdlib loop must retain its erased element cast");
        let name_index = u16::try_from(source_cp.len()).expect("constant pool fits u16");
        source_cp.push(C::Utf8("java/lang/String".to_string()));
        let class_index = u16::try_from(source_cp.len()).expect("constant pool fits u16");
        source_cp.push(C::Class(name_index));
        let mut wrong_cast = instructions.clone();
        wrong_cast[cast] = Insn::Plain {
            op: 0xc0,
            operands: class_index.to_be_bytes().to_vec(),
        };
        assert!(recognize_body(&declaration, &wrong_cast, &source_cp, &handlers).is_none());
    }

    #[test]
    fn iterable_recognizer_rejects_effect_branch_increment_and_handler_mutations() {
        let version = crate::toolchain::kotlin_version();
        if !matches!(version.as_str(), "2.4.0" | "2.4.10") {
            return;
        }
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = JvmLibraries::new(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            vec![stdlib],
        )))
        .expect("JVM provider initialization");
        let plain = callable(
            &libraries,
            "kotlin/collections",
            "forEach",
            "(Ljava/lang/Iterable;Lkotlin/jvm/functions/Function1;)V",
        );
        let (instructions, source_cp, handlers) = decoded_body(&libraries, &plain);
        assert!(matches!(
            recognize_body(&plain, &instructions, &source_cp, &handlers),
            Some(RecognizedIteration::Plain { .. })
        ));

        let mut extra_effect = instructions.clone();
        extra_effect.insert(
            6,
            Insn::Plain {
                op: 0x00,
                operands: Vec::new(),
            },
        );
        assert!(recognize_body(&plain, &extra_effect, &source_cp, &handlers).is_none());

        let mut wrong_branch = instructions.clone();
        wrong_branch[13] = Insn::Branch {
            op: 0x99,
            target: BranchTarget::Internal(21),
        };
        assert!(recognize_body(&plain, &wrong_branch, &source_cp, &handlers).is_none());

        let unexpected_handler = [ExcEntry {
            start_pc: 0,
            end_pc: 1,
            handler_pc: 1,
            catch_type: 0,
        }];
        assert!(recognize_body(&plain, &instructions, &source_cp, &unexpected_handler).is_none());

        let indexed = callable(
            &libraries,
            "kotlin/collections",
            "forEachIndexed",
            "(Ljava/lang/Iterable;Lkotlin/jvm/functions/Function2;)V",
        );
        let (mut instructions, source_cp, handlers) = decoded_body(&libraries, &indexed);
        assert!(matches!(
            recognize_body(&indexed, &instructions, &source_cp, &handlers),
            Some(RecognizedIteration::CheckedIndex { .. })
        ));
        instructions[21] = Insn::Plain {
            op: 0x84,
            operands: vec![3, 2],
        };
        assert!(recognize_body(&indexed, &instructions, &source_cp, &handlers).is_none());
    }
}
