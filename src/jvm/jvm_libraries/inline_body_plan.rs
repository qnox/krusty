//! Provider-side decoding of declaration-scoped inline control-flow plans.
//!
//! Bytecode is used only to recognize the physical control-flow and exact call targets. Semantic
//! member signatures come from the Kotlin classifier model built from metadata.

mod collection_transform;
mod dependencies;
mod iteration;
mod recovery;

use super::inline_capability::metadata_inline;
use super::JvmLibraries;
use crate::jvm::classreader::{ExcEntry, C};
use crate::jvm::inline::{self, Insn};
use crate::libraries::{
    FnKind, InlineBodyCall, InlineBodyCallReceiver, InlineBodyPlan, InlineBodyValue,
    InlineIterationIndex, LibraryCallable, LibraryMember,
};
use crate::types::{type_name, Ty, TyKind};

type MethodTarget<'a> = (&'a str, &'a str, &'a str, bool);

enum InlineDependency<T> {
    Found(T),
    Rejected,
    Unavailable,
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

fn stored_int_local(instruction: &Insn) -> Option<u16> {
    let Insn::Plain { op, operands } = instruction else {
        return None;
    };
    match *op {
        0x3b..=0x3e => Some(u16::from(*op - 0x3b)),
        0x36 => operands.first().copied().map(u16::from),
        0xc4 if operands.first() == Some(&0x36) => operands
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

fn exact_plain(instruction: &Insn, op: u8) -> bool {
    matches!(instruction, Insn::Plain { op: actual, operands } if *actual == op && operands.is_empty())
}

fn exact_marker_one(instructions: &[Insn], source_cp: &[C], name: &str) -> bool {
    let [one, marker] = instructions else {
        return false;
    };
    exact_plain(one, 0x04)
        && inline::invoked_method(marker, source_cp).is_some_and(
            |(owner, actual_name, descriptor, _)| {
                owner == "kotlin/jvm/internal/InlineMarker"
                    && actual_name == name
                    && descriptor == "(I)V"
            },
        )
}

/// Every body this module recognizes as an invoke-lambda plan has a prelude that is empty or a
/// parameter null-check. A declaration compiled with its own function-marker store
/// (`iconst_0; istore $i$f$`) fails that exact template, so a recognized invoke-lambda body is
/// exactly an `@InlineOnly` declaration: its frame carries no body debug surface of its own.
fn invoke_lambda_frame(callable: &LibraryCallable) -> crate::libraries::InlineBodyFrame {
    crate::libraries::InlineBodyFrame {
        callee: callable.name.as_str().into(),
        inline_only: true,
        source: None,
    }
}

/// Resolve one line of a recognized declaration body through the body's own debug mapping — the
/// same resolution the bytecode splice applies when it maps an inlined line into a caller. A
/// present mapping that does not cover the line drops it, exactly as the splice would.
fn resolve_body_line(
    body: &crate::jvm::classreader::MethodCode,
    line: u16,
) -> Option<(Box<str>, Box<str>, u16)> {
    match body.dependency_source_map.as_ref() {
        Some(map) => map
            .resolve(line)
            .map(|(name, path, source)| (name.into(), path.into(), source)),
        None => Some((
            body.source_file.as_deref()?.into(),
            body.defining_class.as_str().into(),
            line,
        )),
    }
}

/// The line covering byte `pc` in the body's own line table: the last entry at or before it.
fn body_line_at(body: &crate::jvm::classreader::MethodCode, pc: usize) -> Option<u16> {
    body.lines
        .iter()
        .rev()
        .find(|(start, _)| usize::from(*start) <= pc)
        .map(|(_, line)| *line)
}

/// The debug source of a recognized non-inline-only declaration body, given the instruction index
/// of its first loop instruction and of the frame-closing return. Both lines resolve under the one
/// declaration's file identity; the recognized loop templates never span files.
fn recognized_body_source(
    body: &crate::jvm::classreader::MethodCode,
    instructions: &[Insn],
    loop_start: usize,
    close: usize,
    element: Option<Box<str>>,
) -> Option<crate::libraries::InlineBodySource> {
    let offsets = inline::insn_offsets_at(instructions, 0);
    let invoke_line = body_line_at(body, *offsets.get(loop_start)?)?;
    let close_line = body_line_at(body, *offsets.get(close)?)?;
    let (file, path, invoke_line) = resolve_body_line(body, invoke_line)?;
    let (_, _, close_line) = resolve_body_line(body, close_line)?;
    Some(crate::libraries::InlineBodySource {
        file,
        path,
        invoke_line,
        close_line,
        element,
    })
}

impl JvmLibraries {
    fn register_inline_iteration_traversal(
        &self,
        traversal: &mut crate::libraries::InlineIterationTraversal,
    ) {
        match traversal {
            crate::libraries::InlineIterationTraversal::Iterator {
                prepare,
                has_next,
                next,
            } => {
                for member in prepare.iter_mut().chain([has_next.as_mut(), next.as_mut()]) {
                    self.register_external_inline_member(member);
                }
            }
            crate::libraries::InlineIterationTraversal::Array => {}
            crate::libraries::InlineIterationTraversal::Counted { size, get } => {
                self.register_external_inline_member(size);
                self.register_external_inline_member(get);
            }
        }
    }

    /// Assign stable external identities to every declaration referenced by an inline plan before
    /// the plan crosses the provider boundary.
    pub(super) fn register_inline_body_plan_dependencies(&self, plan: &mut InlineBodyPlan) {
        match plan {
            InlineBodyPlan::ReifiedTypeTest { .. } => {}
            InlineBodyPlan::InvokeLambda {
                prologue,
                cleanup,
                recovery,
                ..
            } => {
                for call in prologue.iter_mut().chain(cleanup) {
                    let kind = match call.receiver {
                        Some(InlineBodyCallReceiver::Dispatch(_)) => FnKind::Member,
                        Some(InlineBodyCallReceiver::Extension(_)) => FnKind::Extension,
                        None => FnKind::TopLevel,
                    };
                    self.register_external_inline_dependency_callable(&mut call.callable, kind);
                }
                if let Some(recovery) = recovery {
                    let owner = recovery
                        .constructor
                        .owner
                        .expect("inline recovery constructor must name its classifier");
                    recovery.constructor.external_identity = None;
                    self.register_external_constructor(owner, &mut recovery.constructor);
                    self.register_external_inline_dependency_callable(
                        &mut recovery.failure.callable,
                        FnKind::TopLevel,
                    );
                }
            }
            InlineBodyPlan::Iteration {
                index, traversal, ..
            } => {
                if let Some(InlineIterationIndex::Checked { overflow }) = index {
                    self.register_external_inline_dependency_callable(
                        &mut overflow.callable,
                        FnKind::TopLevel,
                    );
                }
                self.register_inline_iteration_traversal(traversal);
            }
            InlineBodyPlan::CollectionTransform {
                traversal,
                factory,
                capacity,
                append,
                ..
            } => {
                self.register_inline_iteration_traversal(traversal);
                let owner = factory
                    .owner
                    .expect("collection inline factory must name its classifier");
                factory.external_identity = None;
                self.register_external_constructor(owner, factory);
                if let Some(capacity) = capacity {
                    match capacity {
                        crate::libraries::InlineCollectionCapacity::Member(member) => {
                            self.register_external_inline_member(member)
                        }
                        crate::libraries::InlineCollectionCapacity::Extension {
                            callable, ..
                        } => self.register_external_inline_dependency_callable(
                            callable,
                            FnKind::Extension,
                        ),
                    }
                }
                match append {
                    crate::libraries::InlineCollectionAppend::Member(member) => {
                        self.register_external_inline_member(member)
                    }
                    crate::libraries::InlineCollectionAppend::Extension(callable) => self
                        .register_external_inline_dependency_callable(callable, FnKind::Extension),
                }
            }
        }
    }

    pub(super) fn inline_body_plan(&self, callable: &LibraryCallable) -> Option<InlineBodyPlan> {
        // A suspend inline function is inlined from its bytecode, as kotlinc inlines it: its body,
        // or the `$default` bridge for a call that omits arguments, is spliced with its own locals,
        // markers and line mapping, and the caller's coroutine transformation spills them.
        if !callable.inline.can_inline() || callable.suspend {
            return None;
        }
        let body_descriptor = callable.descriptor.as_str();
        let generic_signature = callable
            .generic_sig
            .as_deref()
            .map(|signature| (signature.receiver, signature.ret));
        let cache_input = crate::jvm::classpath::InlinePlanCacheInput {
            owner: callable.owner,
            name: callable.physical_name(),
            body_descriptor,
            physical_parameters: &callable.physical_params,
            context_count: callable.context_count,
            source_receiver: callable.source_receiver,
            semantic_parameters: &callable.params,
            semantic_result: callable.ret,
            suspend: callable.suspend,
            generic_signature,
            default_realization: callable.default_realization.as_deref(),
        };
        if let Some(plan) = self.cp.cached_inline_plan(cache_input) {
            return plan.map(|boxed| *boxed);
        }
        let parameter_slots = callable_parameter_slots(&callable.physical_params);
        let mut decode_unavailable = false;
        let plan = self.inline_body_plan_uncached(
            callable,
            body_descriptor,
            &parameter_slots,
            &mut decode_unavailable,
        );
        // Failed body or required metadata/member reads are transient. Do not globally cache one as
        // the stable declaration fact "this inline body has no recognized plan".
        if !decode_unavailable {
            self.cp
                .memoize_inline_plan(cache_input, plan.clone().map(Box::new));
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
        match self.inline_collection_transform_body_plan(callable, body_descriptor, parameter_slots)
        {
            collection_transform::CollectionTransformDecode::Plan(plan) => return Some(*plan),
            collection_transform::CollectionTransformDecode::Rejected => return None,
            collection_transform::CollectionTransformDecode::Unavailable => {
                *decode_unavailable = true;
                return None;
            }
            collection_transform::CollectionTransformDecode::NotRecognized => {}
        }
        if let Some(plan) = self.inline_iteration_body_plan(
            callable,
            body_descriptor,
            parameter_slots,
            decode_unavailable,
        ) {
            return Some(plan);
        }
        if *decode_unavailable {
            return None;
        }
        let Some(body) =
            self.cp
                .method_code_name(callable.owner, callable.physical_name(), body_descriptor)
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
            callable.params.get(lambda_parameter).copied().map(Ty::kind),
            Some(TyKind::Fun(lambda)) if lambda.params.len() == invoke_argument_slots.len()
        );
        if semantic_lambda_matches {
            if let Some(decoded) = recovery::decode(
                &instructions,
                &body.source_cp,
                &offsets,
                &body.handlers,
                *invoke,
                lambda_slot,
                invoke_argument_slots,
                parameter_slots,
            ) {
                return recovery::normalize(
                    self,
                    callable,
                    decoded,
                    lambda_parameter,
                    invoke_argument_slots,
                    parameter_slots,
                    decode_unavailable,
                );
            }
        }
        if exact_operands
            && exact_prelude
            && semantic_lambda_matches
            && handler_free_lambda_body(&body.handlers)
        {
            if let Some(return_parameter) = return_parameter {
                return Some(InlineBodyPlan::InvokeLambda {
                    frame: invoke_lambda_frame(callable),
                    lambda_parameter,
                    arguments: invoke_argument_slots
                        .iter()
                        .rev()
                        .map(|slot| parameter_at(*slot))
                        .map(|parameter| parameter.map(InlineBodyValue::Parameter))
                        .collect::<Option<Vec<_>>>()?,
                    prologue: Vec::new(),
                    cleanup: Vec::new(),
                    cause: None,
                    recovery: None,
                    defaults: Vec::new(),
                    result: return_parameter.map(InlineBodyValue::Parameter),
                });
            }
        }

        let cause_finally_shape = callable.context_count == 0
            && callable.params.len() == 2
            && callable.physical_params.len() == 2
            && parameter_slots.len() == 2
            && lambda_parameter == 1
            && invoke_argument_slots == [parameter_slots[0]]
            && callable
                .generic_sig
                .as_deref()
                .and_then(|signature| signature.receiver)
                .is_some()
            && matches!(
                callable.params.get(lambda_parameter).copied().map(Ty::kind),
                Some(TyKind::Fun(lambda))
                    if !lambda.suspend
                        && lambda.context_count == 0
                        && !lambda.has_receiver
                        && lambda.params.as_slice() == [callable.params[0]]
            );
        if cause_finally_shape {
            let first_local = slot_after_parameters(&callable.physical_params)?;
            if let Some((target, caught)) = recovery::decode_cause_finally_cleanup(
                &instructions,
                &body.source_cp,
                &offsets,
                &body.handlers,
                parameter_slots[0],
                lambda_slot,
                first_local,
                *invoke,
            ) {
                let cleanup = match self.inline_plan_static_extension(target) {
                    InlineDependency::Found(cleanup) => cleanup,
                    InlineDependency::Rejected => return None,
                    InlineDependency::Unavailable => {
                        *decode_unavailable = true;
                        return None;
                    }
                };
                if cleanup.context_count != 0
                    || cleanup.suspend
                    || cleanup.ret != Ty::Unit
                    || cleanup.params.len() != 2
                    || cleanup
                        .generic_sig
                        .as_deref()
                        .and_then(|signature| signature.receiver)
                        .is_none()
                {
                    return None;
                }
                return Some(InlineBodyPlan::InvokeLambda {
                    frame: invoke_lambda_frame(callable),
                    lambda_parameter,
                    arguments: vec![InlineBodyValue::Parameter(0)],
                    prologue: Vec::new(),
                    cleanup: vec![InlineBodyCall {
                        callable: Box::new(cleanup),
                        receiver: Some(InlineBodyCallReceiver::Extension(
                            InlineBodyValue::Parameter(0),
                        )),
                        arguments: vec![InlineBodyValue::Cause],
                    }],
                    cause: Some(Ty::nullable(Ty::obj(
                        crate::jvm::jvm_class_map::to_kotlin_internal(caught),
                    ))),
                    recovery: None,
                    defaults: Vec::new(),
                    result: None,
                });
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbol_source::SymbolNamespace;

    fn plain(op: u8) -> Insn {
        Insn::Plain {
            op,
            operands: Vec::new(),
        }
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
    fn closeable_use_requires_the_complete_cause_finally_template() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = JvmLibraries::new(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            vec![stdlib],
        )))
        .expect("JVM provider initialization");
        let symbols = libraries.symbols(SymbolNamespace::Package(type_name("kotlin/io")), "use");
        let [function] = symbols.callables.functions() else {
            panic!("kotlin.io.use must have exactly one metadata declaration")
        };
        let callable = &function.callable;
        assert_eq!(
            callable.descriptor,
            "(Ljava/io/Closeable;Lkotlin/jvm/functions/Function1;)Ljava/lang/Object;"
        );
        let Some(InlineBodyPlan::InvokeLambda {
            lambda_parameter: 1,
            arguments,
            prologue,
            cleanup,
            cause,
            recovery: None,
            defaults,
            result: None,
            ..
        }) = callable.inline_body_plan.as_deref()
        else {
            panic!("kotlin.io.use must publish its exact inline plan")
        };
        assert_eq!(arguments.as_slice(), [InlineBodyValue::Parameter(0)]);
        assert!(prologue.is_empty());
        assert_eq!(*cause, Some(Ty::nullable(Ty::obj("kotlin/Throwable"))));
        assert!(defaults.is_empty());
        let [cleanup] = cleanup.as_slice() else {
            panic!("kotlin.io.use must publish one cleanup call")
        };
        assert_eq!(cleanup.callable.owner, type_name("kotlin/io/CloseableKt"));
        assert_eq!(cleanup.callable.name, "closeFinally");
        assert_eq!(
            cleanup.callable.descriptor,
            "(Ljava/io/Closeable;Ljava/lang/Throwable;)V"
        );
        assert_eq!(cleanup.callable.params.len(), 2);
        assert_eq!(cleanup.callable.ret, Ty::Unit);
        assert!(!cleanup.callable.suspend);
        assert_eq!(
            cleanup.receiver,
            Some(InlineBodyCallReceiver::Extension(
                InlineBodyValue::Parameter(0),
            ))
        );
        assert_eq!(cleanup.arguments.as_slice(), [InlineBodyValue::Cause]);
        assert!(cleanup.callable.external_identity.is_some());
        let body = libraries
            .cp
            .method_code_name(callable.owner, "use", &callable.descriptor)
            .expect("stdlib must expose the selected use body");
        let instructions = inline::disassemble(&body.code).expect("valid use bytecode");
        let offsets = inline::insn_offsets_at(&instructions, 0);
        let parameter_slots = callable_parameter_slots(&callable.physical_params);
        let [receiver_slot, lambda_slot] = parameter_slots.as_slice() else {
            panic!("use must have receiver and lambda physical parameters")
        };
        let invoke_sites = inline::function_invoke_sites(&instructions, &body.source_cp);
        let [invoke] = invoke_sites.as_slice() else {
            panic!("use must contain exactly one lambda invocation")
        };
        let first_local = slot_after_parameters(&callable.physical_params)
            .expect("use local layout must fit in a JVM slot");
        assert_eq!(
            recovery::decode_cause_finally_cleanup(
                &instructions,
                &body.source_cp,
                &offsets,
                &body.handlers,
                *receiver_slot,
                *lambda_slot,
                first_local,
                *invoke,
            ),
            Some((
                (
                    "kotlin/io/CloseableKt",
                    "closeFinally",
                    "(Ljava/io/Closeable;Ljava/lang/Throwable;)V",
                    false,
                ),
                "java/lang/Throwable",
            ))
        );

        let mut extra_effect = instructions.clone();
        extra_effect[5] = plain(0x57);
        assert!(recovery::decode_cause_finally_cleanup(
            &extra_effect,
            &body.source_cp,
            &offsets,
            &body.handlers,
            *receiver_slot,
            *lambda_slot,
            first_local,
            *invoke,
        )
        .is_none());

        let mut wrong_handler = body.handlers.clone();
        wrong_handler[0].end_pc = wrong_handler[0].end_pc.saturating_add(1);
        assert!(recovery::decode_cause_finally_cleanup(
            &instructions,
            &body.source_cp,
            &offsets,
            &wrong_handler,
            *receiver_slot,
            *lambda_slot,
            first_local,
            *invoke,
        )
        .is_none());
    }

    fn only_callable(jars: Vec<std::path::PathBuf>, package: &str, name: &str) -> LibraryCallable {
        let libraries = JvmLibraries::new(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            jars,
        )))
        .expect("JVM provider initialization");
        let symbols = libraries.symbols(SymbolNamespace::Package(type_name(package)), name);
        let [function] = symbols.callables.functions() else {
            panic!("{package}.{name} must have exactly one metadata declaration")
        };
        function.callable.clone()
    }

    #[test]
    fn provider_decodes_plain_lambda_parameter_roles_exactly() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let callable = only_callable(vec![stdlib], "kotlin", "let");
        assert_eq!(
            callable.descriptor,
            "(Ljava/lang/Object;Lkotlin/jvm/functions/Function1;)Ljava/lang/Object;"
        );
        let Some(InlineBodyPlan::InvokeLambda {
            lambda_parameter: 1,
            arguments,
            prologue,
            cleanup,
            cause: None,
            recovery: None,
            defaults,
            result: None,
            ..
        }) = callable.inline_body_plan.as_deref()
        else {
            panic!("kotlin.let must publish its exact plain invocation plan")
        };
        assert_eq!(arguments.as_slice(), [InlineBodyValue::Parameter(0)]);
        assert!(prologue.is_empty());
        assert!(cleanup.is_empty());
        assert!(defaults.is_empty());
    }

    #[test]
    fn suspend_inline_functions_publish_no_structural_plan() {
        let (Some(stdlib), Some(coroutines)) = (
            crate::toolchain::stdlib_jar(),
            crate::toolchain::coroutines_jar(),
        ) else {
            return;
        };
        for name in ["withLock", "withPermit"] {
            let callable = only_callable(
                vec![stdlib.clone(), coroutines.clone()],
                "kotlinx/coroutines/sync",
                name,
            );
            assert!(callable.suspend);
            assert!(
                callable.inline_body_plan.is_none(),
                "{name} publishes a plan"
            );
        }
    }

    #[test]
    fn cached_nested_callables_are_registered_in_each_classpath() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let warm = JvmLibraries::new(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            vec![stdlib.clone()],
        )))
        .expect("JVM provider initialization");
        let warm_symbols = warm.symbols(SymbolNamespace::Package(type_name("kotlin/io")), "use");
        let [warm_function] = warm_symbols.callables.functions() else {
            panic!("warm classpath must decode one use declaration")
        };
        assert!(warm_function.callable.inline_body_plan.is_some());

        let current = JvmLibraries::new(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            vec![stdlib],
        )))
        .expect("JVM provider initialization");
        let symbols = current.symbols(SymbolNamespace::Package(type_name("kotlin/io")), "use");
        let [function] = symbols.callables.functions() else {
            panic!("second classpath must decode one use declaration")
        };
        let Some(InlineBodyPlan::InvokeLambda { cleanup, .. }) =
            function.callable.inline_body_plan.as_deref()
        else {
            panic!("cached use plan must remain present")
        };
        let [cleanup] = cleanup.as_slice() else {
            panic!("cached use plan must retain one cleanup")
        };
        let identity = cleanup
            .callable
            .external_identity
            .expect("cached nested call must receive a per-classpath identity");
        let realization = current
            .cp
            .external_callable(identity)
            .expect("nested identity must resolve in the consuming classpath");
        assert_eq!(
            realization.kind,
            crate::jvm::classpath::ExternalCallableKind::Extension
        );
        assert_eq!(
            realization.callable.owner,
            type_name("kotlin/io/CloseableKt")
        );
        assert_eq!(realization.callable.name, "closeFinally");
    }

    #[test]
    fn invoke_lambda_plan_rejects_an_exception_table() {
        assert!(handler_free_lambda_body(&[]));
        assert!(!handler_free_lambda_body(&valid_handlers()));
    }
}
