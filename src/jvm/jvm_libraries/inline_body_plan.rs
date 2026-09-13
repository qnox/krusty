//! Provider-side decoding of declaration-scoped inline control-flow plans.
//!
//! Bytecode is used only to recognize the physical control-flow and exact call targets. Semantic
//! member signatures come from the Kotlin classifier model built from metadata.

use super::{JvmLibraries, CONTINUATION_PARAM_DESCRIPTOR};
use crate::jvm::classreader::ExcEntry;
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

fn invocation_operands(instructions: &[Insn], index: usize, descriptor: &str) -> Option<Vec<u16>> {
    let parameter_count = crate::jvm::names::parse_method_descriptor(descriptor)?
        .0
        .len();
    let operands = instructions[..index]
        .iter()
        .rev()
        .filter_map(inline::loaded_local)
        .take(parameter_count + 1)
        .collect::<Vec<_>>();
    (operands.len() == parameter_count + 1).then_some(operands)
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
        let default_descriptor = callable
            .default_realization
            .as_deref()
            .map(|realization| realization.descriptor.as_str());
        if let Some(plan) = self.cp.cached_inline_plan(
            callable.owner,
            &callable.name,
            &body_descriptor,
            &parameter_slots,
            default_descriptor,
        ) {
            return plan.map(|boxed| *boxed);
        }
        let mut body_unavailable = false;
        let plan = self.inline_body_plan_uncached(
            callable,
            &body_descriptor,
            &parameter_slots,
            &mut body_unavailable,
        );
        // A failed body read is transient. Do not globally cache it as a declaration fact.
        if !body_unavailable {
            self.cp.memoize_inline_plan(
                callable.owner,
                &callable.name,
                &body_descriptor,
                &parameter_slots,
                default_descriptor,
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
        body_unavailable: &mut bool,
    ) -> Option<InlineBodyPlan> {
        let owner = callable.owner.render();
        let inline_name = format!("{}$$forInline", callable.name);
        let Some(body) = self
            .cp
            .method_code(&owner, &inline_name, body_descriptor)
            .or_else(|| self.cp.method_code(&owner, &callable.name, body_descriptor))
        else {
            *body_unavailable = true;
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

        let calls = instructions
            .iter()
            .enumerate()
            .filter_map(|(index, instruction)| {
                let target = inline::invoked_method(instruction, &body.source_cp)?;
                (!target.0.starts_with("kotlin/jvm/internal/")
                    && !target.0.starts_with("kotlin/jvm/functions/"))
                .then_some((index, target))
            })
            .collect::<Vec<_>>();
        let enter_candidates = calls
            .iter()
            .filter(|(index, _)| *index < *invoke)
            .filter_map(|(index, target)| {
                self.inline_plan_member(*target)
                    .filter(|member| member.suspend())
                    .map(|member| (*index, *target, member))
            })
            .collect::<Vec<_>>();
        if enter_candidates.is_empty() {
            if !calls.is_empty() {
                return None;
            }
            return Some(InlineBodyPlan::InvokeLambda {
                lambda_parameter,
                argument_parameters: invoke_argument_slots
                    .iter()
                    .rev()
                    .map(|slot| parameter_at(*slot))
                    .collect::<Option<Vec<_>>>()?,
                return_parameter: instructions
                    .iter()
                    .rev()
                    .nth(1)
                    .and_then(inline::loaded_local)
                    .and_then(parameter_at),
            });
        }
        let [enter] = enter_candidates.as_slice() else {
            return None;
        };
        if !invoke_argument_slots.is_empty() {
            return None;
        }
        let cleanup_calls = calls
            .iter()
            .filter(|(index, _)| *index > *invoke)
            .collect::<Vec<_>>();
        let [cleanup, repeated] = cleanup_calls.as_slice() else {
            return None;
        };
        if cleanup.1 != repeated.1 {
            return None;
        }
        let cleanup_member = self.inline_plan_member(cleanup.1)?;
        if cleanup_member.suspend()
            || enter.2.ret != Ty::Unit
            || cleanup_member.ret != Ty::Unit
            || enter.1 .0 != cleanup.1 .0
            || enter.2.params != cleanup_member.params
            || enter.2.params.len() > 1
        {
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
        let state = if enter.2.params.is_empty() {
            None
        } else {
            let parameter = 1usize;
            let state_slot = *parameter_slots.get(parameter)?;
            match self.inline_default_is_null(callable, parameter_slots, parameter) {
                None => {
                    *body_unavailable = true;
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
        let enter_operands = invocation_operands(&instructions, enter.0, enter.1 .2)?;
        let normal_cleanup_operands = invocation_operands(&instructions, cleanup.0, cleanup.1 .2)?;
        let repeated_cleanup_operands =
            invocation_operands(&instructions, repeated.0, repeated.1 .2)?;
        if !valid_finally_contract(FinallyContract {
            enter_pc: *offsets.get(enter.0)?,
            lambda_pc: *offsets.get(*invoke)?,
            normal_cleanup_pc: *offsets.get(cleanup.0)?,
            repeated_cleanup_pc: *offsets.get(repeated.0)?,
            enter_operands: &enter_operands,
            normal_cleanup_operands: &normal_cleanup_operands,
            repeated_cleanup_operands: &repeated_cleanup_operands,
            expected_enter_operands: &expected_enter,
            expected_cleanup_operands: &expected_cleanup,
            handlers: &body.handlers,
        }) {
            return None;
        }
        Some(InlineBodyPlan::SuspendBeforeLambdaFinally {
            lambda_parameter,
            state: state.map(|(_, state)| state),
            enter: Box::new(enter.2.clone()),
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

    /// Whether the `$default` bridge stores `null` into `parameter`'s slot. `None` means the bridge
    /// body could not be read and must not be cached as a stable negative.
    fn inline_default_is_null(
        &self,
        callable: &LibraryCallable,
        parameter_slots: &[u16],
        parameter: usize,
    ) -> Option<bool> {
        let Some(realization) = callable.default_realization.as_deref() else {
            return Some(false);
        };
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
        Some(instructions.windows(2).any(|window| {
            matches!(window[0], Insn::Plain { op: 0x01, .. })
                && inline::stored_local(&window[1]) == Some(slot)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_finally_contract_is_rejected_before_publication() {
        let handlers = [
            ExcEntry {
                start_pc: 20,
                end_pc: 30,
                handler_pc: 40,
                catch_type: 0,
            },
            ExcEntry {
                start_pc: 40,
                end_pc: 42,
                handler_pc: 40,
                catch_type: 0,
            },
        ];
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
        }));
    }
}
