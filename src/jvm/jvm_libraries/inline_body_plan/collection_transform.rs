//! Strict recognition of declaration-owned collection-transform bodies.

use super::iteration::{class_name, recognize_collection_transform_traversal, RecognizedTraversal};
use super::{
    exact_parameter_null_check, loaded_reference_local, stored_int_local, stored_reference_local,
    InlineDependency, MethodTarget,
};
use crate::jvm::classreader::{MethodCode, MethodLocal};
use crate::jvm::inline::{self, BranchTarget, Insn};
use crate::libraries::{
    InlineBodyPlan, InlineCollectionAppend, InlineCollectionCapacity, InlineCollectionLocalNames,
    LibraryCallable,
};
use crate::types::Ty;

use super::super::JvmLibraries;

#[derive(Clone, Copy)]
enum RecognizedCapacity<'a> {
    Member(MethodTarget<'a>),
    Extension {
        target: MethodTarget<'a>,
        default: i32,
    },
}

#[derive(Clone, Copy)]
enum RecognizedAppend<'a> {
    Member(MethodTarget<'a>),
    Extension(MethodTarget<'a>),
}

struct RecognizedCollectionTransform<'a> {
    lambda_parameter: usize,
    traversal: RecognizedTraversal<'a>,
    factory: MethodTarget<'a>,
    capacity: Option<RecognizedCapacity<'a>>,
    append: RecognizedAppend<'a>,
    local_names: InlineCollectionLocalNames,
}

pub(super) enum CollectionTransformDecode {
    NotRecognized,
    Rejected,
    Unavailable,
    Plan(InlineBodyPlan),
}

fn plain(instruction: Option<&Insn>, op: u8) -> bool {
    matches!(instruction, Some(Insn::Plain { op: actual, operands }) if *actual == op && operands.is_empty())
}

fn opcode(instruction: Option<&Insn>, op: u8) -> bool {
    matches!(instruction, Some(Insn::Plain { op: actual, .. }) if *actual == op)
}

fn integer_constant(instruction: &Insn) -> Option<i32> {
    let Insn::Plain { op, operands } = instruction else {
        return None;
    };
    match (*op, operands.as_slice()) {
        (0x02..=0x08, []) => Some(i32::from(*op) - 3),
        (0x10, [value]) => Some(i32::from(*value as i8)),
        (0x11, [high, low]) => Some(i32::from(i16::from_be_bytes([*high, *low]))),
        _ => None,
    }
}

fn descriptor_for_class(name: &str) -> String {
    format!("L{name};")
}

fn covered_local<'a>(
    body: &'a MethodCode,
    slot: u16,
    descriptor: &str,
    coverage_start: usize,
    coverage_end: usize,
) -> Option<&'a MethodLocal> {
    let mut matches = body.locals.iter().filter(|local| {
        let start = usize::from(local.start_pc);
        let end = start.checked_add(usize::from(local.length));
        local.slot == slot
            && local.descriptor == descriptor
            && start <= coverage_start
            && end.is_some_and(|end| end >= coverage_end)
    });
    let local = matches.next()?;
    matches.next().is_none().then_some(local)
}

fn stripped_local_name(
    local: &MethodLocal,
    receiver: bool,
    inline_depth: usize,
) -> Option<Box<str>> {
    let mut name = if receiver {
        local.name.strip_prefix("$this$")?
    } else {
        local.name.as_str()
    };
    for _ in 0..inline_depth {
        name = name.strip_suffix("$iv")?;
    }
    (!name.is_empty()).then(|| name.into())
}

fn only_invocation<'a>(
    instructions: &[Insn],
    source_cp: &'a [crate::jvm::classreader::C],
    range: std::ops::Range<usize>,
) -> Option<(usize, MethodTarget<'a>)> {
    let mut calls = instructions
        .get(range.clone())?
        .iter()
        .enumerate()
        .filter_map(|(offset, instruction)| {
            inline::invoked_method(instruction, source_cp)
                .map(|target| (range.start + offset, target))
        });
    let call = calls.next()?;
    calls.next().is_none().then_some(call)
}

fn collection_transform_lambda_parameter(
    callable: &LibraryCallable,
    parameter_slots: &[u16],
) -> Option<usize> {
    if !callable.inline.can_inline()
        || callable.suspend
        || callable.context_count != 0
        || callable.params.len() != 2
        || callable.physical_params.len() != 2
        || parameter_slots.len() != 2
        || parameter_slots
            .iter()
            .enumerate()
            .any(|(index, slot)| parameter_slots[..index].contains(slot))
    {
        return None;
    }
    let generic = callable.generic_sig.as_deref()?;
    if generic.ret.type_args().len() != 1 || generic.receiver.is_none() {
        return None;
    }
    callable
        .params
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(parameter, ty)| {
            (parameter != callable.context_count)
                .then_some(ty)
                .and_then(|ty| match ty {
                    Ty::Fun(action)
                        if !action.suspend
                            && action.context_count == 0
                            && !action.has_receiver
                            && action.params.len() == 1 =>
                    {
                        Some(parameter)
                    }
                    _ => None,
                })
        })
        .next()
}

fn recognize<'a>(
    callable: &LibraryCallable,
    body_descriptor: &str,
    parameter_slots: &[u16],
    body: &'a MethodCode,
    instructions: &[Insn],
) -> Option<RecognizedCollectionTransform<'a>> {
    // This decoder's bytecode template has exactly one extension receiver and one function
    // argument. Their indices are still discovered from the normalized signature below; the
    // cardinality guard prevents a different body shape from being partially decoded.
    let lambda_parameter = collection_transform_lambda_parameter(callable, parameter_slots)?;
    if !body.handlers.is_empty() {
        return None;
    }
    let receiver_parameter = callable.context_count;
    let (physical_parameters, physical_result) =
        crate::jvm::names::parse_method_descriptor(body_descriptor)?;
    if physical_parameters.len() != callable.physical_params.len() {
        return None;
    }
    let receiver_descriptor = physical_parameters.get(receiver_parameter)?;

    // Consume the complete declaration prefix. The two parameter checks and two inline-marker
    // locals are representation scaffolding; every other operation below is part of the published
    // transform. Fixing every instruction position here prevents an unmodeled invocation from
    // being silently dropped by the structural expansion.
    let outer_marker_slot = instructions.get(7).and_then(stored_int_local)?;
    if !exact_parameter_null_check(
        instructions.get(0..3)?,
        &body.source_cp,
        parameter_slots[receiver_parameter],
    ) || !exact_parameter_null_check(
        instructions.get(3..6)?,
        &body.source_cp,
        *parameter_slots.get(lambda_parameter)?,
    ) || instructions.get(6).and_then(integer_constant) != Some(0)
        || parameter_slots.contains(&outer_marker_slot)
    {
        return None;
    }

    let invoke_sites = inline::function_invoke_sites(instructions, &body.source_cp);
    let [invoke] = invoke_sites.as_slice() else {
        return None;
    };
    let invoke_target = inline::invoked_method(instructions.get(*invoke)?, &body.source_cp)?;
    let (invoke_parameters, invoke_result) =
        crate::jvm::names::parse_method_descriptor(invoke_target.2)?;
    if instructions
        .get(invoke.checked_sub(2)?)
        .and_then(loaded_reference_local)
        != Some(*parameter_slots.get(lambda_parameter)?)
        || invoke_parameters.len() != 1
        || !(invoke_result.starts_with('L') || invoke_result.starts_with('['))
    {
        return None;
    }
    let traversal =
        recognize_collection_transform_traversal(instructions, &body.source_cp, *invoke)?;

    // Accept the factory only as one exact JVM allocation flow. The constructor owner, each
    // argument producer, and the destination store are therefore linked instead of being merely
    // observed somewhere in the same declaration body.
    let mut allocations = instructions
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| {
            class_name(instruction, &body.source_cp, 0xbb).map(|owner| (index, owner))
        });
    let (allocation_index, allocation_owner) = allocations.next()?;
    if allocation_index != 10
        || allocations.next().is_some()
        || !plain(instructions.get(allocation_index + 1), 0x59)
    {
        return None;
    }
    let constructors = instructions
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| {
            opcode(Some(instruction), 0xb7)
                .then(|| inline::invoked_method(instruction, &body.source_cp))
                .flatten()
                .map(|target| (index, target))
        })
        .collect::<Vec<_>>();
    let [(constructor_index, factory)] = constructors.as_slice() else {
        return None;
    };
    if factory.0 != allocation_owner || factory.1 != "<init>" || factory.3 {
        return None;
    }
    let (factory_parameters, factory_result) = super::super::parse_method_desc(factory.2)?;
    if factory_result != Ty::Unit {
        return None;
    }
    let capacity = match factory_parameters.as_slice() {
        [] if *constructor_index == allocation_index + 2 => None,
        [Ty::Int]
            if *constructor_index == allocation_index + 4
                && loaded_reference_local(instructions.get(allocation_index + 2)?)
                    == Some(parameter_slots[receiver_parameter]) =>
        {
            let capacity_index = allocation_index + 3;
            let capacity_target =
                inline::invoked_method(instructions.get(capacity_index)?, &body.source_cp)?;
            let (parameters, result) = super::super::parse_method_desc(capacity_target.2)?;
            let owner_descriptor = descriptor_for_class(capacity_target.0);
            if result != Ty::Int
                || !parameters.is_empty()
                || owner_descriptor != *receiver_descriptor
                || !matches!(
                    instructions.get(capacity_index),
                    Some(Insn::Plain {
                        op: 0xb6 | 0xb9,
                        ..
                    })
                )
            {
                return None;
            }
            Some(RecognizedCapacity::Member(capacity_target))
        }
        [Ty::Int]
            if *constructor_index == allocation_index + 5
                && loaded_reference_local(instructions.get(allocation_index + 2)?)
                    == Some(parameter_slots[receiver_parameter]) =>
        {
            let default = integer_constant(instructions.get(allocation_index + 3)?)?;
            let capacity_index = allocation_index + 4;
            let capacity_target =
                inline::invoked_method(instructions.get(capacity_index)?, &body.source_cp)?;
            let (parameters, result) = super::super::parse_method_desc(capacity_target.2)?;
            let (physical_capacity_parameters, physical_capacity_result) =
                crate::jvm::names::parse_method_descriptor(capacity_target.2)?;
            if result != Ty::Int
                || parameters.len() != 2
                || parameters[1] != Ty::Int
                || physical_capacity_parameters.as_slice() != [*receiver_descriptor, "I"]
                || physical_capacity_result != "I"
                || !opcode(instructions.get(capacity_index), 0xb8)
            {
                return None;
            }
            Some(RecognizedCapacity::Extension {
                target: capacity_target,
                default,
            })
        }
        _ => return None,
    };
    let storage_cast = class_name(
        instructions.get(constructor_index + 1)?,
        &body.source_cp,
        0xc0,
    );
    let destination_store = constructor_index + 1 + usize::from(storage_cast.is_some());
    let destination_slot = instructions
        .get(destination_store)
        .and_then(stored_reference_local)?;
    let destination_descriptor = descriptor_for_class(storage_cast.unwrap_or(allocation_owner));

    let receiver_copy = instructions
        .get(..allocation_index)?
        .windows(2)
        .enumerate()
        .filter_map(|(index, window)| {
            (loaded_reference_local(&window[0]) == Some(parameter_slots[receiver_parameter]))
                .then(|| stored_reference_local(&window[1]).map(|slot| (index, slot)))
                .flatten()
        })
        .collect::<Vec<_>>();
    let [(receiver_copy_index, inner_receiver_slot)] = receiver_copy.as_slice() else {
        return None;
    };
    let inner_marker_slot = instructions
        .get(destination_store + 2)
        .and_then(stored_int_local)?;
    if *receiver_copy_index != 8
        || loaded_reference_local(instructions.get(*receiver_copy_index)?)
            != Some(parameter_slots[receiver_parameter])
        || stored_reference_local(instructions.get(*receiver_copy_index + 1)?)
            != Some(*inner_receiver_slot)
        || instructions
            .get(destination_store + 1)
            .and_then(integer_constant)
            != Some(0)
        || parameter_slots.contains(&inner_marker_slot)
        || inner_marker_slot == *inner_receiver_slot
        || inner_marker_slot == destination_slot
    {
        return None;
    }

    // The prepare chain is a real JVM stack chain: load the declaration's copied receiver once,
    // then invoke each zero-argument member immediately on the prior result, and store only the
    // final iterator. Merely collecting same-shaped calls by descriptor/order is insufficient — it
    // could turn unrelated calls into a different chain during lowering.
    let prepare_start = destination_store + 3;
    let RecognizedTraversal::Iterator {
        prepare,
        has_next: traversal_has_next,
        next: traversal_next,
    } = &traversal
    else {
        return None;
    };
    if loaded_reference_local(instructions.get(prepare_start)?) != Some(*inner_receiver_slot) {
        return None;
    }
    let mut prepare_cursor = prepare_start + 1;
    for target in prepare {
        if !matches!(
            instructions.get(prepare_cursor),
            Some(Insn::Plain {
                op: 0xb6 | 0xb9,
                ..
            })
        ) || inline::invoked_method(instructions.get(prepare_cursor)?, &body.source_cp)
            != Some(*target)
        {
            return None;
        }
        prepare_cursor += 1;
    }
    let strict_iterator_slot = instructions
        .get(prepare_cursor)
        .and_then(stored_reference_local)?;

    // There is one element-producing call between hasNext and the lambda invocation. Its optional
    // cast and following store define the element local's exact physical descriptor.
    let has_next_calls = instructions
        .get(..*invoke)?
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| {
            let target = inline::invoked_method(instruction, &body.source_cp)?;
            (crate::jvm::names::parse_method_descriptor(target.2)?.1 == "Z")
                .then_some((index, target))
        })
        .collect::<Vec<_>>();
    let [(has_next, _)] = has_next_calls.as_slice() else {
        return None;
    };
    let (next_index, next_target) =
        only_invocation(instructions, &body.source_cp, has_next + 2..*invoke)?;
    let (_, next_result) = crate::jvm::names::parse_method_descriptor(next_target.2)?;
    let element_cast = class_name(instructions.get(next_index + 1)?, &body.source_cp, 0xc0);
    let element_store = next_index + 1 + usize::from(element_cast.is_some());
    let element_slot = instructions
        .get(element_store)
        .and_then(stored_reference_local)?;
    let element_descriptor = element_cast
        .map(descriptor_for_class)
        .unwrap_or_else(|| next_result.to_owned());

    let calls_after_invoke = instructions
        .get(invoke + 1..)?
        .iter()
        .enumerate()
        .filter_map(|(offset, instruction)| {
            inline::invoked_method(instruction, &body.source_cp)
                .map(|target| (invoke + 1 + offset, target))
        })
        .collect::<Vec<_>>();
    let [(append_index, append_target)] = calls_after_invoke.as_slice() else {
        return None;
    };
    let (append_parameters, append_result) = super::super::parse_method_desc(append_target.2)?;
    if append_result != Ty::Boolean || !plain(instructions.get(append_index + 1), 0x57) {
        return None;
    }
    let (append, element_live_start) = if append_parameters.len() == 1
        && matches!(
            instructions.get(*append_index),
            Some(Insn::Plain {
                op: 0xb6 | 0xb9,
                ..
            })
        )
        && *append_index == invoke + 1
        && instructions
            .get(invoke.checked_sub(3)?)
            .and_then(loaded_reference_local)
            == Some(destination_slot)
        && descriptor_for_class(append_target.0) == destination_descriptor
    {
        (
            RecognizedAppend::Member(*append_target),
            invoke.checked_sub(3)?,
        )
    } else if append_parameters.len() == 2
        && opcode(instructions.get(*append_index), 0xb8)
        && opcode(instructions.get(invoke + 1), 0xc0)
        && instructions
            .get(invoke + 2)
            .and_then(stored_reference_local)
            .is_some()
        && *append_index == invoke + 5
    {
        let part_class = class_name(instructions.get(invoke + 1)?, &body.source_cp, 0xc0)?;
        let part_descriptor = descriptor_for_class(part_class);
        let (physical_append_parameters, physical_append_result) =
            crate::jvm::names::parse_method_descriptor(append_target.2)?;
        if physical_append_parameters.as_slice()
            != [destination_descriptor.as_str(), part_descriptor.as_str()]
            || physical_append_result != "Z"
        {
            return None;
        }
        let part_slot = instructions
            .get(invoke + 2)
            .and_then(stored_reference_local)?;
        if instructions
            .get(invoke + 3)
            .and_then(loaded_reference_local)
            != Some(destination_slot)
            || instructions
                .get(invoke + 4)
                .and_then(loaded_reference_local)
                != Some(part_slot)
        {
            return None;
        }
        (
            RecognizedAppend::Extension(*append_target),
            invoke.checked_sub(2)?,
        )
    } else {
        return None;
    };

    let loop_head = has_next.checked_sub(1)?;
    let loop_back = append_index + 2;
    if prepare_cursor + 1 != loop_head
        || loaded_reference_local(instructions.get(loop_head)?) != Some(strict_iterator_slot)
        || inline::invoked_method(instructions.get(*has_next)?, &body.source_cp)
            != Some(*traversal_has_next)
        || next_index != *has_next + 3
        || loaded_reference_local(instructions.get(next_index.checked_sub(1)?)?)
            != Some(strict_iterator_slot)
        || inline::invoked_method(instructions.get(next_index)?, &body.source_cp)
            != Some(*traversal_next)
        || !matches!(
            instructions.get(loop_back),
            Some(Insn::Branch {
                op: 0xa7,
                target: BranchTarget::Internal(target),
            }) if *target == loop_head
        )
    {
        return None;
    }
    let exit = match instructions.get(has_next + 1)? {
        Insn::Branch {
            op: 0x99,
            target: BranchTarget::Internal(target),
        } => *target,
        _ => return None,
    };
    let exact_append_prefix = match append {
        RecognizedAppend::Member(_) => element_store + 1 == invoke.checked_sub(3)?,
        RecognizedAppend::Extension(_) => element_store + 1 == invoke.checked_sub(2)?,
    };
    if !exact_append_prefix || exit != loop_back + 1 {
        return None;
    }
    let result_class = class_name(instructions.get(exit + 1)?, &body.source_cp, 0xc0);
    let result_cast = usize::from(result_class.is_some());
    if instructions.get(exit).and_then(loaded_reference_local) != Some(destination_slot)
        || result_class
            .map(descriptor_for_class)
            .unwrap_or_else(|| destination_descriptor.clone())
            != physical_result
        || !plain(instructions.get(exit + 1 + result_cast), 0xb0)
        || exit + 2 + result_cast != instructions.len()
    {
        return None;
    }

    let offsets = inline::insn_offsets_at(instructions, 0);
    let offset = |index: usize| offsets.get(index).copied();
    let outer = covered_local(
        body,
        parameter_slots[receiver_parameter],
        receiver_descriptor,
        0,
        body.code.len(),
    )?;
    let inner = covered_local(
        body,
        *inner_receiver_slot,
        receiver_descriptor,
        offset(loop_head)?,
        offset(exit + 1)?,
    )?;
    let destination = covered_local(
        body,
        destination_slot,
        &destination_descriptor,
        offset(element_live_start)?,
        offset(exit + 1)?,
    )?;
    let element = covered_local(
        body,
        element_slot,
        &element_descriptor,
        offset(element_live_start)?,
        offset(loop_back)?,
    )?;
    if offset(*receiver_copy_index)? < usize::from(outer.start_pc) {
        return None;
    }
    let local_names = InlineCollectionLocalNames {
        outer_receiver: stripped_local_name(outer, true, 0)?,
        inner_receiver: stripped_local_name(inner, true, 1)?,
        destination: stripped_local_name(destination, false, 1)?,
        element: stripped_local_name(element, false, 1)?,
    };

    Some(RecognizedCollectionTransform {
        lambda_parameter,
        traversal,
        factory: *factory,
        capacity,
        append,
        local_names,
    })
}

impl JvmLibraries {
    pub(super) fn inline_collection_transform_body_plan(
        &self,
        callable: &LibraryCallable,
        body_descriptor: &str,
        parameter_slots: &[u16],
    ) -> CollectionTransformDecode {
        // Reject the overwhelmingly common non-transform inline declaration before touching its
        // classfile body. The structural recognizer repeats this guard so direct tests cannot call
        // it with an ineligible view.
        if collection_transform_lambda_parameter(callable, parameter_slots).is_none() {
            return CollectionTransformDecode::NotRecognized;
        }
        let owner = callable.owner.render();
        let Some(body) = self.cp.method_code(&owner, &callable.name, body_descriptor) else {
            return CollectionTransformDecode::Unavailable;
        };
        let Some(instructions) = inline::disassemble(&body.code) else {
            return CollectionTransformDecode::Rejected;
        };
        let Some(recognized) = recognize(
            callable,
            body_descriptor,
            parameter_slots,
            &body,
            &instructions,
        ) else {
            return CollectionTransformDecode::NotRecognized;
        };
        let Some(traversal) = self.normalize_iteration_traversal(recognized.traversal) else {
            // Member normalization can observe an unavailable provider dependency. A negative plan
            // for that transient state must not escape into the process-global declaration cache.
            return CollectionTransformDecode::Unavailable;
        };
        let Some((_, factory)) = self.inline_plan_constructor(recognized.factory) else {
            return CollectionTransformDecode::Unavailable;
        };
        let capacity = match recognized.capacity {
            None => None,
            Some(RecognizedCapacity::Member(target)) => {
                let Some(member) = self.inline_plan_member(target) else {
                    return CollectionTransformDecode::Unavailable;
                };
                Some(InlineCollectionCapacity::Member(Box::new(member)))
            }
            Some(RecognizedCapacity::Extension { target, default }) => {
                let callable = match self.inline_plan_static_extension(target) {
                    InlineDependency::Found(callable) => callable,
                    InlineDependency::Rejected => return CollectionTransformDecode::Rejected,
                    InlineDependency::Unavailable => return CollectionTransformDecode::Unavailable,
                };
                Some(InlineCollectionCapacity::Extension {
                    callable: Box::new(callable),
                    default,
                })
            }
        };
        let append = match recognized.append {
            RecognizedAppend::Member(target) => {
                let Some(member) = self.inline_plan_member(target) else {
                    return CollectionTransformDecode::Unavailable;
                };
                InlineCollectionAppend::Member(Box::new(member))
            }
            RecognizedAppend::Extension(target) => {
                let callable = match self.inline_plan_static_extension(target) {
                    InlineDependency::Found(callable) => callable,
                    InlineDependency::Rejected => return CollectionTransformDecode::Rejected,
                    InlineDependency::Unavailable => return CollectionTransformDecode::Unavailable,
                };
                InlineCollectionAppend::Extension(Box::new(callable))
            }
        };
        CollectionTransformDecode::Plan(InlineBodyPlan::CollectionTransform {
            lambda_parameter: recognized.lambda_parameter,
            traversal,
            local_names: recognized.local_names,
            factory: Box::new(factory),
            capacity,
            append,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbol_source::SymbolNamespace;
    use crate::types::type_name;

    fn callable(libraries: &JvmLibraries, descriptor: &str) -> LibraryCallable {
        libraries
            .symbols(
                SymbolNamespace::Package(type_name("kotlin/collections")),
                "map",
            )
            .callables
            .functions()
            .iter()
            .find(|function| function.callable.descriptor == descriptor)
            .expect("stdlib map declaration")
            .callable
            .clone()
    }

    fn raw_map_with_descriptor(
        expected_descriptor: &str,
    ) -> (LibraryCallable, String, MethodCode, Vec<Insn>) {
        let stdlib = crate::toolchain::stdlib_jar()
            .expect("collection-transform decoder test requires the repository Kotlin stdlib");
        let libraries = JvmLibraries::new(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            vec![stdlib],
        )))
        .expect("JVM provider initialization");
        let callable = callable(&libraries, expected_descriptor);
        let descriptor = callable.descriptor.clone();
        let body = libraries
            .cp
            .method_code(&callable.owner.render(), &callable.name, &descriptor)
            .expect("stdlib Iterable.map body");
        let instructions = inline::disassemble(&body.code).expect("valid Iterable.map bytecode");
        (callable, descriptor, body, instructions)
    }

    fn raw_map() -> (LibraryCallable, String, MethodCode, Vec<Insn>) {
        raw_map_with_descriptor(
            "(Ljava/lang/Iterable;Lkotlin/jvm/functions/Function1;)Ljava/util/List;",
        )
    }

    fn shift_internal_targets(instructions: &mut [Insn], from: usize, amount: usize) {
        for instruction in instructions {
            match instruction {
                Insn::Branch {
                    target: BranchTarget::Internal(target),
                    ..
                }
                | Insn::BranchW {
                    target: BranchTarget::Internal(target),
                    ..
                } if *target >= from => *target += amount,
                Insn::TableSwitch {
                    default, targets, ..
                } => {
                    if *default >= from {
                        *default += amount;
                    }
                    for target in targets {
                        if *target >= from {
                            *target += amount;
                        }
                    }
                }
                Insn::LookupSwitch { default, pairs } => {
                    if *default >= from {
                        *default += amount;
                    }
                    for (_, target) in pairs {
                        if *target >= from {
                            *target += amount;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn accepts(
        callable: &LibraryCallable,
        descriptor: &str,
        body: &MethodCode,
        instructions: &[Insn],
    ) -> bool {
        recognize(callable, descriptor, &[0, 1], body, instructions).is_some()
    }

    #[test]
    fn collection_transform_decoder_requires_exact_factory_dataflow() {
        let (callable, descriptor, body, instructions) = raw_map();
        let recognized = recognize(&callable, &descriptor, &[0, 1], &body, &instructions)
            .expect("baseline collection-transform body");
        assert_eq!(recognized.lambda_parameter, 1);

        let allocation = instructions
            .iter()
            .position(|instruction| class_name(instruction, &body.source_cp, 0xbb).is_some())
            .expect("map allocation");
        let mut without_dup = instructions.clone();
        without_dup[allocation + 1] = Insn::Plain {
            op: 0x00,
            operands: Vec::new(),
        };
        assert!(!accepts(&callable, &descriptor, &body, &without_dup));

        let replacement_class = instructions
            .iter()
            .find_map(|instruction| match instruction {
                Insn::Plain { op: 0xc0, operands } => Some(operands.clone()),
                _ => None,
            })
            .expect("map destination cast");
        let mut wrong_allocation_owner = instructions.clone();
        let Insn::Plain { operands, .. } = &mut wrong_allocation_owner[allocation] else {
            unreachable!("allocation was identified as a plain new instruction")
        };
        *operands = replacement_class;
        assert!(!accepts(
            &callable,
            &descriptor,
            &body,
            &wrong_allocation_owner
        ));

        let mut wrong_capacity_receiver = instructions.clone();
        wrong_capacity_receiver[allocation + 2] = Insn::Plain {
            op: 0x2b,
            operands: Vec::new(),
        };
        assert!(!accepts(
            &callable,
            &descriptor,
            &body,
            &wrong_capacity_receiver
        ));

        let constructor = instructions
            .iter()
            .position(|instruction| opcode(Some(instruction), 0xb7))
            .expect("map constructor");
        let mut detached_store = instructions.clone();
        detached_store[constructor + 2] = Insn::Plain {
            op: 0x4e,
            operands: Vec::new(),
        };
        assert!(!accepts(&callable, &descriptor, &body, &detached_store));
    }

    #[test]
    fn collection_transform_decoder_discovers_the_lambda_parameter_and_physical_slot() {
        let (callable, descriptor, mut body, instructions) = raw_map();
        let invoke = inline::function_invoke_sites(&instructions, &body.source_cp)[0];
        let mut moved_lambda_slot = instructions.clone();
        moved_lambda_slot[3] = Insn::Plain {
            op: 0x19,
            operands: vec![9],
        };
        moved_lambda_slot[invoke - 2] = Insn::Plain {
            op: 0x19,
            operands: vec![9],
        };
        // The indexed loads are one byte wider than the fixture's compact `aload_1`; broaden the
        // unchanged synthetic LVT so this test isolates parameter identity instead of failing on
        // byte-offset drift introduced by the mutation.
        for local in &mut body.locals {
            local.start_pc = 0;
            local.length = u16::MAX;
        }
        let moved = recognize(&callable, &descriptor, &[0, 9], &body, &moved_lambda_slot)
            .expect("the decoded parameter identity follows its physical slot");
        assert_eq!(moved.lambda_parameter, 1);
        assert!(recognize(&callable, &descriptor, &[0, 1], &body, &moved_lambda_slot,).is_none());

        let mut function_in_receiver_slot = callable.clone();
        function_in_receiver_slot.params.swap(0, 1);
        assert!(recognize(
            &function_in_receiver_slot,
            &descriptor,
            &[0, 1],
            &body,
            &instructions,
        )
        .is_none());
    }

    #[test]
    fn collection_transform_decoder_rejects_an_unmodeled_side_effect_invocation() {
        let (callable, descriptor, body, instructions) = raw_map();
        assert!(accepts(&callable, &descriptor, &body, &instructions));

        // Inject a third, valid `checkNotNullParameter(receiver, <this>)` call. The old decoder
        // filtered void/argument-taking calls out of its call inventory and silently omitted this
        // throwing side effect from the published expansion.
        let insertion = 6;
        let mut with_side_effect = instructions.clone();
        with_side_effect.splice(insertion..insertion, instructions[0..3].iter().cloned());
        shift_internal_targets(&mut with_side_effect, insertion, 3);
        assert!(!accepts(&callable, &descriptor, &body, &with_side_effect));
    }

    #[test]
    fn collection_transform_decoder_requires_each_prepare_result_to_feed_the_next_receiver() {
        let (callable, descriptor, mut body, instructions) = raw_map_with_descriptor(
            "(Ljava/util/Map;Lkotlin/jvm/functions/Function1;)Ljava/util/List;",
        );
        let recognized = recognize(&callable, &descriptor, &[0, 1], &body, &instructions)
            .expect("baseline Map transform body");
        let RecognizedTraversal::Iterator { prepare, .. } = recognized.traversal else {
            panic!("Map transform must use an iterator chain")
        };
        let [first, second] = prepare.as_slice() else {
            panic!("Map transform must expose a two-step prepare chain")
        };
        let first_index = instructions
            .iter()
            .position(|instruction| {
                inline::invoked_method(instruction, &body.source_cp) == Some(*first)
            })
            .expect("first Map traversal step");
        assert_eq!(
            inline::invoked_method(&instructions[first_index + 1], &body.source_cp),
            Some(*second),
            "the baseline declaration chains the second call directly on the first result"
        );

        // Put the original receiver back on the stack between the two calls. Both target identities
        // and their order remain present, but the second call no longer consumes the first result.
        let insertion = first_index + 1;
        let mut broken_chain = instructions.clone();
        broken_chain.insert(insertion, instructions[8].clone());
        shift_internal_targets(&mut broken_chain, insertion, 1);
        // Synthetic insertion changes instruction offsets; broaden debug-local ranges so rejection
        // is attributable to the broken stack chain rather than incidental LVT coverage.
        for local in &mut body.locals {
            local.start_pc = 0;
            local.length = u16::MAX;
        }
        assert!(!accepts(&callable, &descriptor, &body, &broken_chain));
    }

    #[test]
    fn collection_transform_decoder_requires_append_loop_and_lvt_provenance() {
        let (callable, descriptor, body, instructions) = raw_map();
        let recognized = recognize(&callable, &descriptor, &[0, 1], &body, &instructions)
            .expect("baseline collection-transform body");

        let invoke = inline::function_invoke_sites(&instructions, &body.source_cp)[0];
        let mut without_append = instructions.clone();
        without_append[invoke + 1] = Insn::Plain {
            op: 0x00,
            operands: Vec::new(),
        };
        assert!(!accepts(&callable, &descriptor, &body, &without_append));

        let mut wrong_loop = instructions.clone();
        wrong_loop[invoke + 3] = Insn::Branch {
            op: 0xa7,
            target: BranchTarget::Internal(0),
        };
        assert!(!accepts(&callable, &descriptor, &body, &wrong_loop));

        let constructor = instructions
            .iter()
            .position(|instruction| opcode(Some(instruction), 0xb7))
            .expect("decoded factory constructor");
        let destination_slot = instructions
            .get(constructor + 1..constructor + 3)
            .and_then(|tail| tail.iter().find_map(stored_reference_local))
            .expect("factory result destination slot");
        let mut wrong_locals = body.clone();
        let destination = wrong_locals
            .locals
            .iter_mut()
            .find(|local| {
                local.slot == destination_slot
                    && stripped_local_name(local, false, 1).as_deref()
                        == Some(recognized.local_names.destination.as_ref())
            })
            .expect("decoded destination local");
        destination.descriptor = "Ljava/lang/Object;".to_owned();
        assert!(!accepts(
            &callable,
            &descriptor,
            &wrong_locals,
            &instructions
        ));

        let element_slot = instructions
            .get(invoke - 1)
            .and_then(loaded_reference_local)
            .expect("lambda element slot");
        let mut truncated_element = body.clone();
        let element = truncated_element
            .locals
            .iter_mut()
            .find(|local| {
                local.slot == element_slot
                    && stripped_local_name(local, false, 1).as_deref()
                        == Some(recognized.local_names.element.as_ref())
            })
            .expect("decoded element local");
        element.length = 1;
        assert!(!accepts(
            &callable,
            &descriptor,
            &truncated_element,
            &instructions
        ));

        let mut unnamed_destination = body.clone();
        let destination = unnamed_destination
            .locals
            .iter_mut()
            .find(|local| {
                local.slot == destination_slot
                    && stripped_local_name(local, false, 1).as_deref()
                        == Some(recognized.local_names.destination.as_ref())
            })
            .expect("decoded destination local");
        destination.name = recognized.local_names.destination.to_string();
        assert!(!accepts(
            &callable,
            &descriptor,
            &unnamed_destination,
            &instructions
        ));
    }
}
