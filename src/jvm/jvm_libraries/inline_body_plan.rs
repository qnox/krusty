//! Provider-side decoding of declaration-scoped inline control-flow plans.
//!
//! Bytecode is used only to recognize the physical control-flow and exact call targets. Semantic
//! member signatures come from the Kotlin classifier model built from metadata.

use super::{JvmLibraries, CONTINUATION_PARAM_DESCRIPTOR};
use crate::jvm::classreader::{ExcEntry, C};
use crate::jvm::inline::{self, Insn};
use crate::libraries::{InlineBodyPlan, InlineBodyState, LibraryCallable, LibraryMember};
use crate::types::{type_name, Ty};

type MethodTarget<'a> = (&'a str, &'a str, &'a str, bool);

fn inline_body_descriptor(callable: &LibraryCallable) -> Option<String> {
    if !callable.suspend {
        return Some(callable.descriptor.clone());
    }
    let close = callable.descriptor.rfind(')')?;
    Some(format!(
        "({}{}){}",
        &callable.descriptor[1..close],
        CONTINUATION_PARAM_DESCRIPTOR,
        &callable.descriptor[close + 1..]
    ))
}

fn callable_parameter_slots(parameters: &[Ty]) -> Vec<u16> {
    let mut next = 0u16;
    parameters
        .iter()
        .map(|parameter| {
            let slot = next;
            next += u16::from(matches!(*parameter, Ty::Long | Ty::Double)) + 1;
            slot
        })
        .collect()
}

fn slot_after_parameters(parameters: &[Ty]) -> Option<u16> {
    parameters.iter().try_fold(0u16, |slot, parameter| {
        slot.checked_add(u16::from(matches!(*parameter, Ty::Long | Ty::Double)) + 1)
    })
}

fn physical_descriptor(member: &LibraryMember) -> String {
    crate::jvm::names::method_descriptor(&member.physical_params, member.physical_ret)
}

fn loaded_reference_local(instruction: &Insn) -> Option<u16> {
    let Insn::Plain { op, operands } = instruction else {
        return None;
    };
    match *op {
        0x2a..=0x2d => Some(u16::from(*op - 0x2a)),
        0x19 => operands.first().copied().map(u16::from),
        0xc4 if operands.first() == Some(&0x19) => operands
            .get(1)
            .zip(operands.get(2))
            .map(|(&high, &low)| (u16::from(high) << 8) | u16::from(low)),
        _ => None,
    }
}

fn loaded_int_local(instruction: &Insn) -> Option<u16> {
    let Insn::Plain { op, operands } = instruction else {
        return None;
    };
    match *op {
        0x1a..=0x1d => Some(u16::from(*op - 0x1a)),
        0x15 => operands.first().copied().map(u16::from),
        0xc4 if operands.first() == Some(&0x15) => operands
            .get(1)
            .zip(operands.get(2))
            .map(|(&high, &low)| (u16::from(high) << 8) | u16::from(low)),
        _ => None,
    }
}

fn stored_reference_local(instruction: &Insn) -> Option<u16> {
    let Insn::Plain { op, operands } = instruction else {
        return None;
    };
    match *op {
        0x4b..=0x4e => Some(u16::from(*op - 0x4b)),
        0x3a => operands.first().copied().map(u16::from),
        0xc4 if operands.first() == Some(&0x3a) => operands
            .get(1)
            .zip(operands.get(2))
            .map(|(&high, &low)| (u16::from(high) << 8) | u16::from(low)),
        _ => None,
    }
}

fn invocation_operands(
    instructions: &[Insn],
    source_cp: &[C],
    index: usize,
    descriptor: &str,
) -> Option<(Vec<u16>, usize)> {
    let parameter_count = crate::jvm::names::parse_method_descriptor(descriptor)?
        .0
        .len();
    let receiver_count = match instructions.get(index)? {
        Insn::Plain { op: 0xb8, .. } => 0,
        Insn::Plain {
            op: 0xb6 | 0xb7 | 0xb9,
            ..
        } => 1,
        _ => return None,
    };
    let mut cursor = index;
    let operand_count = parameter_count.checked_add(receiver_count)?;
    let mut operands = Vec::with_capacity(operand_count);
    while operands.len() < operand_count {
        cursor = cursor.checked_sub(1)?;
        if let Some(local) = loaded_reference_local(&instructions[cursor]) {
            operands.push(local);
            continue;
        }
        // kotlinc may put `InlineMarker.mark(const)` between an enter call's already-loaded
        // operands and the invocation. It is stack-neutral as a pair; no other call or instruction
        // may be crossed while attributing operand producers to this call.
        if inline::invoked_method(&instructions[cursor], source_cp).is_some_and(
            |(owner, name, descriptor, _)| {
                owner == "kotlin/jvm/internal/InlineMarker"
                    && name == "mark"
                    && descriptor == "(I)V"
            },
        ) {
            cursor = cursor.checked_sub(1)?;
            if !matches!(
                instructions[cursor],
                Insn::Plain {
                    op: 0x02..=0x08,
                    ..
                }
            ) {
                return None;
            }
            continue;
        }
        return None;
    }
    Some((operands, cursor))
}

fn exact_call_partition(call_indices: &[usize], lambda: usize) -> bool {
    call_indices.len() == 3
        && call_indices[0] < lambda
        && call_indices[1] > lambda
        && call_indices[2] > lambda
}

fn exact_parameter_roles(
    lambda_parameter: usize,
    semantic_parameters: usize,
    physical_parameters: usize,
    parameter_slots: usize,
    state_parameters: usize,
) -> bool {
    state_parameters.checked_add(1).is_some_and(|lambda_role| {
        lambda_parameter == lambda_role
            && lambda_role.checked_add(1).is_some_and(|role_count| {
                semantic_parameters == role_count
                    && physical_parameters == role_count
                    && parameter_slots == role_count
            })
    })
}

fn exact_null_default_prefix(instructions: &[Insn], mask_slot: u16, parameter_slot: u16) -> bool {
    let [mask, one, and, branch, null, store, ..] = instructions else {
        return false;
    };
    loaded_int_local(mask) == Some(mask_slot)
        && matches!(one, Insn::Plain { op: 0x04, operands } if operands.is_empty())
        && matches!(and, Insn::Plain { op: 0x7e, operands } if operands.is_empty())
        && matches!(
            branch,
            Insn::Branch {
                op: 0x99,
                target: inline::BranchTarget::Internal(6),
            }
        )
        && matches!(null, Insn::Plain { op: 0x01, operands } if operands.is_empty())
        && stored_reference_local(store) == Some(parameter_slot)
}

fn is_structural_marker(target: MethodTarget<'_>) -> bool {
    target.0 == "kotlin/jvm/internal/InlineMarker"
        && target.2 == "(I)V"
        && matches!(target.1, "mark" | "finallyStart" | "finallyEnd")
}

fn exact_parameter_null_check(instructions: &[Insn], source_cp: &[C], parameter_slot: u16) -> bool {
    let [load, Insn::Plain {
        op: 0x12 | 0x13, ..
    }, invoke] = instructions
    else {
        return false;
    };
    loaded_reference_local(load) == Some(parameter_slot)
        && inline::invoked_method(invoke, source_cp).is_some_and(|(owner, name, descriptor, _)| {
            owner == "kotlin/jvm/internal/Intrinsics"
                && name == "checkNotNullParameter"
                && descriptor == "(Ljava/lang/Object;Ljava/lang/String;)V"
        })
}

fn exact_lambda_return_parameter(
    instructions: &[Insn],
    parameter_at: &impl Fn(u16) -> Option<usize>,
) -> Option<Option<usize>> {
    match instructions {
        [Insn::Plain { op: 0xb0, .. }] => Some(None),
        [Insn::Plain { op: 0x57, .. }, load, Insn::Plain { op: 0xb0, .. }] => {
            Some(Some(parameter_at(loaded_reference_local(load)?)?))
        }
        _ => None,
    }
}

fn handler_free_lambda_body(handlers: &[ExcEntry]) -> bool {
    handlers.is_empty()
}

fn instruction_index(offsets: &[usize], byte_offset: u16) -> Option<usize> {
    offsets.binary_search(&usize::from(byte_offset)).ok()
}

fn is_inline_marker_pair(instructions: &[Insn], source_cp: &[C], name: &str) -> bool {
    let [Insn::Plain {
        op: 0x02..=0x08, ..
    }, marker] = instructions
    else {
        return false;
    };
    inline::invoked_method(marker, source_cp).is_some_and(|(owner, actual_name, descriptor, _)| {
        owner == "kotlin/jvm/internal/InlineMarker" && actual_name == name && descriptor == "(I)V"
    })
}

fn enter_result_starts_body(instructions: &[Insn], source_cp: &[C]) -> bool {
    let [Insn::Plain {
        op: 0x02..=0x08, ..
    }, marker, Insn::Plain { op: 0x57, .. }] = instructions
    else {
        return false;
    };
    inline::invoked_method(marker, source_cp).is_some_and(|(owner, name, descriptor, _)| {
        owner == "kotlin/jvm/internal/InlineMarker" && name == "mark" && descriptor == "(I)V"
    })
}

fn exact_inline_local_prefix(instructions: &[Insn], scratch_slot: u16) -> bool {
    let [Insn::Plain { op: 0x03, .. }, store] = instructions else {
        return false;
    };
    inline::stored_local(store) == Some(scratch_slot)
}

fn exact_loaded_operands(instructions: &[Insn], operands: &[u16]) -> bool {
    instructions.len() == operands.len()
        && instructions
            .iter()
            .zip(operands.iter().rev())
            .all(|(instruction, operand)| loaded_reference_local(instruction) == Some(*operand))
}

fn exact_enter_operands(instructions: &[Insn], source_cp: &[C], operands: &[u16]) -> bool {
    let Some(loads) = instructions.get(..operands.len()) else {
        return false;
    };
    exact_loaded_operands(loads, operands)
        && instructions
            .get(operands.len()..)
            .is_some_and(|marker| is_inline_marker_pair(marker, source_cp, "mark"))
}

fn exact_control_flow(instructions: &[Insn], normal_branch: usize, normal_return: usize) -> bool {
    normal_return < instructions.len()
        && matches!(
        instructions.get(normal_branch),
        Some(
            Insn::Branch {
                op: 0xa7,
                target: inline::BranchTarget::Internal(target),
            } | Insn::BranchW {
                op: 0xc8,
                target: inline::BranchTarget::Internal(target),
            }
        ) if *target == normal_return
        )
        && instructions
            .iter()
            .enumerate()
            .all(|(index, instruction)| match instruction {
                Insn::Branch {
                    op: 0xa7,
                    target: inline::BranchTarget::Internal(target),
                }
                | Insn::BranchW {
                    op: 0xc8,
                    target: inline::BranchTarget::Internal(target),
                } => index == normal_branch && *target == normal_return,
                Insn::Branch { .. }
                | Insn::BranchW { .. }
                | Insn::TableSwitch { .. }
                | Insn::LookupSwitch { .. } => false,
                Insn::Plain { .. } => true,
            })
}

fn has_only_template_effects(instructions: &[Insn]) -> bool {
    instructions.iter().all(|instruction| match instruction {
        Insn::Plain { op, .. } => !matches!(
            *op,
            0x4f..=0x56 // array stores
                | 0x84 // iinc
                | 0xb2..=0xb5 // field reads/writes
                | 0xba // invokedynamic
                | 0xbb | 0xbc | 0xbd // allocation
                | 0xc2 | 0xc3 // monitor enter/exit
                | 0xc5 // multidimensional allocation
        ),
        Insn::Branch { .. } | Insn::BranchW { .. } => true,
        Insn::TableSwitch { .. } | Insn::LookupSwitch { .. } => false,
    })
}

fn cleanup_boundaries_are_immediate(
    invoke: usize,
    normal_cleanup_start: usize,
    handler: usize,
    exceptional_cleanup_start: usize,
) -> bool {
    invoke.checked_add(2) == Some(normal_cleanup_start)
        && handler.checked_add(1) == Some(exceptional_cleanup_start)
}

fn normal_result_return(
    instructions: &[Insn],
    source_cp: &[C],
    invoke: usize,
    normal_cleanup: usize,
    handler: usize,
) -> Option<(usize, usize)> {
    let result_slot = instructions
        .get(invoke + 1)
        .and_then(stored_reference_local)?;
    let branch = instructions
        .get(normal_cleanup + 1..handler)
        .into_iter()
        .flatten()
        .enumerate()
        .find_map(|(offset, instruction)| match instruction {
            Insn::Branch {
                op: 0xa7,
                target: inline::BranchTarget::Internal(target),
            }
            | Insn::BranchW {
                op: 0xc8,
                target: inline::BranchTarget::Internal(target),
            } => Some((normal_cleanup + 1 + offset, *target)),
            _ => None,
        });
    let (branch, target) = branch?;
    (is_inline_marker_pair(
        &instructions[normal_cleanup + 1..branch],
        source_cp,
        "finallyEnd",
    ) && branch + 1 == handler
        && instructions.get(target).and_then(loaded_reference_local) == Some(result_slot)
        && matches!(
            instructions.get(target + 1),
            Some(Insn::Plain { op: 0xb0, .. })
        )
        && target + 2 == instructions.len())
    .then_some((branch, target))
}

fn exceptional_cleanup_rethrows(
    instructions: &[Insn],
    source_cp: &[C],
    handler: usize,
    repeated_cleanup: usize,
    normal_return: usize,
) -> bool {
    let Some(exception_slot) = instructions.get(handler).and_then(stored_reference_local) else {
        return false;
    };
    let load = instructions
        .get(repeated_cleanup + 1..)
        .into_iter()
        .flatten()
        .enumerate()
        .find_map(|(offset, instruction)| {
            (loaded_reference_local(instruction) == Some(exception_slot))
                .then_some(repeated_cleanup + 1 + offset)
        })
        .filter(|load| {
            is_inline_marker_pair(
                &instructions[repeated_cleanup + 1..*load],
                source_cp,
                "finallyEnd",
            )
        });
    load.is_some_and(|load| {
        matches!(
            instructions.get(load + 1),
            Some(Insn::Plain { op: 0xbf, .. })
        ) && load + 2 == normal_return
    })
}

#[derive(Clone, Copy)]
struct FinallyContract<'a> {
    enter_pc: usize,
    lambda_pc: usize,
    normal_cleanup_pc: usize,
    repeated_cleanup_pc: usize,
    enter_operands: &'a [u16],
    normal_cleanup_operands: &'a [u16],
    repeated_cleanup_operands: &'a [u16],
    expected_enter_operands: &'a [u16],
    expected_cleanup_operands: &'a [u16],
    handlers: &'a [ExcEntry],
    enter_result_flows_to_lambda: bool,
    normal_cleanup_starts_finally: bool,
    exceptional_cleanup_starts_finally: bool,
    normal_result_flows_to_return: bool,
    exceptional_cleanup_rethrows: bool,
    exact_parameter_roles: bool,
    exact_instruction_template: bool,
}

/// Accept only the complete `enter; try { lambda } finally { cleanup }` bytecode contract. Merely
/// seeing the same two method references is insufficient: publishing a partial plan would replace
/// declaration behavior with different control flow in every caller.
fn valid_finally_contract(contract: FinallyContract<'_>) -> bool {
    let [body, handler] = contract.handlers else {
        return false;
    };
    body.catch_type == 0
        && handler.catch_type == 0
        && contract.enter_pc < usize::from(body.start_pc)
        && usize::from(body.start_pc) <= contract.lambda_pc
        && contract.lambda_pc < usize::from(body.end_pc)
        && usize::from(body.end_pc) <= contract.normal_cleanup_pc
        && contract.normal_cleanup_pc < usize::from(body.handler_pc)
        && usize::from(body.handler_pc) <= contract.repeated_cleanup_pc
        && handler.start_pc == body.handler_pc
        && handler.handler_pc == body.handler_pc
        && usize::from(handler.end_pc) <= contract.repeated_cleanup_pc
        && contract.enter_operands == contract.expected_enter_operands
        && contract.normal_cleanup_operands == contract.expected_cleanup_operands
        && contract.repeated_cleanup_operands == contract.expected_cleanup_operands
        && contract.enter_result_flows_to_lambda
        && contract.normal_cleanup_starts_finally
        && contract.exceptional_cleanup_starts_finally
        && contract.normal_result_flows_to_return
        && contract.exceptional_cleanup_rethrows
        && contract.exact_parameter_roles
        && contract.exact_instruction_template
}

impl JvmLibraries {
    pub(super) fn inline_body_plan(&self, callable: &LibraryCallable) -> Option<InlineBodyPlan> {
        if !callable.inline.can_inline() {
            return None;
        }
        let body_descriptor = inline_body_descriptor(callable)?;
        // Every candidate overload the provider builds computes a plan, so the decode below is
        // memoized per declaration. The key carries every physical input read by the decoder.
        let parameter_slots = callable_parameter_slots(&callable.physical_params);
        let default_target = callable.default_realization.as_deref().map(|realization| {
            (
                realization.declaration_owner,
                realization.name.as_str(),
                realization.descriptor.as_str(),
            )
        });
        if let Some(plan) = self.cp.cached_inline_plan(
            callable.owner,
            &callable.name,
            &body_descriptor,
            &parameter_slots,
            callable.context_count,
            callable.source_receiver,
            &callable.params,
            default_target,
        ) {
            return plan.map(|boxed| *boxed);
        }
        let mut decode_unavailable = false;
        let plan = self.inline_body_plan_uncached(
            callable,
            &body_descriptor,
            &parameter_slots,
            &mut decode_unavailable,
        );
        // Failed body or required metadata/member reads are transient. Do not globally cache one as
        // the stable declaration fact "this inline body has no recognized plan".
        if !decode_unavailable {
            self.cp.memoize_inline_plan(
                callable.owner,
                &callable.name,
                &body_descriptor,
                &parameter_slots,
                callable.context_count,
                callable.source_receiver,
                &callable.params,
                default_target,
                plan.clone().map(Box::new),
            );
        }
        plan
    }

    fn inline_body_plan_uncached(
        &self,
        callable: &LibraryCallable,
        body_descriptor: &str,
        parameter_slots: &[u16],
        decode_unavailable: &mut bool,
    ) -> Option<InlineBodyPlan> {
        let owner = callable.owner.render();
        let inline_name = format!("{}$$forInline", callable.name);
        let Some(body) = self
            .cp
            .method_code(&owner, &inline_name, body_descriptor)
            .or_else(|| self.cp.method_code(&owner, &callable.name, body_descriptor))
        else {
            *decode_unavailable = true;
            return None;
        };
        let instructions = inline::disassemble(&body.code)?;
        let offsets = inline::insn_offsets_at(&instructions, 0);
        let parameter_at = |slot: u16| {
            parameter_slots
                .iter()
                .position(|candidate| *candidate == slot)
        };
        let invoke_sites = inline::function_invoke_sites(&instructions, &body.source_cp);
        let [invoke] = invoke_sites.as_slice() else {
            return None;
        };
        let invoke_loads = instructions[..*invoke]
            .iter()
            .rev()
            .map_while(inline::loaded_local)
            .collect::<Vec<_>>();
        // Invocation operands are loaded receiver-first. Walking backward sees arguments before the
        // function object.
        let (&lambda_slot, invoke_argument_slots) = invoke_loads.split_last()?;
        let lambda_parameter = parameter_at(lambda_slot)?;

        let first_invoke_operand = invoke.checked_sub(invoke_loads.len())?;
        let exact_operands = instructions
            .get(first_invoke_operand..*invoke)
            .is_some_and(|loads| {
                loads
                    .iter()
                    .zip(invoke_loads.iter().rev())
                    .all(|(load, slot)| loaded_reference_local(load) == Some(*slot))
            });
        let exact_prelude = instructions
            .get(..first_invoke_operand)
            .is_some_and(|prelude| {
                prelude.is_empty()
                    || exact_parameter_null_check(prelude, &body.source_cp, lambda_slot)
            });
        let return_parameter = instructions
            .get(invoke + 1..)
            .and_then(|suffix| exact_lambda_return_parameter(suffix, &parameter_at));
        let semantic_lambda_matches = matches!(
            callable.params.get(lambda_parameter).copied(),
            Some(Ty::Fun(lambda)) if lambda.params.len() == invoke_argument_slots.len()
        );
        if exact_operands
            && exact_prelude
            && semantic_lambda_matches
            && handler_free_lambda_body(&body.handlers)
        {
            if let Some(return_parameter) = return_parameter {
                return Some(InlineBodyPlan::InvokeLambda {
                    lambda_parameter,
                    argument_parameters: invoke_argument_slots
                        .iter()
                        .rev()
                        .map(|slot| parameter_at(*slot))
                        .collect::<Option<Vec<_>>>()?,
                    return_parameter,
                });
            }
        }

        let calls = instructions
            .iter()
            .enumerate()
            .filter_map(|(index, instruction)| {
                let target = inline::invoked_method(instruction, &body.source_cp)?;
                (index != *invoke && !is_structural_marker(target)).then_some((index, target))
            })
            .collect::<Vec<_>>();
        let call_indices = calls.iter().map(|(index, _)| *index).collect::<Vec<_>>();
        if !exact_call_partition(&call_indices, *invoke) {
            return None;
        }
        if !invoke_argument_slots.is_empty() {
            return None;
        }
        let [enter_target, cleanup, repeated] = calls.as_slice() else {
            unreachable!("the exact call partition has three entries")
        };
        if cleanup.1 != repeated.1 {
            return None;
        }
        let Some(enter_member) = self.inline_plan_member(enter_target.1) else {
            *decode_unavailable = true;
            return None;
        };
        let Some(cleanup_member) = self.inline_plan_member(cleanup.1) else {
            *decode_unavailable = true;
            return None;
        };
        if cleanup_member.suspend()
            || !enter_member.suspend()
            || enter_member.ret != Ty::Unit
            || cleanup_member.ret != Ty::Unit
            || enter_target.1 .0 != cleanup.1 .0
            || enter_member.params != cleanup_member.params
            || enter_member.params.len() > 1
        {
            return None;
        }
        let exact_roles = callable.context_count == 0
            && callable.source_receiver.is_some()
            && exact_parameter_roles(
                lambda_parameter,
                callable.params.len(),
                callable.physical_params.len(),
                parameter_slots.len(),
                enter_member.params.len(),
            );
        if !exact_roles {
            return None;
        }
        let Ty::Fun(lambda) = callable.params.get(lambda_parameter).copied()? else {
            return None;
        };
        if !lambda.params.is_empty() || lambda.context_count != 0 || lambda.has_receiver {
            return None;
        }

        let continuation_slot = slot_after_parameters(&callable.physical_params)?;
        let receiver_slot = *parameter_slots.first()?;
        let state = if enter_member.params.is_empty() {
            None
        } else {
            let parameter = 1usize;
            let state_slot = *parameter_slots.get(parameter)?;
            match self.inline_default_is_null(callable, parameter_slots, parameter) {
                None => {
                    *decode_unavailable = true;
                    return None;
                }
                Some(false) => return None,
                Some(true) => {}
            }
            Some((
                state_slot,
                InlineBodyState {
                    parameter,
                    default: crate::libraries::DefaultValue::Null,
                },
            ))
        };
        let mut expected_enter = vec![continuation_slot];
        let mut expected_cleanup = Vec::new();
        if let Some((state_slot, _)) = &state {
            expected_enter.push(*state_slot);
            expected_cleanup.push(*state_slot);
        }
        expected_enter.push(receiver_slot);
        expected_cleanup.push(receiver_slot);
        let (enter_operands, enter_first_producer) = invocation_operands(
            &instructions,
            &body.source_cp,
            enter_target.0,
            enter_target.1 .2,
        )?;
        let (normal_cleanup_operands, normal_cleanup_first_producer) =
            invocation_operands(&instructions, &body.source_cp, cleanup.0, cleanup.1 .2)?;
        let (repeated_cleanup_operands, repeated_cleanup_first_producer) =
            invocation_operands(&instructions, &body.source_cp, repeated.0, repeated.1 .2)?;
        let handler = instruction_index(&offsets, body.handlers.first()?.handler_pc)?;
        let body_start = instruction_index(&offsets, body.handlers.first()?.start_pc)?;
        let normal_cleanup_start = instruction_index(&offsets, body.handlers.first()?.end_pc)?;
        let exceptional_cleanup_start = instruction_index(&offsets, body.handlers.get(1)?.end_pc)?;
        let normal_return =
            normal_result_return(&instructions, &body.source_cp, *invoke, cleanup.0, handler);
        if !valid_finally_contract(FinallyContract {
            enter_pc: *offsets.get(enter_target.0)?,
            lambda_pc: *offsets.get(*invoke)?,
            normal_cleanup_pc: *offsets.get(cleanup.0)?,
            repeated_cleanup_pc: *offsets.get(repeated.0)?,
            enter_operands: &enter_operands,
            normal_cleanup_operands: &normal_cleanup_operands,
            repeated_cleanup_operands: &repeated_cleanup_operands,
            expected_enter_operands: &expected_enter,
            expected_cleanup_operands: &expected_cleanup,
            handlers: &body.handlers,
            enter_result_flows_to_lambda: enter_result_starts_body(
                instructions.get(enter_target.0 + 1..body_start)?,
                &body.source_cp,
            ) && matches!(
                instructions.get(body_start),
                Some(Insn::Plain { op: 0x00, .. })
            ) && body_start.checked_add(2) == Some(*invoke),
            normal_cleanup_starts_finally: is_inline_marker_pair(
                instructions.get(normal_cleanup_start..normal_cleanup_first_producer)?,
                &body.source_cp,
                "finallyStart",
            ),
            exceptional_cleanup_starts_finally: is_inline_marker_pair(
                instructions.get(exceptional_cleanup_start..repeated_cleanup_first_producer)?,
                &body.source_cp,
                "finallyStart",
            ),
            normal_result_flows_to_return: normal_return.is_some(),
            exceptional_cleanup_rethrows: normal_return.is_some_and(|(_, normal_return)| {
                exceptional_cleanup_rethrows(
                    &instructions,
                    &body.source_cp,
                    handler,
                    repeated.0,
                    normal_return,
                )
            }),
            exact_parameter_roles: exact_roles,
            exact_instruction_template: normal_return.is_some_and(
                |(normal_branch, normal_return)| {
                    let Some(scratch_slot) = continuation_slot.checked_add(1) else {
                        return false;
                    };
                    let (
                        Some(prefix),
                        Some(enter_producers),
                        Some(normal_cleanup_producers),
                        Some(repeated_cleanup_producers),
                    ) = (
                        instructions.get(..enter_first_producer),
                        instructions.get(enter_first_producer..enter_target.0),
                        instructions.get(normal_cleanup_first_producer..cleanup.0),
                        instructions.get(repeated_cleanup_first_producer..repeated.0),
                    )
                    else {
                        return false;
                    };
                    cleanup_boundaries_are_immediate(
                        *invoke,
                        normal_cleanup_start,
                        handler,
                        exceptional_cleanup_start,
                    ) && exact_inline_local_prefix(prefix, scratch_slot)
                        && exact_enter_operands(enter_producers, &body.source_cp, &enter_operands)
                        && exact_loaded_operands(normal_cleanup_producers, &normal_cleanup_operands)
                        && exact_loaded_operands(
                            repeated_cleanup_producers,
                            &repeated_cleanup_operands,
                        )
                        && exact_control_flow(&instructions, normal_branch, normal_return)
                        && has_only_template_effects(&instructions)
                },
            ),
        }) {
            return None;
        }
        Some(InlineBodyPlan::SuspendBeforeLambdaFinally {
            lambda_parameter,
            state: state.map(|(_, state)| state),
            enter: Box::new(enter_member),
            cleanup: Box::new(cleanup_member),
        })
    }

    /// Match an invoked physical target to exactly one metadata-normalized Kotlin member. JVM
    /// descriptors identify the realization only; they never supply semantic parameter/result types.
    fn inline_plan_member(&self, target: MethodTarget<'_>) -> Option<LibraryMember> {
        let (owner, name, descriptor, interface) = target;
        let owner = type_name(owner);
        // Decode the raw declaration directly. Going through `classifier_record` here can observe a
        // recursively-building cache entry while the enclosing top-level inline declaration is being
        // normalized; treating that transient partial view as "no plan" would then cache an ordinary
        // call fallback for this declaration.
        let classifier = self.build_library_type(owner)?;
        let mut matches = classifier.members.iter().filter(|member| {
            member
                .physical_name
                .as_deref()
                .unwrap_or(member.name.as_str())
                == name
                && physical_descriptor(member) == descriptor
        });
        let mut member = matches.next()?.clone();
        if matches.next().is_some() {
            return None;
        }
        // Retain the invoked descriptor only as a physical realization. A suspend call's common
        // shape excludes its CPS continuation; the suspend pass appends that operand exactly once.
        let (mut physical_params, physical_ret) = super::parse_method_desc(descriptor)?;
        if member.suspend()
            && !physical_params.pop().is_some_and(|parameter| {
                parameter
                    .obj_internal()
                    .is_some_and(|name| name.matches("kotlin/coroutines/Continuation"))
            })
        {
            return None;
        }
        member.owner = Some(owner);
        member.descriptor = if member.suspend() {
            super::strip_continuation_param(descriptor)
        } else {
            descriptor.to_string()
        };
        member.physical_params = physical_params;
        member.physical_ret = physical_ret;
        member.set_is_interface(interface);
        Some(member)
    }

    /// Whether the `$default` bridge's exact leading mask branch assigns `null` to `parameter`.
    /// `None` means the bridge body could not be read and must not be cached as a stable negative.
    fn inline_default_is_null(
        &self,
        callable: &LibraryCallable,
        parameter_slots: &[u16],
        parameter: usize,
    ) -> Option<bool> {
        let Some(realization) = callable.default_realization.as_deref() else {
            return Some(false);
        };
        if realization.mask_count != 1 {
            return Some(false);
        }
        let owner = realization.declaration_owner.render();
        let body = self
            .cp
            .method_code(&owner, &realization.name, &realization.descriptor)?;
        let Some(instructions) = inline::disassemble(&body.code) else {
            return Some(false);
        };
        let Some(slot) = parameter_slots.get(parameter).copied() else {
            return Some(false);
        };
        let Some(mask_slot) = slot_after_parameters(&realization.real_params)
            .and_then(|slot| slot.checked_add(u16::from(realization.suspend)))
        else {
            return Some(false);
        };
        Some(exact_null_default_prefix(&instructions, mask_slot, slot))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(op: u8) -> Insn {
        Insn::Plain {
            op,
            operands: Vec::new(),
        }
    }

    fn valid_null_default_prefix() -> Vec<Insn> {
        vec![
            Insn::Plain {
                op: 0x15,
                operands: vec![4],
            },
            plain(0x04),
            plain(0x7e),
            Insn::Branch {
                op: 0x99,
                target: inline::BranchTarget::Internal(6),
            },
            plain(0x01),
            plain(0x4c),
            plain(0x00),
        ]
    }

    fn valid_handlers() -> [ExcEntry; 2] {
        [
            ExcEntry {
                start_pc: 20,
                end_pc: 35,
                handler_pc: 40,
                catch_type: 0,
            },
            ExcEntry {
                start_pc: 40,
                end_pc: 45,
                handler_pc: 40,
                catch_type: 0,
            },
        ]
    }

    #[test]
    fn complete_finally_contract_is_accepted() {
        assert!(valid_finally_contract(FinallyContract {
            enter_pc: 10,
            lambda_pc: 25,
            normal_cleanup_pc: 35,
            repeated_cleanup_pc: 45,
            enter_operands: &[3, 1, 0],
            normal_cleanup_operands: &[1, 0],
            repeated_cleanup_operands: &[1, 0],
            expected_enter_operands: &[3, 1, 0],
            expected_cleanup_operands: &[1, 0],
            handlers: &valid_handlers(),
            enter_result_flows_to_lambda: true,
            normal_cleanup_starts_finally: true,
            exceptional_cleanup_starts_finally: true,
            normal_result_flows_to_return: true,
            exceptional_cleanup_rethrows: true,
            exact_parameter_roles: true,
            exact_instruction_template: true,
        }));
    }

    #[test]
    fn exact_masked_null_default_prefix_is_accepted() {
        assert!(exact_null_default_prefix(
            &valid_null_default_prefix(),
            4,
            1,
        ));
    }

    #[test]
    fn invoke_lambda_plan_rejects_an_exception_table() {
        assert!(handler_free_lambda_body(&[]));
        assert!(!handler_free_lambda_body(&valid_handlers()));
    }

    #[test]
    fn conditional_or_effectful_null_default_is_rejected() {
        let mut conditional = valid_null_default_prefix();
        conditional[3] = Insn::Branch {
            op: 0x99,
            target: inline::BranchTarget::Internal(7),
        };
        conditional.insert(4, plain(0x00));
        assert!(!exact_null_default_prefix(&conditional, 4, 1));

        let mut effectful = valid_null_default_prefix();
        effectful.insert(0, plain(0xb8));
        assert!(!exact_null_default_prefix(&effectful, 4, 1));
    }

    #[test]
    fn extra_non_marker_call_before_lambda_is_rejected() {
        assert!(exact_call_partition(&[10, 30, 40], 20));
        assert!(!exact_call_partition(&[5, 10, 30, 40], 20));
    }

    #[test]
    fn extra_physical_parameter_role_is_rejected() {
        assert!(exact_parameter_roles(2, 3, 3, 3, 1));
        assert!(!exact_parameter_roles(2, 3, 4, 4, 1));
        assert!(!exact_parameter_roles(3, 4, 4, 4, 1));
    }

    #[test]
    fn extra_branch_or_switch_is_rejected() {
        let normal = Insn::Branch {
            op: 0xa7,
            target: inline::BranchTarget::Internal(3),
        };
        assert!(exact_control_flow(
            &[
                Insn::Plain {
                    op: 0x00,
                    operands: Vec::new(),
                },
                Insn::Plain {
                    op: 0x00,
                    operands: Vec::new(),
                },
                normal.clone(),
                Insn::Plain {
                    op: 0x00,
                    operands: Vec::new(),
                },
            ],
            2,
            3,
        ));
        assert!(!exact_control_flow(
            &[
                normal.clone(),
                Insn::Branch {
                    op: 0x99,
                    target: inline::BranchTarget::Internal(0),
                },
            ],
            0,
            3,
        ));
        assert!(!exact_control_flow(
            &[
                normal,
                Insn::TableSwitch {
                    default: 0,
                    low: 0,
                    targets: Vec::new(),
                },
            ],
            0,
            3,
        ));
    }

    #[test]
    fn instructions_between_result_or_exception_store_and_finally_are_rejected() {
        assert!(cleanup_boundaries_are_immediate(10, 12, 20, 21));
        assert!(!cleanup_boundaries_are_immediate(10, 13, 20, 21));
        assert!(!cleanup_boundaries_are_immediate(10, 12, 20, 22));
    }

    #[test]
    fn field_monitor_and_mutation_effects_are_rejected() {
        for op in [0x4f, 0x84, 0xb2, 0xb5, 0xba, 0xbb, 0xc2, 0xc3] {
            assert!(!has_only_template_effects(&[Insn::Plain {
                op,
                operands: Vec::new(),
            }]));
        }
    }

    #[test]
    fn invocation_operands_must_be_contiguous_stack_producers() {
        let invoke = Insn::Plain {
            op: 0xb6,
            operands: Vec::new(),
        };
        assert_eq!(
            invocation_operands(
                &[
                    Insn::Plain {
                        op: 0x2a,
                        operands: Vec::new(),
                    },
                    invoke.clone(),
                ],
                &[],
                1,
                "()V",
            ),
            Some((vec![0], 0)),
        );
        assert_eq!(
            invocation_operands(
                &[
                    Insn::Plain {
                        op: 0x2a,
                        operands: Vec::new(),
                    },
                    Insn::Plain {
                        op: 0x00,
                        operands: Vec::new(),
                    },
                    invoke,
                ],
                &[],
                2,
                "()V",
            ),
            None,
            "a non-producing instruction must not be skipped while reconstructing operands",
        );
    }

    #[test]
    fn malformed_finally_contract_is_rejected_before_publication() {
        let handlers = valid_handlers();
        let expected_enter = [3, 1, 0];
        let expected_cleanup = [1, 0];
        let wrong_repeated_cleanup = [2, 0];
        assert!(!valid_finally_contract(FinallyContract {
            enter_pc: 10,
            lambda_pc: 25,
            normal_cleanup_pc: 35,
            repeated_cleanup_pc: 45,
            enter_operands: &expected_enter,
            normal_cleanup_operands: &expected_cleanup,
            repeated_cleanup_operands: &wrong_repeated_cleanup,
            expected_enter_operands: &expected_enter,
            expected_cleanup_operands: &expected_cleanup,
            handlers: &handlers,
            enter_result_flows_to_lambda: true,
            normal_cleanup_starts_finally: true,
            exceptional_cleanup_starts_finally: true,
            normal_result_flows_to_return: true,
            exceptional_cleanup_rethrows: true,
            exact_parameter_roles: true,
            exact_instruction_template: true,
        }));
    }

    #[test]
    fn malformed_handler_range_is_rejected() {
        let mut handlers = valid_handlers();
        handlers[1].end_pc = 46;
        assert!(!valid_finally_contract(FinallyContract {
            enter_pc: 10,
            lambda_pc: 25,
            normal_cleanup_pc: 35,
            repeated_cleanup_pc: 45,
            enter_operands: &[3, 1, 0],
            normal_cleanup_operands: &[1, 0],
            repeated_cleanup_operands: &[1, 0],
            expected_enter_operands: &[3, 1, 0],
            expected_cleanup_operands: &[1, 0],
            handlers: &handlers,
            enter_result_flows_to_lambda: true,
            normal_cleanup_starts_finally: true,
            exceptional_cleanup_starts_finally: true,
            normal_result_flows_to_return: true,
            exceptional_cleanup_rethrows: true,
            exact_parameter_roles: true,
            exact_instruction_template: true,
        }));
    }

    #[test]
    fn missing_finally_start_is_rejected() {
        assert!(!valid_finally_contract(FinallyContract {
            enter_pc: 10,
            lambda_pc: 25,
            normal_cleanup_pc: 35,
            repeated_cleanup_pc: 45,
            enter_operands: &[3, 1, 0],
            normal_cleanup_operands: &[1, 0],
            repeated_cleanup_operands: &[1, 0],
            expected_enter_operands: &[3, 1, 0],
            expected_cleanup_operands: &[1, 0],
            handlers: &valid_handlers(),
            enter_result_flows_to_lambda: true,
            normal_cleanup_starts_finally: false,
            exceptional_cleanup_starts_finally: true,
            normal_result_flows_to_return: true,
            exceptional_cleanup_rethrows: true,
            exact_parameter_roles: true,
            exact_instruction_template: true,
        }));
    }

    #[test]
    fn missing_normal_result_flow_is_rejected() {
        assert!(!valid_finally_contract(FinallyContract {
            enter_pc: 10,
            lambda_pc: 25,
            normal_cleanup_pc: 35,
            repeated_cleanup_pc: 45,
            enter_operands: &[3, 1, 0],
            normal_cleanup_operands: &[1, 0],
            repeated_cleanup_operands: &[1, 0],
            expected_enter_operands: &[3, 1, 0],
            expected_cleanup_operands: &[1, 0],
            handlers: &valid_handlers(),
            enter_result_flows_to_lambda: true,
            normal_cleanup_starts_finally: true,
            exceptional_cleanup_starts_finally: true,
            normal_result_flows_to_return: false,
            exceptional_cleanup_rethrows: true,
            exact_parameter_roles: true,
            exact_instruction_template: true,
        }));
    }

    #[test]
    fn missing_exception_rethrow_is_rejected() {
        assert!(!valid_finally_contract(FinallyContract {
            enter_pc: 10,
            lambda_pc: 25,
            normal_cleanup_pc: 35,
            repeated_cleanup_pc: 45,
            enter_operands: &[3, 1, 0],
            normal_cleanup_operands: &[1, 0],
            repeated_cleanup_operands: &[1, 0],
            expected_enter_operands: &[3, 1, 0],
            expected_cleanup_operands: &[1, 0],
            handlers: &valid_handlers(),
            enter_result_flows_to_lambda: true,
            normal_cleanup_starts_finally: true,
            exceptional_cleanup_starts_finally: true,
            normal_result_flows_to_return: true,
            exceptional_cleanup_rethrows: false,
            exact_parameter_roles: true,
            exact_instruction_template: true,
        }));
    }
}
