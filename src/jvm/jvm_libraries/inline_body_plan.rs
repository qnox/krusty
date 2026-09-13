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

use super::{
    parse_method_desc, strip_continuation_param, JvmLibraries, CONTINUATION_PARAM_DESCRIPTOR,
};
use crate::jvm::inline::{BranchTarget, Insn};
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

/// One non-intrinsic call decoded out of an inline body: its instruction index, its opcode, and the
/// method it targets.
struct DecodedCall<'a> {
    index: usize,
    opcode: u8,
    target: (&'a str, &'a str, &'a str, bool),
}

impl DecodedCall<'_> {
    /// `invokestatic` is what distinguishes a top-level function from a call on a receiver. The
    /// distinction is the opcode's, not a guess from the owner's name.
    fn is_static(&self) -> bool {
        self.opcode == 0xb8
    }
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

/// Whether the body runs straight through: no arms, no loops.
///
/// A plan claims "these calls run, then the lambda, then these calls" — which only a body without
/// branching of its own can honour. `CharSequence.ifEmpty` reads its receiver's length and then
/// invokes its lambda from ONE arm of an `if`; expanded as an unconditional prologue plus an
/// unconditional invocation it would run both arms and lose the condition entirely.
///
/// Unconditional FORWARD jumps are not arms, and these bodies do contain them: one past the
/// catch-all copy of a `finally`, and one to the very next instruction around an inline marker. A
/// backward jump is a loop, which this shape cannot describe.
fn is_unconditional_body(instructions: &[Insn]) -> bool {
    instructions
        .iter()
        .enumerate()
        .all(|(index, instruction)| match instruction {
            Insn::Plain { .. } => true,
            Insn::Branch {
                op: 0xa7,
                target: BranchTarget::Internal(target),
            }
            | Insn::BranchW {
                op: 0xc8,
                target: BranchTarget::Internal(target),
            } => *target > index,
            Insn::Branch { .. }
            | Insn::BranchW { .. }
            | Insn::TableSwitch { .. }
            | Insn::LookupSwitch { .. } => false,
        })
}

/// The locals a call's operands are loaded from, in left-to-right order: the dispatch receiver (for
/// anything but `invokestatic`) followed by one local per declared parameter.
///
/// JVM operands are pushed left to right, so this walks BACKWARD and reverses. Intervening
/// instructions are skipped rather than terminating the walk — kotlinc emits `InlineMarker.mark` and
/// `InlineMarker.finallyStart` between a call's operands and the call itself — which is why the
/// count comes from the target's own descriptor instead of scanning to the first non-load.
fn call_operand_slots(instructions: &[Insn], call: &DecodedCall<'_>) -> Option<Vec<u16>> {
    let count = parse_method_desc(call.target.2)?.0.len() + usize::from(!call.is_static());
    let mut slots = instructions[..call.index]
        .iter()
        .rev()
        .filter_map(crate::jvm::inline::loaded_local)
        .take(count)
        .collect::<Vec<_>>();
    (slots.len() == count).then(|| {
        slots.reverse();
        slots
    })
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
        let parameter_at = |slot: u16| {
            parameter_slots
                .iter()
                .position(|candidate| *candidate == slot)
        };
        let invoke_sites =
            crate::jvm::inline::function_invoke_sites(&instructions, &body.source_cp);
        let [invoke] = invoke_sites.as_slice() else {
            return None;
        };
        let invoke = *invoke;
        let invoke_loads = instructions[..invoke]
            .iter()
            .rev()
            .map_while(crate::jvm::inline::loaded_local)
            .collect::<Vec<_>>();
        // JVM invocation operands are loaded receiver-first. Walking backward therefore sees
        // arguments first and the function object last.
        let (&lambda_slot, invoke_argument_slots) = invoke_loads.split_last()?;
        let lambda_parameter = parameter_at(lambda_slot)?;
        if !is_unconditional_body(&instructions) {
            return None;
        }
        let arguments = invoke_argument_slots
            .iter()
            .rev()
            .map(|slot| parameter_at(*slot).map(InlineBodyValue::Parameter))
            .collect::<Option<Vec<_>>>()?;

        let calls = instructions
            .iter()
            .enumerate()
            .filter_map(|(index, instruction)| {
                let target = crate::jvm::inline::invoked_method(instruction, &body.source_cp)?;
                let Insn::Plain { op, .. } = instruction else {
                    return None;
                };
                (!target.0.starts_with("kotlin/jvm/internal/")
                    && !target.0.starts_with("kotlin/jvm/functions/"))
                .then_some(DecodedCall {
                    index,
                    opcode: *op,
                    target,
                })
            })
            .collect::<Vec<_>>();
        let (prologue_calls, exit_calls) =
            calls.split_at(calls.partition_point(|call| call.index < invoke));
        // kotlinc emits the whole `finally` block TWICE — once on the normal exit and once in the
        // catch-all handler. A tail that is not exactly one sequence repeated is some other shape.
        let (cleanup_calls, repeated) = exit_calls.split_at(exit_calls.len() / 2);
        if exit_calls.len() % 2 != 0
            || cleanup_calls
                .iter()
                .zip(repeated)
                .any(|(call, again)| call.target != again.target)
        {
            return None;
        }

        // The body records the throwable that left the lambda when it initializes a non-parameter
        // local to `null` before the invocation and stores into it again afterwards — which is the
        // `catch (t: Throwable) { cause = t; throw t }` kotlinc writes around a cleanup that needs
        // to suppress its own failure onto the body's exception.
        let cause_slot = instructions[..invoke]
            .windows(2)
            .filter(|window| matches!(window[0], Insn::Plain { op: 0x01, .. }))
            .filter_map(|window| crate::jvm::inline::stored_local(&window[1]))
            .find(|slot| {
                parameter_at(*slot).is_none()
                    && instructions[invoke..].iter().any(|instruction| {
                        crate::jvm::inline::stored_local(instruction) == Some(*slot)
                    })
            });
        // A recorded cause is only faithful if the body really catches every throwable; a narrower
        // handler would leave the expansion claiming a `catch (t: Throwable)` it does not have.
        let records_cause = cause_slot.is_some_and(|_| {
            body.handlers.iter().any(|handler| {
                crate::jvm::inline::caught_class(&body.source_cp, handler.catch_type)
                    == Some("java/lang/Throwable")
            })
        });
        let value_at = |slot: u16| match parameter_at(slot) {
            Some(parameter) => Some(InlineBodyValue::Parameter(parameter)),
            None if records_cause && Some(slot) == cause_slot => Some(InlineBodyValue::Cause),
            None => None,
        };
        let decode_call = |call: &DecodedCall<'_>| {
            let slots = call_operand_slots(&instructions, call)?;
            let (dispatch, argument_slots) = match call.is_static() {
                true => (None, slots.as_slice()),
                false => {
                    let (receiver, rest) = slots.split_first()?;
                    (Some(value_at(*receiver)?), rest)
                }
            };
            let member = inline_plan_member(call.target)?;
            // A suspend callee's trailing continuation is supplied by the caller's own state
            // machine, so it is not one of the plan's semantic arguments.
            let argument_slots =
                &argument_slots[..argument_slots.len() - usize::from(member.suspend())];
            Some(InlineBodyCall {
                member: Box::new(member),
                dispatch,
                arguments: argument_slots
                    .iter()
                    .map(|slot| value_at(*slot))
                    .collect::<Option<Vec<_>>>()?,
            })
        };
        let prologue = prologue_calls
            .iter()
            .map(decode_call)
            .collect::<Option<Vec<_>>>()?;
        let cleanup = cleanup_calls
            .iter()
            .map(decode_call)
            .collect::<Option<Vec<_>>>()?;

        // `apply`/`also` end on a parameter rather than the invocation result.
        let result = instructions
            .iter()
            .rev()
            .nth(1)
            .and_then(crate::jvm::inline::loaded_local)
            .and_then(parameter_at)
            .map(InlineBodyValue::Parameter);

        // A parameter the surrounding calls read must survive being omitted at the call site, which
        // it can only do when the declaration's own `$default` bridge supplies a value.
        let mut defaults = Vec::new();
        for parameter in prologue
            .iter()
            .chain(&cleanup)
            .flat_map(|call| call.dispatch.iter().chain(&call.arguments))
            .filter_map(|value| match value {
                InlineBodyValue::Parameter(parameter) => Some(*parameter),
                InlineBodyValue::Cause => None,
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
            lambda_parameter,
            arguments,
            prologue,
            cleanup,
            records_cause,
            defaults,
            result,
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
        cleanup: Vec<DecodedCall>,
        records_cause: bool,
        defaults: Vec<(usize, String)>,
        result: Option<InlineBodyValue>,
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
                        cleanup,
                        records_cause,
                        defaults,
                        result,
                    } => Some(DecodedPlan {
                        descriptor: callable.descriptor.clone(),
                        lambda_parameter: *lambda_parameter,
                        arguments: arguments.clone(),
                        prologue: prologue.iter().map(decoded_call).collect(),
                        cleanup: cleanup.iter().map(decoded_call).collect(),
                        records_cause: *records_cause,
                        defaults: defaults
                            .iter()
                            .map(|default| (default.parameter, format!("{:?}", default.value)))
                            .collect(),
                        result: *result,
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
                cleanup: Vec::new(),
                records_cause: false,
                defaults: Vec::new(),
                result: None,
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
                records_cause: false,
                defaults: vec![(1, "Null".to_owned())],
                result: None,
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
                records_cause: false,
                defaults: Vec::new(),
                result: None,
            })]
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
                records_cause: true,
                defaults: Vec::new(),
                result: None,
            })]
        );
    }
}
