//! Decoding an inline declaration's compiled body into a source-independent expansion plan.
//!
//! A `public inline fun` publishes its pre-transform body as bytecode, and krusty expands the shapes
//! it recognizes at IR level rather than splicing bytes — that is what lets a suspension written
//! inside an inline lambda join the CALLER's state machine. This module is the recogniser: it reads
//! one method body and answers which [`InlineBodyPlan`] it is, or none.
//!
//! It decodes structure, never declarations by name. A body that sets up, calls its lambda and
//! tears down is ONE shape whatever library it comes from: `let` populates none of the optional
//! parts, `apply` returns a parameter, `Mutex.withLock` enters a region and leaves it from
//! `finally`, and `Closeable.use` hands its cleanup the throwable that left the body. Everything
//! the recogniser publishes is a provider fact (opaque member handles, parameter ordinals); no
//! target ABI detail escapes it.

mod abstract_body;

use super::{
    parse_method_desc, strip_continuation_param, JvmLibraries, CONTINUATION_PARAM_DESCRIPTOR,
};
use crate::jvm::inline::Insn;
use crate::libraries::{
    DefaultValue, InlineBodyCall, InlineBodyDefault, InlineBodyPlan, InlineBodyValue,
    LibraryCallable, LibraryMember,
};
use crate::types::{type_name, Ty};

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

/// Translate one simulated operand into a plan value. `base` is how many calls of the simulated
/// block precede the list this value will live in, so an index into the block's calls becomes an
/// index into that list. A value produced BEFORE the list cannot be named from inside it.
fn plan_value(value: abstract_body::Value, base: usize) -> Option<InlineBodyValue> {
    Some(match value {
        abstract_body::Value::Parameter(parameter) => InlineBodyValue::Parameter(parameter),
        abstract_body::Value::Cause => InlineBodyValue::Cause,
        abstract_body::Value::Invocation => InlineBodyValue::Invocation,
        abstract_body::Value::Call(index) => InlineBodyValue::Call(index.checked_sub(base)?),
        abstract_body::Value::Opaque => return None,
    })
}

/// The local a body uses to remember the throwable that left its invocation: a non-parameter local
/// initialized to `null` before the invocation and written again afterwards, which is the
/// `catch (t: Throwable) { cause = t; throw t }` kotlinc writes around a cleanup that needs to
/// suppress its own failure onto the body's exception.
fn cause_slot(
    instructions: &[Insn],
    source_cp: &[crate::jvm::classreader::C],
    parameter_slots: &[u16],
) -> Option<u16> {
    let invoke = *crate::jvm::inline::function_invoke_sites(instructions, source_cp).first()?;
    instructions[..invoke.min(instructions.len())]
        .windows(2)
        .filter(|window| matches!(window[0], Insn::Plain { op: 0x01, .. }))
        .filter_map(|window| crate::jvm::inline::stored_local(&window[1]))
        .find(|slot| {
            !parameter_slots.contains(slot)
                && instructions[invoke.min(instructions.len())..]
                    .iter()
                    .any(|instruction| crate::jvm::inline::stored_local(instruction) == Some(*slot))
        })
}

/// A body whose only rethrowing handler is the cause recorder still has a `finally`-free shape, so
/// a recovery may coexist with it. This keeps the two exits from being conflated.
fn rethrows_is_only_recorder(rethrows: bool) -> bool {
    !rethrows
}

/// The declaration a decoded call names, as an opaque provider handle. A trailing continuation
/// parameter is what makes the target a `suspend fun`, so both facts come from its descriptor.
fn inline_plan_member(target: (&str, &str, &str, bool)) -> Option<LibraryMember> {
    let (owner, name, descriptor, interface) = target;
    let suspend = descriptor
        .rfind(')')
        .is_some_and(|close| descriptor[..close].ends_with(CONTINUATION_PARAM_DESCRIPTOR));
    let logical_descriptor = if suspend {
        strip_continuation_param(descriptor)
    } else {
        descriptor.to_string()
    };
    let (params, ret) = parse_method_desc(&logical_descriptor)?;
    let mut member = LibraryMember::new(name.to_string(), params, ret, logical_descriptor);
    member.owner = Some(type_name(owner));
    member.physical_ret = parse_method_desc(descriptor)?.1;
    member.set_is_interface(interface);
    member.set_suspend(suspend);
    Some(member)
}

impl JvmLibraries {
    /// Decode one callable's inline-body plan from bytecode. `body_unavailable` is set (and `None`
    /// returned) when a body READ failed — the caller must not memoize that answer; a `None` with
    /// the flag clear is a decoded "no expandable shape", which is a stable fact of the bytes.
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
        let instructions = crate::jvm::inline::disassemble(&body.code)?;
        let shape = abstract_body::BodyShape {
            instructions: &instructions,
            source_cp: &body.source_cp,
            parameter_slots,
            cause_slot: cause_slot(&instructions, &body.source_cp, parameter_slots),
        };
        let entry = abstract_body::simulate(&shape, 0, Vec::new())?;
        let invocation = entry.invocation.as_ref()?;
        let (prologue_calls, exit_calls) = entry.calls.split_at(invocation.after);

        // Each handler block is simulated once, entered with the caught throwable on the stack
        // exactly as the JVM leaves it. A block that yields a value RECOVERS; one that rethrows is
        // either the `finally` copy or the store that records the cause.
        let mut recover_block = None;
        let mut cleanup_len = 0;
        let mut rethrows = false;
        let mut seen = std::collections::HashSet::new();
        let offsets = crate::jvm::inline::insn_offsets_at(&instructions, 0);
        for handler in body
            .handlers
            .iter()
            .filter(|handler| seen.insert(handler.handler_pc))
        {
            let index = offsets
                .iter()
                .position(|offset| *offset == handler.handler_pc as usize)?;
            let block = abstract_body::simulate(&shape, index, vec![abstract_body::Value::Cause])?;
            if block.invocation.is_some() {
                return None;
            }
            match block.returned {
                Some(_) => {
                    // Two different recoveries would be two plans; one body has one.
                    if recover_block.is_some() {
                        return None;
                    }
                    recover_block = Some(block);
                }
                None => {
                    rethrows = true;
                    // The `finally` copy repeats the tail of the normal exit verbatim.
                    if !block.calls.is_empty() {
                        if !exit_calls.ends_with(&block.calls) {
                            return None;
                        }
                        cleanup_len = cleanup_len.max(block.calls.len());
                    }
                }
            }
        }
        // A body cannot both recover and run a `finally` tail: the two describe different exits and
        // nothing in the stdlib mixes them, so refusing keeps the expansion honest.
        if recover_block.is_some() && (cleanup_len != 0 || !rethrows_is_only_recorder(rethrows)) {
            return None;
        }
        let (normal_calls, cleanup_calls) = exit_calls.split_at(exit_calls.len() - cleanup_len);

        // A body that boxes or unboxes around its invocation is describing REPRESENTATION, not
        // semantics: `inline fun applyIt(x: Int, f: (Int) -> Int) = f(x)` calls `Integer.valueOf`
        // before the invocation and `intValue` after it. Naming those as plan calls would make the
        // expansion hand a boxed value where a primitive is expected, so a body carrying one is not
        // expressible here and keeps the bytecode splice it already had.
        if entry
            .calls
            .iter()
            .chain(recover_block.iter().flat_map(|block| block.calls.iter()))
            .any(|call| crate::jvm::jvm_class_map::wrapper_to_kotlin_prim(call.target.0).is_some())
        {
            return None;
        }
        let member = |call: &abstract_body::SimulatedCall<'_>| inline_plan_member(call.target);
        let decode = |calls: &[abstract_body::SimulatedCall<'_>], base: usize| {
            calls
                .iter()
                .map(|call| {
                    let member = member(call)?;
                    let (dispatch, arguments) = match call.is_static {
                        true => (None, call.operands.as_slice()),
                        false => {
                            let (receiver, rest) = call.operands.split_first()?;
                            (Some(plan_value(*receiver, base)?), rest)
                        }
                    };
                    // A suspend callee's trailing continuation comes from the caller's own state
                    // machine, so it is not one of the plan's semantic arguments.
                    let arguments = &arguments[..arguments.len() - usize::from(member.suspend())];
                    Some(InlineBodyCall {
                        member: Box::new(member),
                        dispatch,
                        arguments: arguments
                            .iter()
                            .map(|value| plan_value(*value, base))
                            .collect::<Option<Vec<_>>>()?,
                    })
                })
                .collect::<Option<Vec<_>>>()
        };
        let prologue = decode(prologue_calls, 0)?;
        let normal = crate::libraries::InlineBodyArm {
            calls: decode(normal_calls, invocation.after)?,
            value: plan_value(entry.returned?, invocation.after)?,
        };
        let cleanup = decode(cleanup_calls, invocation.after + normal.calls.len())?;
        let recover = match recover_block {
            None => None,
            Some(block) => Some(crate::libraries::InlineBodyArm {
                calls: decode(&block.calls, 0)?,
                value: plan_value(block.returned?, 0)?,
            }),
        };
        let arguments = invocation
            .arguments
            .iter()
            .map(|value| plan_value(*value, 0))
            .collect::<Option<Vec<_>>>()?;

        // A parameter the surrounding calls read must survive being omitted at the call site, which
        // it can only do when the declaration's own `$default` bridge supplies a value.
        let mut defaults = Vec::new();
        for parameter in prologue
            .iter()
            .chain(&normal.calls)
            .chain(&cleanup)
            .chain(recover.iter().flat_map(|arm| arm.calls.iter()))
            .flat_map(|call| call.dispatch.iter().chain(&call.arguments))
            .filter_map(|value| match value {
                InlineBodyValue::Parameter(parameter) => Some(*parameter),
                _ => None,
            })
        {
            if defaults
                .iter()
                .any(|default: &InlineBodyDefault| default.parameter == parameter)
            {
                continue;
            }
            match self.inline_default_is_null(callable, parameter_slots, parameter) {
                None => {
                    *body_unavailable = true;
                    return None;
                }
                Some(false) => {}
                Some(true) => defaults.push(InlineBodyDefault {
                    parameter,
                    value: DefaultValue::Null,
                }),
            }
        }

        Some(InlineBodyPlan::InvokeLambda {
            lambda_parameter: invocation.lambda_parameter,
            arguments,
            prologue,
            normal,
            recover,
            cleanup,
            records_cause: shape.cause_slot.is_some() && rethrows,
            defaults,
        })
    }

    /// Whether the `$default` bridge stores `null` into `parameter`'s slot. `None` means the bridge
    /// body could not be READ (a transient failure the caller must not memoize); `Some(false)`
    /// covers every decoded negative, including "the callable has no `$default` bridge at all"
    /// (stable — the bridge descriptor is part of the plan cache key).
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
        // A failed bridge-body READ is the transient case the caller must not memoize.
        let body = self
            .cp
            .method_code(&owner, &realization.name, &realization.descriptor)?;
        let Some(instructions) = crate::jvm::inline::disassemble(&body.code) else {
            return Some(false);
        };
        let Some(slot) = parameter_slots.get(parameter).copied() else {
            return Some(false);
        };
        Some(instructions.windows(2).any(|window| {
            matches!(window[0], Insn::Plain { op: 0x01, .. })
                && crate::jvm::inline::stored_local(&window[1]) == Some(slot)
        }))
    }

    pub(super) fn inline_body_plan(&self, callable: &LibraryCallable) -> Option<InlineBodyPlan> {
        if !callable.inline.can_inline() {
            return None;
        }
        let body_descriptor = inline_body_descriptor(callable)?;
        // Every candidate overload the provider builds computes a plan, so the decode below —
        // body read, disassembly, invoke-site analysis — is memoized per declaration. The key must
        // carry EVERY input the decode reads: besides the bytecode locator, the callable's
        // physical slot layout and `$default` bridge — the same JVM method surfaces through
        // several provider channels (plain, suspend facade, extension) whose plans differ in
        // exactly those parameter indexes.
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
        // "No plan" is only a memoizable FACT when it was decoded from bytes actually read. A
        // failed body read (archive open/read error under load, a jar changing mid-run) must stay
        // transient — publishing it into the per-entry global map would suppress the plan for
        // every later compile sharing the jar (the body cache guards the same hazard one level
        // down: "only a SUCCESSFUL read may populate the process-global cache").
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
}

#[cfg(test)]
mod tests {
    use crate::libraries::{InlineBodyPlan, InlineBodyValue};
    use crate::symbol_source::SymbolNamespace;
    use crate::types::type_name;

    /// Everything the recogniser decided about one compiled body, in a form a test compares whole.
    #[derive(Debug, Eq, PartialEq)]
    struct DecodedPlan {
        descriptor: String,
        lambda_parameter: usize,
        arguments: Vec<InlineBodyValue>,
        prologue: Vec<DecodedCall>,
        normal: DecodedArm,
        recover: Option<DecodedArm>,
        cleanup: Vec<DecodedCall>,
        records_cause: bool,
        defaults: Vec<(usize, String)>,
    }

    #[derive(Debug, Eq, PartialEq)]
    struct DecodedArm {
        calls: Vec<DecodedCall>,
        value: InlineBodyValue,
    }

    #[derive(Debug, Eq, PartialEq)]
    struct DecodedCall {
        owner: String,
        name: String,
        descriptor: String,
        suspend: bool,
        dispatch: Option<InlineBodyValue>,
        arguments: Vec<InlineBodyValue>,
    }

    fn decoded_call(call: &crate::libraries::InlineBodyCall) -> DecodedCall {
        DecodedCall {
            owner: call
                .member
                .owner
                .expect("plan call names its owner")
                .render(),
            name: call.member.name.clone(),
            descriptor: call.member.descriptor.clone(),
            suspend: call.member.suspend(),
            dispatch: call.dispatch,
            arguments: call.arguments.clone(),
        }
    }

    fn decoded_arm(arm: &crate::libraries::InlineBodyArm) -> DecodedArm {
        DecodedArm {
            calls: arm.calls.iter().map(decoded_call).collect(),
            value: arm.value,
        }
    }

    /// Every inline plan decoded for the declarations named `name` in `package`, in the order the
    /// provider publishes them. A declaration with no plan contributes `None`, so a shape that stops
    /// decoding shows up as a diff rather than as a silently skipped candidate.
    fn decoded_plans(
        jars: Vec<std::path::PathBuf>,
        package: &str,
        name: &str,
    ) -> Vec<Option<DecodedPlan>> {
        let libraries = super::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(jars),
        ));
        let symbols = libraries.symbols(SymbolNamespace::Package(type_name(package)), name);
        symbols
            .callables
            .functions()
            .iter()
            .map(|function| {
                let callable = &function.callable;
                match callable.inline_body_plan.as_deref()? {
                    InlineBodyPlan::CollectionTransform { .. } => None,
                    InlineBodyPlan::InvokeLambda {
                        lambda_parameter,
                        arguments,
                        prologue,
                        normal,
                        recover,
                        cleanup,
                        records_cause,
                        defaults,
                    } => Some(DecodedPlan {
                        descriptor: callable.descriptor.clone(),
                        lambda_parameter: *lambda_parameter,
                        arguments: arguments.clone(),
                        prologue: prologue.iter().map(decoded_call).collect(),
                        normal: decoded_arm(normal),
                        recover: recover.as_ref().map(decoded_arm),
                        cleanup: cleanup.iter().map(decoded_call).collect(),
                        records_cause: *records_cause,
                        defaults: defaults
                            .iter()
                            .map(|default| (default.parameter, format!("{:?}", default.value)))
                            .collect(),
                    }),
                }
            })
            .collect()
    }

    /// `let` populates none of the optional parts: no prologue, no cleanup, no recorded cause, and
    /// the invocation result is the declaration's result.
    #[test]
    fn metadata_inline_bodies_decode_parameter_roles_without_source_dispatch() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        assert_eq!(
            decoded_plans(vec![stdlib], "kotlin", "let"),
            [Some(DecodedPlan {
                descriptor:
                    "(Ljava/lang/Object;Lkotlin/jvm/functions/Function1;)Ljava/lang/Object;"
                        .to_owned(),
                lambda_parameter: 1,
                arguments: vec![InlineBodyValue::Parameter(0)],
                prologue: Vec::new(),
                normal: DecodedArm {
                    calls: Vec::new(),
                    value: InlineBodyValue::Invocation,
                },
                recover: None,
                cleanup: Vec::new(),
                records_cause: false,
                defaults: Vec::new(),
            })]
        );
    }

    /// `Mutex.withLock` enters a suspending region on the receiver and leaves it from `finally`,
    /// threading its `owner` parameter — which the caller may omit — into both members.
    #[test]
    fn suspend_finally_inline_body_decodes_exact_member_handles() {
        let (Some(stdlib), Some(coroutines)) = (
            crate::toolchain::stdlib_jar(),
            crate::toolchain::coroutines_jar(),
        ) else {
            return;
        };
        let owner = "kotlinx/coroutines/sync/Mutex".to_owned();
        assert_eq!(
            decoded_plans(
                vec![stdlib, coroutines],
                "kotlinx/coroutines/sync",
                "withLock"
            ),
            [Some(DecodedPlan {
                descriptor: "(Lkotlinx/coroutines/sync/Mutex;Ljava/lang/Object;\
                             Lkotlin/jvm/functions/Function0;)Ljava/lang/Object;"
                    .to_owned(),
                lambda_parameter: 2,
                arguments: Vec::new(),
                prologue: vec![DecodedCall {
                    owner: owner.clone(),
                    name: "lock".to_owned(),
                    descriptor: "(Ljava/lang/Object;)Ljava/lang/Object;".to_owned(),
                    suspend: true,
                    dispatch: Some(InlineBodyValue::Parameter(0)),
                    arguments: vec![InlineBodyValue::Parameter(1)],
                }],
                cleanup: vec![DecodedCall {
                    owner,
                    name: "unlock".to_owned(),
                    descriptor: "(Ljava/lang/Object;)V".to_owned(),
                    suspend: false,
                    dispatch: Some(InlineBodyValue::Parameter(0)),
                    arguments: vec![InlineBodyValue::Parameter(1)],
                }],
                normal: DecodedArm {
                    calls: Vec::new(),
                    value: InlineBodyValue::Invocation,
                },
                recover: None,
                records_cause: false,
                defaults: vec![(1, "Null".to_owned())],
            })]
        );
    }

    /// The same shape with NO threaded argument. `Semaphore.withPermit` calls `acquire(continuation)`
    /// and `release()`, where `Mutex.withLock` calls `lock(owner, continuation)` and `unlock(owner)`.
    /// Counting each call's operands from its own descriptor is what makes both decode; a fixed
    /// operand position recognized only the one that happens to carry an extra parameter.
    #[test]
    fn suspend_finally_inline_body_decodes_without_a_state_argument() {
        let (Some(stdlib), Some(coroutines)) = (
            crate::toolchain::stdlib_jar(),
            crate::toolchain::coroutines_jar(),
        ) else {
            return;
        };
        let owner = "kotlinx/coroutines/sync/Semaphore".to_owned();
        assert_eq!(
            decoded_plans(
                vec![stdlib, coroutines],
                "kotlinx/coroutines/sync",
                "withPermit"
            ),
            [Some(DecodedPlan {
                descriptor: "(Lkotlinx/coroutines/sync/Semaphore;\
                             Lkotlin/jvm/functions/Function0;)Ljava/lang/Object;"
                    .to_owned(),
                lambda_parameter: 1,
                arguments: Vec::new(),
                prologue: vec![DecodedCall {
                    owner: owner.clone(),
                    name: "acquire".to_owned(),
                    descriptor: "()Ljava/lang/Object;".to_owned(),
                    suspend: true,
                    dispatch: Some(InlineBodyValue::Parameter(0)),
                    arguments: Vec::new(),
                }],
                cleanup: vec![DecodedCall {
                    owner,
                    name: "release".to_owned(),
                    descriptor: "()V".to_owned(),
                    suspend: false,
                    dispatch: Some(InlineBodyValue::Parameter(0)),
                    arguments: Vec::new(),
                }],
                normal: DecodedArm {
                    calls: Vec::new(),
                    value: InlineBodyValue::Invocation,
                },
                recover: None,
                records_cause: false,
                defaults: Vec::new(),
            })]
        );
    }

    /// `runCatching` is the body a backward LOCAL scan cannot read: it hands the invocation's result
    /// straight to the wrapper that builds a `Result` and never stores it. Its exceptional exit
    /// produces a value instead of rethrowing, which is what makes it a recovering arm rather than a
    /// `finally`.
    #[test]
    fn run_catching_decodes_a_recovering_arm_over_the_stack() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let wrap = |arguments: Vec<InlineBodyValue>| DecodedCall {
            owner: "kotlin/Result".to_owned(),
            name: "constructor-impl".to_owned(),
            descriptor: "(Ljava/lang/Object;)Ljava/lang/Object;".to_owned(),
            suspend: false,
            dispatch: None,
            arguments,
        };
        assert_eq!(
            decoded_plans(vec![stdlib], "kotlin", "runCatching"),
            [
                Some(DecodedPlan {
                    descriptor: "(Lkotlin/jvm/functions/Function0;)Ljava/lang/Object;".to_owned(),
                    lambda_parameter: 0,
                    arguments: Vec::new(),
                    prologue: Vec::new(),
                    normal: DecodedArm {
                        calls: vec![wrap(vec![InlineBodyValue::Invocation])],
                        value: InlineBodyValue::Call(0),
                    },
                    recover: Some(DecodedArm {
                        calls: vec![
                            DecodedCall {
                                owner: "kotlin/ResultKt".to_owned(),
                                name: "createFailure".to_owned(),
                                descriptor: "(Ljava/lang/Throwable;)Ljava/lang/Object;".to_owned(),
                                suspend: false,
                                dispatch: None,
                                arguments: vec![InlineBodyValue::Cause],
                            },
                            wrap(vec![InlineBodyValue::Call(0)]),
                        ],
                        value: InlineBodyValue::Call(1),
                    }),
                    cleanup: Vec::new(),
                    records_cause: false,
                    defaults: Vec::new(),
                }),
                Some(DecodedPlan {
                    descriptor: "(Ljava/lang/Object;Lkotlin/jvm/functions/Function1;)\
                                 Ljava/lang/Object;"
                        .to_owned(),
                    lambda_parameter: 1,
                    arguments: vec![InlineBodyValue::Parameter(0)],
                    prologue: Vec::new(),
                    normal: DecodedArm {
                        calls: vec![wrap(vec![InlineBodyValue::Invocation])],
                        value: InlineBodyValue::Call(0),
                    },
                    recover: Some(DecodedArm {
                        calls: vec![
                            DecodedCall {
                                owner: "kotlin/ResultKt".to_owned(),
                                name: "createFailure".to_owned(),
                                descriptor: "(Ljava/lang/Throwable;)Ljava/lang/Object;".to_owned(),
                                suspend: false,
                                dispatch: None,
                                arguments: vec![InlineBodyValue::Cause],
                            },
                            wrap(vec![InlineBodyValue::Call(0)]),
                        ],
                        value: InlineBodyValue::Call(1),
                    }),
                    cleanup: Vec::new(),
                    records_cause: false,
                    defaults: Vec::new(),
                }),
            ]
        );
    }

    /// A body that invokes its lambda from ONE arm of an `if` is NOT this shape. `ifEmpty` reads its
    /// receiver's length first and calls the lambda only when it is empty; decoding that call as an
    /// unconditional prologue and the invocation as unconditional would run both arms and drop the
    /// condition, so the recogniser must publish no plan at all.
    #[test]
    fn a_conditional_lambda_invocation_decodes_to_no_plan() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        assert_eq!(
            decoded_plans(vec![stdlib], "kotlin/text", "ifEmpty"),
            [None]
        );
    }

    /// `Closeable.use` is the same one shape with three parts populated differently: it invokes its
    /// lambda WITH the receiver, its cleanup is a top-level function rather than a call on the
    /// receiver, and it records the throwable that left the body so the cleanup can suppress a
    /// failing `close()` onto it instead of replacing it.
    #[test]
    fn use_inline_body_decodes_a_top_level_cleanup_reading_the_recorded_cause() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        assert_eq!(
            decoded_plans(vec![stdlib], "kotlin/io", "use"),
            [Some(DecodedPlan {
                descriptor: "(Ljava/io/Closeable;Lkotlin/jvm/functions/Function1;)\
                             Ljava/lang/Object;"
                    .to_owned(),
                lambda_parameter: 1,
                arguments: vec![InlineBodyValue::Parameter(0)],
                prologue: Vec::new(),
                cleanup: vec![DecodedCall {
                    owner: "kotlin/io/CloseableKt".to_owned(),
                    name: "closeFinally".to_owned(),
                    descriptor: "(Ljava/io/Closeable;Ljava/lang/Throwable;)V".to_owned(),
                    suspend: false,
                    dispatch: None,
                    arguments: vec![InlineBodyValue::Parameter(0), InlineBodyValue::Cause],
                }],
                normal: DecodedArm {
                    calls: Vec::new(),
                    value: InlineBodyValue::Invocation,
                },
                recover: None,
                records_cause: true,
                defaults: Vec::new(),
            })]
        );
    }
}
