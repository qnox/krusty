//! Exact recognition of value-producing inline exception recovery.

use super::*;
use crate::libraries::InlineBodyRecovery;

/// Match the complete `try { lambda(receiver) } catch { cause = t; throw t } finally {
/// receiver.cleanup(cause) }` template emitted for `Closeable.use`. The cleanup is physically
/// static, but its semantic extension kind is recovered separately from Kotlin metadata.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_cause_finally_cleanup<'a>(
    instructions: &'a [Insn],
    source_cp: &'a [C],
    offsets: &[usize],
    handlers: &[ExcEntry],
    receiver_slot: u16,
    lambda_slot: u16,
    first_local_slot: u16,
    invoke: usize,
) -> Option<(MethodTarget<'a>, &'a str)> {
    let cause_slot = first_local_slot;
    let result_slot = cause_slot.checked_add(1)?;
    let exception_slot = result_slot.checked_add(1)?;
    if invoke != 8 || instructions.len() != 34 || handlers.len() != 4 {
        return None;
    }
    if !exact_parameter_null_check(&instructions[..3], source_cp, lambda_slot)
        || !exact_plain(&instructions[3], 0x01)
        || stored_reference_local(&instructions[4]) != Some(cause_slot)
        || !exact_plain(&instructions[5], 0x00)
        || loaded_reference_local(&instructions[6]) != Some(lambda_slot)
        || loaded_reference_local(&instructions[7]) != Some(receiver_slot)
        || stored_reference_local(&instructions[9]) != Some(result_slot)
        || !exact_marker_one(&instructions[10..12], source_cp, "finallyStart")
        || loaded_reference_local(&instructions[12]) != Some(receiver_slot)
        || loaded_reference_local(&instructions[13]) != Some(cause_slot)
        || !exact_marker_one(&instructions[15..17], source_cp, "finallyEnd")
        || loaded_reference_local(&instructions[17]) != Some(result_slot)
        || !exact_plain(&instructions[18], 0xb0)
        || stored_reference_local(&instructions[19]) != Some(exception_slot)
        || loaded_reference_local(&instructions[20]) != Some(exception_slot)
        || stored_reference_local(&instructions[21]) != Some(cause_slot)
        || loaded_reference_local(&instructions[22]) != Some(exception_slot)
        || !exact_plain(&instructions[23], 0xbf)
        || stored_reference_local(&instructions[24]) != Some(exception_slot)
        || !exact_marker_one(&instructions[25..27], source_cp, "finallyStart")
        || loaded_reference_local(&instructions[27]) != Some(receiver_slot)
        || loaded_reference_local(&instructions[28]) != Some(cause_slot)
        || !exact_marker_one(&instructions[30..32], source_cp, "finallyEnd")
        || loaded_reference_local(&instructions[32]) != Some(exception_slot)
        || !exact_plain(&instructions[33], 0xbf)
    {
        return None;
    }
    let cleanup = inline::invoked_method(&instructions[14], source_cp)?;
    let repeated = inline::invoked_method(&instructions[29], source_cp)?;
    if cleanup != repeated
        || !matches!(instructions[14], Insn::Plain { op: 0xb8, .. })
        || !matches!(instructions[29], Insn::Plain { op: 0xb8, .. })
    {
        return None;
    }
    let pc = |index: usize| u16::try_from(*offsets.get(index)?).ok();
    let expected = [
        (pc(5)?, pc(10)?, pc(19)?, Some("java/lang/Throwable")),
        (pc(5)?, pc(10)?, pc(24)?, None),
        (pc(19)?, pc(24)?, pc(24)?, None),
        (pc(24)?, pc(25)?, pc(24)?, None),
    ];
    let matches_handlers =
        handlers
            .iter()
            .zip(expected)
            .all(|(handler, (start, end, target, caught))| {
                handler.start_pc == start
                    && handler.end_pc == end
                    && handler.handler_pc == target
                    && inline::caught_class(source_cp, handler.catch_type) == caught
            });
    let caught = inline::caught_class(source_cp, handlers.first()?.catch_type)?;
    (matches_handlers && caught == "java/lang/Throwable").then_some((cleanup, caught))
}

pub(super) struct DecodedRecovery<'a> {
    pub(super) caught: &'a str,
    pub(super) constructor: MethodTarget<'a>,
    pub(super) failure: MethodTarget<'a>,
    pub(super) companion: (&'a str, &'a str, &'a str),
}

pub(super) fn normalize(
    libraries: &JvmLibraries,
    callable: &LibraryCallable,
    decoded: DecodedRecovery<'_>,
    lambda_parameter: usize,
    invoke_argument_slots: &[u16],
    parameter_slots: &[u16],
    decode_unavailable: &mut bool,
) -> Option<InlineBodyPlan> {
    let caught = Ty::obj(crate::jvm::jvm_class_map::to_kotlin_internal(
        decoded.caught,
    ));
    let (classifier, constructor) = libraries.inline_plan_constructor(decoded.constructor)?;
    let classifier_record = libraries.build_library_type(classifier)?;
    let (companion_field, companion_classifier) = classifier_record.companion_object.as_ref()?;
    let physical_companion_owner = type_name(decoded.companion.0);
    let semantic_companion_owner =
        crate::jvm::jvm_class_map::jvm_to_kotlin_builtin_metadata_name(physical_companion_owner)
            .unwrap_or(physical_companion_owner);
    let descriptor_classifier = decoded.companion.2.strip_prefix('L')?.strip_suffix(';')?;
    if callable.context_count != 0
        || callable.ret.non_null().obj_internal() != Some(classifier)
        || constructor.params.len() != 1
        || semantic_companion_owner != classifier
        || decoded.companion.1 != companion_field
        || type_name(descriptor_classifier) != *companion_classifier
    {
        return None;
    }
    let failure = match libraries.inline_plan_static_top_level(decoded.failure) {
        InlineDependency::Found(failure) => failure,
        InlineDependency::Rejected => return None,
        InlineDependency::Unavailable => {
            *decode_unavailable = true;
            return None;
        }
    };
    if failure.context_count != 0
        || failure.suspend
        || failure.params.as_slice() != [caught]
        || (failure.ret != constructor.params[0] && failure.ret != constructor.params[0].non_null())
    {
        return None;
    }
    let has_receiver = callable
        .generic_sig
        .as_deref()
        .is_some_and(|signature| signature.receiver.is_some());
    let arguments = if has_receiver {
        if lambda_parameter != 1
            || callable.params.len() != 2
            || invoke_argument_slots != [*parameter_slots.first()?]
        {
            return None;
        }
        vec![InlineBodyValue::Parameter(0)]
    } else {
        if lambda_parameter != 0 || callable.params.len() != 1 || !invoke_argument_slots.is_empty()
        {
            return None;
        }
        Vec::new()
    };
    Some(InlineBodyPlan::InvokeLambda {
        lambda_parameter,
        arguments,
        prologue: Vec::new(),
        cleanup: Vec::new(),
        cause: None,
        recovery: Some(Box::new(InlineBodyRecovery {
            caught,
            constructor: Box::new(constructor),
            failure: Box::new(InlineBodyCall {
                callable: Box::new(failure),
                receiver: None,
                arguments: vec![InlineBodyValue::Cause],
            }),
        })),
        defaults: Vec::new(),
        result: None,
    })
}

fn field_target<'a>(instruction: &Insn, source_cp: &'a [C]) -> Option<(&'a str, &'a str, &'a str)> {
    let Insn::Plain { op: 0xb2, operands } = instruction else {
        return None;
    };
    let index = u16::from_be_bytes([*operands.first()?, *operands.get(1)?]);
    let C::Fieldref(class, names) = source_cp.get(index as usize)? else {
        return None;
    };
    let C::Class(owner) = source_cp.get(*class as usize)? else {
        return None;
    };
    let C::Utf8(owner) = source_cp.get(*owner as usize)? else {
        return None;
    };
    let C::NameAndType(name, descriptor) = source_cp.get(*names as usize)? else {
        return None;
    };
    let (C::Utf8(name), C::Utf8(descriptor)) = (
        source_cp.get(*name as usize)?,
        source_cp.get(*descriptor as usize)?,
    ) else {
        return None;
    };
    Some((owner, name, descriptor))
}

/// Accept only the whole `try { Result(lambda()) } catch (t: E) { Result(failure(t)) }` template.
/// Every instruction, branch, local flow and exception-table edge is accounted for. The returned
/// physical targets are subsequently normalized through metadata before crossing the provider
/// boundary.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode<'a>(
    instructions: &[Insn],
    source_cp: &'a [C],
    offsets: &[usize],
    handlers: &[ExcEntry],
    invoke: usize,
    lambda_slot: u16,
    invoke_argument_slots: &[u16],
    parameter_slots: &[u16],
) -> Option<DecodedRecovery<'a>> {
    let result_slot = parameter_slots
        .last()
        .copied()
        .map_or(Some(0), |last| last.checked_add(1))?;
    let caught_slot = result_slot.checked_add(1)?;
    let join = invoke.checked_add(11)?;
    if instructions.len() != invoke.checked_add(13)?
        || invoke < 7
        || !exact_parameter_null_check(&instructions[..3], source_cp, lambda_slot)
        || !exact_plain(&instructions[3], 0x00)
        || !exact_plain(&instructions[5], 0x57)
        || stored_reference_local(&instructions[invoke + 2]) != Some(result_slot)
        || stored_reference_local(&instructions[invoke + 4]) != Some(caught_slot)
        || !exact_plain(&instructions[invoke + 6], 0x57)
        || loaded_reference_local(&instructions[invoke + 7]) != Some(caught_slot)
        || stored_reference_local(&instructions[invoke + 10]) != Some(result_slot)
        || loaded_reference_local(&instructions[join]) != Some(result_slot)
        || !exact_plain(&instructions[join + 1], 0xb0)
    {
        return None;
    }
    let invocation_loads = &instructions[6..invoke];
    if invocation_loads.len() != invoke_argument_slots.len() + 1
        || loaded_reference_local(&invocation_loads[0]) != Some(lambda_slot)
        || invocation_loads[1..]
            .iter()
            .zip(invoke_argument_slots.iter().rev())
            .any(|(load, slot)| loaded_reference_local(load) != Some(*slot))
    {
        return None;
    }
    let invocation = inline::invoked_method(&instructions[invoke], source_cp)?;
    let (invoke_parameters, invoke_result) =
        crate::jvm::names::parse_method_descriptor(invocation.2)?;
    let exact_interface_operands = matches!(
        &instructions[invoke],
        Insn::Plain { op: 0xb9, operands }
            if operands.len() == 4
                && operands[2] as usize == invoke_argument_slots.len() + 1
                && operands[3] == 0
    );
    if !invocation.3
        || !exact_interface_operands
        || invoke_parameters.len() != invoke_argument_slots.len()
        || invoke_result != "Ljava/lang/Object;"
    {
        return None;
    }
    let constructor = inline::invoked_method(&instructions[invoke + 1], source_cp)?;
    let failure = inline::invoked_method(&instructions[invoke + 8], source_cp)?;
    let repeated_constructor = inline::invoked_method(&instructions[invoke + 9], source_cp)?;
    if !matches!(instructions[invoke + 1], Insn::Plain { op: 0xb8, .. })
        || !matches!(instructions[invoke + 8], Insn::Plain { op: 0xb8, .. })
        || !matches!(instructions[invoke + 9], Insn::Plain { op: 0xb8, .. })
    {
        return None;
    }
    let companion = field_target(&instructions[4], source_cp)?;
    let repeated_companion = field_target(&instructions[invoke + 5], source_cp)?;
    let handler = handlers.first()?;
    let caught = inline::caught_class(source_cp, handler.catch_type)?;
    let (constructor_parameters, constructor_result) =
        crate::jvm::names::parse_method_descriptor(constructor.2)?;
    let (failure_parameters, failure_result) =
        crate::jvm::names::parse_method_descriptor(failure.2)?;
    let [constructor_payload] = constructor_parameters.as_slice() else {
        return None;
    };
    let [failure_caught] = failure_parameters.as_slice() else {
        return None;
    };
    if constructor.3
        || failure.3
        || !constructor_payload.starts_with('L')
        || !constructor_result.starts_with('L')
        || failure_result != *constructor_payload
        || failure_caught
            .strip_prefix('L')
            .and_then(|descriptor| descriptor.strip_suffix(';'))
            != Some(caught)
        || !companion.2.starts_with('L')
        || !companion.2.ends_with(';')
    {
        return None;
    }
    let pc = |index: usize| u16::try_from(*offsets.get(index)?).ok();
    let branch_target = match &instructions[invoke + 3] {
        Insn::Branch {
            op: 0xa7,
            target: inline::BranchTarget::Internal(target),
        }
        | Insn::BranchW {
            op: 0xc8,
            target: inline::BranchTarget::Internal(target),
        } => *target,
        _ => return None,
    };
    if handlers.len() != 1
        || handler.start_pc != pc(3)?
        || handler.end_pc != pc(invoke + 3)?
        || handler.handler_pc != pc(invoke + 4)?
        || branch_target != join
        || handler.catch_type == 0
        || constructor != repeated_constructor
        || companion != repeated_companion
    {
        return None;
    }
    Some(DecodedRecovery {
        caught,
        constructor,
        failure,
        companion,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbol_source::SymbolNamespace;

    struct DecoderFixture {
        instructions: Vec<Insn>,
        source_cp: Vec<C>,
        offsets: Vec<usize>,
        handlers: Vec<ExcEntry>,
        invoke: usize,
        lambda_slot: u16,
        parameter_slots: Vec<u16>,
    }

    fn decoder_fixture() -> DecoderFixture {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            panic!("runCatching decoder regression requires kotlin-stdlib")
        };
        let libraries = JvmLibraries::new(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            vec![stdlib],
        )));
        let symbols =
            libraries.symbols(SymbolNamespace::Package(type_name("kotlin")), "runCatching");
        let callable = symbols
            .callables
            .functions()
            .iter()
            .find(|function| function.kind == FnKind::TopLevel)
            .map(|function| &function.callable)
            .expect("stdlib must declare receiver-less runCatching");
        let owner = callable.owner.render();
        let body = libraries
            .cp
            .method_code(&owner, &callable.name, &callable.descriptor)
            .expect("runCatching must retain its declaration body");
        let instructions = inline::disassemble(&body.code).expect("disassemble runCatching");
        let invoke_sites = inline::function_invoke_sites(&instructions, &body.source_cp);
        let [invoke] = invoke_sites.as_slice() else {
            panic!("runCatching must contain exactly one lambda invocation")
        };
        let parameter_slots = callable_parameter_slots(&callable.physical_params);
        DecoderFixture {
            offsets: inline::insn_offsets_at(&instructions, 0),
            handlers: body.handlers,
            source_cp: body.source_cp,
            lambda_slot: parameter_slots[0],
            parameter_slots,
            instructions,
            invoke: *invoke,
        }
    }

    fn decoded(fixture: &DecoderFixture, instructions: &[Insn], handlers: &[ExcEntry]) -> bool {
        decode(
            instructions,
            &fixture.source_cp,
            &fixture.offsets,
            handlers,
            fixture.invoke,
            fixture.lambda_slot,
            &[],
            &fixture.parameter_slots,
        )
        .is_some()
    }

    #[test]
    fn actual_decoder_rejects_control_flow_target_and_stack_mutations() {
        let fixture = decoder_fixture();
        assert!(decoded(&fixture, &fixture.instructions, &fixture.handlers));

        for mutate in [
            |handler: &mut ExcEntry| handler.start_pc += 1,
            |handler: &mut ExcEntry| handler.end_pc += 1,
            |handler: &mut ExcEntry| handler.handler_pc += 1,
            |handler: &mut ExcEntry| handler.catch_type = 0,
        ] {
            let mut handlers = fixture.handlers.clone();
            mutate(&mut handlers[0]);
            assert!(!decoded(&fixture, &fixture.instructions, &handlers));
        }
        let mut handlers = fixture.handlers.clone();
        handlers.push(handlers[0].clone());
        assert!(!decoded(&fixture, &fixture.instructions, &handlers));

        let mut instructions = fixture.instructions.clone();
        instructions[fixture.invoke + 3] = Insn::Branch {
            op: 0xa7,
            target: inline::BranchTarget::Internal(fixture.invoke + 10),
        };
        assert!(!decoded(&fixture, &instructions, &fixture.handlers));

        for index in [fixture.invoke + 2, fixture.invoke + 4, fixture.invoke + 10] {
            let mut instructions = fixture.instructions.clone();
            let original = stored_reference_local(&instructions[index])
                .expect("recovery template stores a reference local");
            let wrong = original
                .checked_add(1)
                .expect("fixture local slot must fit");
            instructions[index] = Insn::Plain {
                op: 0x3a,
                operands: vec![u8::try_from(wrong).expect("fixture local slot must fit")],
            };
            assert!(!decoded(&fixture, &instructions, &fixture.handlers));
        }
        let mut instructions = fixture.instructions.clone();
        instructions[fixture.invoke + 7] = Insn::Plain {
            op: 0x2b,
            operands: Vec::new(),
        };
        assert!(!decoded(&fixture, &instructions, &fixture.handlers));

        let mut instructions = fixture.instructions.clone();
        instructions[fixture.invoke + 9] = instructions[fixture.invoke + 8].clone();
        assert!(!decoded(&fixture, &instructions, &fixture.handlers));

        let original_companion = field_target(&fixture.instructions[4], &fixture.source_cp)
            .expect("runCatching companion access");
        let alternate_field = fixture
            .source_cp
            .iter()
            .enumerate()
            .find_map(|(index, entry)| {
                matches!(entry, C::Fieldref(..))
                    .then(|| u16::try_from(index).ok())
                    .flatten()
                    .filter(|index| {
                        let instruction = Insn::Plain {
                            op: 0xb2,
                            operands: index.to_be_bytes().to_vec(),
                        };
                        field_target(&instruction, &fixture.source_cp)
                            .is_some_and(|field| field != original_companion)
                    })
            })
            .expect("runCatching pool must contain a distinct field target");
        let mut instructions = fixture.instructions.clone();
        instructions[fixture.invoke + 5] = Insn::Plain {
            op: 0xb2,
            operands: alternate_field.to_be_bytes().to_vec(),
        };
        assert!(!decoded(&fixture, &instructions, &fixture.handlers));

        let mut instructions = fixture.instructions.clone();
        instructions.swap(fixture.invoke + 8, fixture.invoke + 9);
        assert!(!decoded(&fixture, &instructions, &fixture.handlers));
        let mut instructions = fixture.instructions.clone();
        instructions[5] = Insn::Plain {
            op: 0x59,
            operands: Vec::new(),
        };
        assert!(!decoded(&fixture, &instructions, &fixture.handlers));
    }

    #[test]
    fn provider_publishes_run_catching_with_exact_dependency_identities() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            panic!("runCatching plan regression requires kotlin-stdlib")
        };
        let warm = JvmLibraries::new(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            vec![stdlib.clone()],
        )));
        let warm_symbols =
            warm.symbols(SymbolNamespace::Package(type_name("kotlin")), "runCatching");
        assert!(warm_symbols
            .callables
            .functions()
            .iter()
            .any(|function| function.callable.inline_body_plan.is_some()));

        let libraries = JvmLibraries::new(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            vec![stdlib],
        )));
        let symbols =
            libraries.symbols(SymbolNamespace::Package(type_name("kotlin")), "runCatching");
        let callable = symbols
            .callables
            .functions()
            .iter()
            .find(|function| function.kind == FnKind::TopLevel)
            .map(|function| &function.callable)
            .expect("stdlib must declare receiver-less runCatching");
        let Some(InlineBodyPlan::InvokeLambda {
            lambda_parameter: 0,
            arguments,
            prologue,
            cleanup,
            cause: None,
            recovery: Some(recovery),
            defaults,
            result: None,
        }) = callable.inline_body_plan.as_deref()
        else {
            panic!("kotlin.runCatching must publish its exact recovery plan")
        };
        assert!(arguments.is_empty());
        assert!(prologue.is_empty());
        assert!(cleanup.is_empty());
        assert!(defaults.is_empty());
        assert_eq!(recovery.caught, Ty::obj("kotlin/Throwable"));
        assert_eq!(recovery.constructor.params.len(), 1);
        assert_eq!(recovery.failure.arguments, [InlineBodyValue::Cause]);
        for identity in [
            recovery.constructor.external_identity,
            recovery.failure.callable.external_identity,
        ] {
            let identity = identity.expect("cached dependency must receive a current identity");
            let realization = libraries
                .cp
                .external_callable(identity)
                .expect("cached dependency identity must resolve in the current classpath");
            assert_eq!(realization.callable.external_identity, Some(identity));
        }
    }
}
