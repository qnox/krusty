//! JVM emission of bridge methods.
//!
//! A bridge exists because a supertype's erased signature differs from the override's. Everything
//! that adapts the two — the type-safe barrier, the argument checkcast/unbox/convert, the delegated
//! call, and the RETURN adapter — lives here rather than in the emitter facade, so the physical
//! boxing and carrier decisions stay in one JVM-owned place.

use super::{
    box_prim_free, discard, emit_num_conv, emit_return, emit_value_class_box_adapter,
    emit_value_class_unbox_adapter, finish_code, finish_code_sig, ir_ty_to_jvm, jvm_declared_ty,
    jvm_function_params, jvm_tys, jvm_value_ty, load, local_variable_desc, method_descriptor,
    slot_words, throw_assertion_error, type_descriptor, unbox_prim_from, verif_for_jvm_free,
    ClassWriter, CodeBuilder, EmitEnv, EmitRun, JvmSignatureFormatter, VerifType,
};
use crate::ir::IrFile;
use crate::types::Ty;

fn finish_bridge(
    cw: &mut ClassWriter,
    desc: &str,
    code: &mut CodeBuilder,
    locals: u16,
    bridge: &crate::ir::Bridge,
    packs_arguments: bool,
    entry: Option<&EntryHeader>,
) {
    let name = bridge.name.as_str();
    if let Some(entry) = entry {
        finish_code_sig::<0x0001>(cw, name, desc, code, locals, entry.signature.as_deref());
        cw.set_method_parameters(name, desc, &entry.reflected);
    } else if packs_arguments {
        finish_code::<{ 0x0001 | 0x0010 | 0x0040 | 0x1000 }>(cw, name, desc, code, locals);
    } else if bridge.special {
        finish_code::<{ 0x0001 | 0x0010 | 0x0040 }>(cw, name, desc, code, locals);
    } else {
        finish_code::<{ 0x0001 | 0x0040 | 0x1000 }>(cw, name, desc, code, locals);
    }
}

fn barrier_instanceof_name(semantic: Ty, jvm: Ty) -> String {
    semantic.jvm_boxed_ref().map_or_else(
        || crate::jvm::names::instanceof_internal_name(jvm),
        crate::jvm::names::instanceof_internal_name,
    )
}

pub(super) fn emit_barrier_outcome(
    outcome: crate::jvm::collection_barriers::BarrierOutcome,
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
) {
    match outcome {
        crate::jvm::collection_barriers::BarrierOutcome::False => {
            code.push_int(0, cw);
            code.ireturn();
        }
        crate::jvm::collection_barriers::BarrierOutcome::NotFound => {
            code.push_int(-1, cw);
            code.ireturn();
        }
        crate::jvm::collection_barriers::BarrierOutcome::Null => {
            code.aconst_null();
            code.areturn();
        }
    }
}

/// An interface entry reads as its member does: the member's declaration line once its parameter
/// checks are done, and `this` with the member's own parameter names (its carrier is `this` here).
fn attach_interface_entry_debug_tables(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    cw: &mut ClassWriter,
    bridge: &crate::ir::Bridge,
    descriptor: &str,
    body_pc: u16,
) {
    let Some(member) = bridge.target_function else {
        return;
    };
    let names =
        crate::jvm::parameter_names::function_locals(ir, member, &jvm_function_params(ir, member))
            .unwrap_or_default();
    let mut locals = vec![(String::from("this"), format!("L{};", c.fq_name()), 0u16)];
    let mut slot = 1u16;
    for (index, parameter) in jvm_tys(&bridge.erased_params).iter().enumerate() {
        // The entry is the declared member itself on the box, so its receiver local is named after
        // the entry's own (physical) name, as for any member extension.
        let name = if is_extension_receiver(bridge, index) {
            Some(crate::jvm::parameter_names::value_class_interface_entry_receiver(&bridge.name))
        } else {
            names.get(index + 1).cloned().flatten()
        };
        if let Some(name) = name {
            locals.push((name, local_variable_desc(*parameter), slot));
        }
        slot += slot_words(*parameter);
    }
    let line = ir.fn_decl_lines.get(&member).map(|&line| (body_pc, line));
    cw.set_method_debug(&bridge.name, descriptor, line, &locals);
}

fn is_extension_receiver(bridge: &crate::ir::Bridge, index: usize) -> bool {
    matches!(
        bridge
            .parameters
            .get(index)
            .map(|parameter| &parameter.identity),
        Some(crate::fir::ResolvedParameterIdentity::ExtensionReceiver)
    )
}

/// A value class's interface entry is an ordinary method, and kotlinc guards its parameters exactly
/// as the static member it calls guards them (that member's first parameter is the carrier).
fn emit_interface_entry_checks(
    ir: &IrFile,
    b: &crate::ir::Bridge,
    parameters: &[Ty],
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
) {
    let Some(member) = b.target_function else {
        return;
    };
    let checks = &ir.functions[member as usize].param_checks;
    let names = crate::jvm::parameter_names::function_assertions(
        ir,
        member,
        &jvm_function_params(ir, member),
    );
    let mut slot = 1u16;
    for (index, parameter) in parameters.iter().enumerate() {
        if checks.get(index + 1).is_some_and(Option::is_some) {
            // Unlike the static replacement, the entry keeps its extension receiver, quoted `<this>`.
            let name = if is_extension_receiver(b, index) {
                String::from("<this>")
            } else {
                names
                    .as_ref()
                    .and_then(|names| names.get(index + 1))
                    .and_then(Clone::clone)
                    .expect("a checked parameter carries an assertion spelling")
            };
            code.aload(slot);
            code.push_string(&name, cw);
            let check = cw.methodref(
                "kotlin/jvm/internal/Intrinsics",
                "checkNotNullParameter",
                "(Ljava/lang/Object;Ljava/lang/String;)V",
            );
            code.invokestatic(check, 2, 0);
        }
        slot += slot_words(*parameter);
    }
}

/// Emit `ACC_BRIDGE|ACC_SYNTHETIC` methods: each has the supertype's erased descriptor, adapts its
/// arguments (type barrier / checkcast / unbox / numeric convert), delegates to the concrete override,
/// and adapts the return value back (box / numeric convert).
pub(super) fn emit_bridges(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    cw: &mut ClassWriter,
    env: &EmitEnv<'_>,
) {
    let class = ClassBridges::new(ir, c, env);
    for (bridge_index, b) in c.bridges.iter().enumerate() {
        // An interface entry stands beside its static member, which emits it.
        if b.kind != crate::ir::BridgeKind::ValueClassInterfaceEntry {
            emit_bridge(&class, cw, bridge_index, b, None);
        }
    }
    for bridge in env.inherited_collection_bridges.for_class(c.fq_name_id()) {
        emit_inherited_collection_bridge(ir, c, cw, bridge);
    }
}

/// The class whose bridges are being emitted, with the run's bridge facts.
#[derive(Clone, Copy)]
struct ClassBridges<'a> {
    ir: &'a IrFile,
    class: &'a crate::ir::IrClass,
    adaptations: &'a crate::jvm::bridge_adaptations::BridgeAdaptations,
    argument_arrays: &'a crate::jvm::function_argument_arrays::FunctionArgumentArrays,
    override_results: &'a crate::jvm::override_results::OverrideResults,
    run: &'a EmitRun,
}

impl<'a> ClassBridges<'a> {
    fn new(ir: &'a IrFile, class: &'a crate::ir::IrClass, env: &EmitEnv<'a>) -> Self {
        Self {
            ir,
            class,
            adaptations: env.bridge_adaptations,
            argument_arrays: env.function_argument_arrays,
            override_results: env.override_results,
            run: env.run,
        }
    }
}

/// Emit one bridge: see [`emit_bridges`].
fn emit_bridge(
    class: &ClassBridges<'_>,
    cw: &mut ClassWriter,
    bridge_index: usize,
    b: &crate::ir::Bridge,
    entry: Option<&EntryHeader>,
) {
    let ClassBridges {
        ir,
        class: c,
        adaptations,
        argument_arrays,
        override_results,
        run,
    } = *class;
    let packs_arguments = argument_arrays.packs(c.fq_name_id(), b);
    let adapter = adaptations.get(
        c.fq_name_id(),
        u32::try_from(bridge_index).expect("bridge ordinals fit u32"),
    );
    let result_adapter = adapter.and_then(|adapter| adapter.result);
    let return_unboxing = match result_adapter {
        Some(crate::jvm::bridge_adaptations::BridgeResultAdapter::Unbox(plan)) => Some(plan),
        _ => None,
    };
    let ep = jvm_tys(&b.erased_params);
    let static_target = b.target_function.and_then(|function| {
        let target = ir.functions.get(function as usize)?;
        (target.is_static && target.dispatch_receiver == Some(c.fq_name))
            .then_some((function, target))
    });
    let target_parameters = static_target.map(|(function, _)| jvm_function_params(ir, function));
    let cp = target_parameters
        .as_deref()
        .and_then(|parameters| parameters.get(1..))
        .map_or_else(|| jvm_tys(&b.concrete_params), <[Ty]>::to_vec);
    let er = ir_ty_to_jvm(&b.erased_ret);
    let cr = ir_ty_to_jvm(&b.concrete_ret);
    // The target is a declaration, so its result is spelled as declared (`Nothing?` is `Void`).
    let tr = static_target.map_or_else(
        || jvm_declared_ty(&b.target_ret.unwrap_or(b.concrete_ret)),
        |(function, _)| jvm_declared_ty(&override_results.physical_result(ir, function)),
    );
    let erased_desc = method_descriptor(&ep, er);
    // A bridge whose (name, descriptor) already names a REAL method on this class would be a
    // duplicate (`ClassFormatError`) — e.g. an interface getter `getX()T` overridden with the SAME
    // type differs from the impl only by a spurious nullability/representation detail. Skip it; the
    // real method already satisfies the interface. (Real methods are emitted before `emit_bridges`.)
    if cw.has_method(&b.name, &erased_desc) {
        return;
    }
    let pw: u16 = ep.iter().map(|t| slot_words(*t)).sum();
    let mut code = CodeBuilder::new(1 + pw);
    if let Some(header) = entry {
        cw.reserve_method_pool_with_annotations(
            &b.name,
            &erased_desc,
            header.signature.as_deref(),
            &header.annotation_types(),
            &crate::ir::DeclarationAnnotations::default(),
            &header.reflected,
        );
    }
    if b.kind == crate::ir::BridgeKind::ValueClassInterfaceEntry {
        emit_interface_entry_checks(ir, b, &ep, cw, &mut code);
    }
    let body_pc = u16::try_from(code.bytes.len()).expect("parameter checks fit a method");
    if let Some(barrier) = b.barrier_plan {
        let dispatch = code.new_label();
        let parameter_slot = 1 + ep[..barrier.parameter]
            .iter()
            .map(|ty| slot_words(*ty))
            .sum::<u16>();
        if b.concrete_params[barrier.parameter].is_nullable() {
            code.aload(parameter_slot);
            code.ifnull(dispatch);
        }
        code.aload(parameter_slot);
        // A primitive parameter's JVM type is the scalar. `instanceof` names its boxed wrapper
        // (`Int` → `java/lang/Integer`); the unbox below still goes through `Number`. A value-class
        // parameter arrives as that class's box. The unbox adapter names the box; the carrier's
        // wrapper (`Integer`) is a different class and would reject a real key.
        let concrete = adapter
            .and_then(|plan| plan.parameter(barrier.parameter))
            .map(|value_class| value_class.owner.render())
            .unwrap_or_else(|| {
                barrier_instanceof_name(b.concrete_params[barrier.parameter], cp[barrier.parameter])
            });
        let concrete_class = cw.class_ref(&concrete);
        code.instance_of(concrete_class);
        code.ifne(dispatch);
        emit_barrier_outcome(barrier.outcome, cw, &mut code);
        let mut locals = vec![VerifType::ObjectName(c.fq_name())];
        locals.extend(ep.iter().map(|ty| verif_for_jvm_free(cw, *ty)));
        code.bind(dispatch);
    }
    if packs_arguments {
        super::function_reference_invoke::check_argument_count(cw, &mut code, cp.len());
    }
    if let Some(parameters) = &target_parameters {
        assert!(
            c.is_value,
            "only a value-class member may become a static bridge target"
        );
        let receiver = *parameters
            .first()
            .expect("a static value-class member target must carry its receiver");
        let field = c
            .fields
            .first()
            .expect("a value-class bridge owner must have an underlying field");
        let field_ty = jvm_value_ty(&field.ty);
        assert_eq!(
            field_ty, receiver,
            "a static value-class member receiver must use its field carrier"
        );
        code.aload(0);
        let field_ref = cw.fieldref(&c.fq_name(), &field.name, &type_descriptor(field_ty));
        code.getfield(field_ref, slot_words(receiver) as i32);
    } else {
        code.aload(0);
    }
    let erased_arguments = if packs_arguments {
        vec![Ty::obj("java/lang/Object"); cp.len()]
    } else {
        ep.clone()
    };
    let mut slot = 1u16;
    for (k, (et, ct)) in erased_arguments.iter().zip(&cp).enumerate() {
        if packs_arguments {
            super::function_reference_invoke::load_erased_function_argument(cw, &mut code, true, k);
        } else {
            load(*et, slot, &mut code);
            slot += slot_words(*et);
        }
        // A boxed value-class param (a generic supertype method `f(Object,…)` delegating to a mangled
        // concrete override taking the underlying): checkcast the incoming `Object` to the boxed `X`,
        // then `unbox-impl` it to the underlying `ct` the target expects.
        // A carrier that holds null (`X?` over a non-null reference) takes a null past `unbox-impl`.
        if let Some(plan) = adapter.and_then(|adapter| adapter.parameter(k)) {
            emit_value_class_unbox_adapter(cw, &mut code, plan.owner, *ct, plan.null_preserving);
        } else if et != ct {
            if et.is_reference() && ct.is_reference() {
                let ci = cw.class_ref(&crate::jvm::names::instanceof_internal_name(*ct));
                code.checkcast(ci);
            } else if et.is_reference() && ct.is_jvm_scalar() {
                unbox_prim_from(cw, &mut code, *et, *ct);
            } else if et.is_jvm_scalar() && ct.is_reference() {
                // The erased slot is a PRIMITIVE but the concrete override takes the generic
                // reference (`B.foo(int)` bridged onto `foo(t: T)="…"` erased to `Object`):
                // box the scalar before delegating, exactly as kotlinc's bridge does.
                box_prim_free(cw, &mut code, *et);
            } else if et.is_jvm_scalar() && ct.is_jvm_scalar() {
                emit_num_conv(*et, *ct, &mut code);
            }
        }
    }
    let argw: i32 = cp.iter().map(|t| slot_words(*t) as i32).sum();
    // A value-class boxing bridge calls the mangled override (`target_name`) which returns the
    // erased underlying, then boxes the result back to `X` with `X.box-impl`.
    let target = b.target_name.as_deref().unwrap_or(&b.name);
    let owner = c.fq_name();
    let target_desc = target_parameters.as_deref().map_or_else(
        || method_descriptor(&cp, tr),
        |parameters| method_descriptor(parameters, tr),
    );
    let m = cw.methodref(&owner, target, &target_desc);
    if let Some(parameters) = &target_parameters {
        let static_argw = parameters
            .iter()
            .map(|parameter| slot_words(*parameter) as i32)
            .sum();
        code.invokestatic(m, static_argw, slot_words(tr) as i32);
    } else {
        code.invokevirtual(m, argw, slot_words(tr) as i32);
    }
    if b.target_ret.is_some() && tr != cr {
        // A CPS target returns Object while this bridge must recover a reference value-class
        // carrier before `box-impl`. Scalar carriers arrive already boxed and never take this path.
        debug_assert!(tr.is_reference() && cr.is_reference());
        let carrier = cw.class_ref(&crate::jvm::names::instanceof_internal_name(cr));
        code.checkcast(carrier);
    }
    // A supertype that spells a value class unboxed takes its carrier out of the result even when
    // the override returns `Nothing`: kotlinc casts the `Void` and unboxes it like any other.
    let unboxes_result = return_unboxing.is_some() && tr.is_reference();
    if !unboxes_result
        && cr.is_reference()
        && crate::jvm::names::instanceof_internal_name(cr) == "java/lang/Void"
        && !er.is_reference()
    {
        // A `Nothing` override may have a `java/lang/Void` descriptor while the value-class
        // supertype bridge returns the unboxed primitive. The target must diverge; if it ever
        // falls through, discard the null-only Void result and throw to keep the bridge verifiable.
        code.pop();
        throw_assertion_error(cw, &mut code);
        finish_bridge(
            cw,
            &erased_desc,
            &mut code,
            1 + pw,
            b,
            packs_arguments,
            entry,
        );
        attach_bridge_debug_tables(ir, c, cw, b, packs_arguments, &erased_desc, body_pc);
        return;
    }
    if b.concrete_ret == Ty::Nothing && !unboxes_result {
        // Kotlin `Nothing` methods must not fall through. If the concrete descriptor still leaves a
        // physical carrier value, discard it before throwing so the assertion path starts with a clean
        // stack for every bridge return representation.
        if cr == Ty::Nothing {
            code.pop();
        } else {
            discard(cr, &mut code);
        }
        throw_assertion_error(cw, &mut code);
        finish_bridge(
            cw,
            &erased_desc,
            &mut code,
            1 + pw,
            b,
            packs_arguments,
            entry,
        );
        attach_bridge_debug_tables(ir, c, cw, b, packs_arguments, &erased_desc, body_pc);
        return;
    }
    if let Some(crate::jvm::bridge_adaptations::BridgeResultAdapter::Box(plan)) = result_adapter {
        emit_value_class_box_adapter(cw, &mut code, plan.owner, cr, plan.null_preserving);
    } else if let Some(plan) = return_unboxing.filter(|_| unboxes_result) {
        // The supertype declares a VALUE CLASS in its UNBOXED form, so this bridge returns that
        // class's carrier and the carrier comes out of the class's own `unbox-impl` — whether
        // the carrier is a scalar (`()I`) or a REFERENCE (`()Ljava/lang/String;`). Keying on the
        // carrier alone gets both wrong: a reference carrier takes the plain `Object`-to-`String`
        // narrowing below and never unboxes at all, and a scalar one reaches a JVM wrapper the
        // boxed value class is not an instance of. Nor may the two JVM types deciding it: an
        // `Any`-carrier value class has `Object` on both sides of the boundary and still has to
        // be unboxed, so the PLAN decides, not a type comparison.
        let owner = plan.owner.render();
        let ci = cw.class_ref(&owner);
        code.checkcast(ci);
        let unbox = cw.methodref(&owner, "unbox-impl", &format!("(){}", type_descriptor(er)));
        if plan.null_preserving {
            // `unbox-impl` is an instance call, so a legal null would throw on the way to a
            // declaration that says this bridge returns null. kotlinc branches around it.
            let mut locals = vec![VerifType::ObjectName(c.fq_name())];
            locals.extend(ep.iter().map(|ty| verif_for_jvm_free(cw, *ty)));
            let null_case = code.new_label();
            let done = code.new_label();
            code.dup();
            // At the branch target the DUPLICATE is still on the stack — the boxed value class,
            // not the carrier it would have unboxed to.
            code.ifnull(null_case);
            code.invokevirtual(unbox, 0, slot_words(er) as i32);
            code.goto(done);
            code.bind(null_case);
            code.pop();
            code.aconst_null();
            code.bind(done);
        } else {
            code.invokevirtual(unbox, 0, slot_words(er) as i32);
        }
    } else if cr != er {
        if er.is_reference() && cr.is_jvm_scalar() {
            box_prim_free(cw, &mut code, cr);
        } else if er.is_jvm_scalar() && cr.is_jvm_scalar() {
            emit_num_conv(cr, er, &mut code);
        } else if cr == Ty::Unit && er.is_reference() {
            // A `Unit`-returning override bridged to a reference-returning supertype method
            // (`B.foo(): Unit` over `A.foo(): Any`): the JVM call is void, so materialize the
            // `kotlin/Unit` singleton the erased bridge must return.
            let f = cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
            code.getstatic(f, 1);
        } else if er.is_jvm_scalar() && cr.is_reference() {
            // The delegated override hands back the ERASED generic REFERENCE while the
            // supertype this bridge serves declares the PRIMITIVE — `interface C { val size: Int }`
            // over `open class A<T> { var size: T }`, where the concrete `getSize()` is
            // `()Ljava/lang/Object;` and this bridge's descriptor is `()I`. Nothing unboxed it:
            // the bridge pushed the reference and emitted `ireturn`, an artifact the verifier
            // rejects at the return with no diagnostic from the compiler. This is the inverse of
            // the `er.is_reference() && cr.is_jvm_scalar()` box above, and it was simply absent.
            // A native unsigned value class must have taken the typed plan above: its boxed
            // wrapper is not a signed primitive wrapper or `Number`, and guessing here would
            // emit a method with the wrong ABI. Refuse the file if the realization was lost.
            if b.erased_ret.non_null().is_unsigned() {
                run.set_emit_error(format!(
                    "missing JVM bridge return adaptation for {}.{}",
                    c.fq_name(),
                    b.name
                ));
                return;
            }
            unbox_prim_from(cw, &mut code, cr, er);
        } else if er.is_reference()
            && !er.is_array()
            && cr.is_reference()
            && !crate::jvm::names::same_type_descriptor(cr, er)
            && crate::jvm::names::instanceof_internal_name(er) != "java/lang/Object"
        {
            // kotlinc's bridge materializes the delegated result at its own return type, and
            // `StackValue.coerce` casts between two different reference types unless the target is
            // `Object`: it does not consult the class hierarchy. So a `String` override bridged to
            // a `CharSequence` declaration is cast, as is the erased `Object` of a generic getter
            // bridged to a narrower declaration and `Nothing?`'s `Void` bridged to a box. A bridge
            // returning `Object` or an array (`Array<String>` over `Array<out Any>`) takes the
            // result as it is.
            let ci = cw.class_ref(&crate::jvm::names::instanceof_internal_name(er));
            code.checkcast(ci);
        }
    }
    emit_return(er, &mut code);
    finish_bridge(
        cw,
        &erased_desc,
        &mut code,
        1 + pw,
        b,
        packs_arguments,
        entry,
    );
    attach_bridge_debug_tables(ir, c, cw, b, packs_arguments, &erased_desc, body_pc);
}

/// kotlinc gives every bridge a `LineNumberTable` rooted at the CLASS declaration and a
/// `LocalVariableTable` naming its receiver and parameters.
///
/// The bridge carries the signature of the supertype declaration it overrides, so the NAMES come
/// from that declaration, while the descriptors are the ERASED ones the bridge actually receives
/// (`item Ljava/lang/Object;`, not `String`). A parameter the declaration does not name keeps the
/// JVM's positional spelling. A property-setter
/// bridge has no source function identity; its generated parameter uses kotlinc's accessor spelling.
/// Attached as each bridge is written, not in a pass afterwards: the local-variable table's
/// strings are interned when they are recorded, and kotlinc interns them with the method they
/// belong to. Deferring the whole set moved `Ljava/lang/Object;` past the next bridge's descriptor.
fn attach_bridge_debug_tables(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    cw: &mut ClassWriter,
    bridge: &crate::ir::Bridge,
    packs_arguments: bool,
    erased_desc: &str,
    body_pc: u16,
) {
    if bridge.kind == crate::ir::BridgeKind::ValueClassInterfaceEntry {
        attach_interface_entry_debug_tables(ir, c, cw, bridge, erased_desc, body_pc);
        return;
    }
    if packs_arguments {
        // kotlinc maps no line of the array bridge and names its one parameter `args`.
        let locals = vec![
            (String::from("this"), format!("L{};", c.fq_name()), 0u16),
            (
                String::from("args"),
                String::from("[Ljava/lang/Object;"),
                1u16,
            ),
        ];
        cw.set_method_debug(&bridge.name, erased_desc, None, &locals);
        return;
    }
    if c.decl_line == 0 {
        return;
    }
    // Where the DECLARATION starts, annotations included — the same line the primary constructor's
    // `super()` maps to. The two coincide unless an annotation sits on its own line above the
    // header, which is exactly the shape a `@Serializable` class has.
    let line = if c.decl_start_line == 0 {
        c.decl_line
    } else {
        c.decl_start_line
    };
    let this_desc = format!("L{};", c.fq_name());
    {
        assert_eq!(
            bridge.parameters.len(),
            bridge.erased_params.len(),
            "bridge debug parameters exactly match physical arity"
        );
        let mut locals = vec![(String::from("this"), this_desc.clone(), 0u16)];
        let mut slot = 1u16;
        // Named and labelled after the overridden declaration the bridge's signature comes from.
        let (identities, semantic_types): (Vec<_>, Vec<_>) = bridge
            .parameters
            .iter()
            .map(|parameter| (parameter.identity.clone(), parameter.semantic))
            .unzip();
        let parameter_names = crate::jvm::parameter_names::bridge_local_variables(
            &identities,
            &semantic_types,
            &bridge.name,
        );
        for (parameter, spelling) in jvm_tys(&bridge.erased_params).iter().zip(parameter_names) {
            let descriptor = local_variable_desc(*parameter);
            if let Some(spelling) = spelling {
                locals.push((spelling, descriptor, slot));
            }
            slot += slot_words(*parameter);
        }
        cw.set_method_debug(&bridge.name, erased_desc, Some((0, line)), &locals);
    }
}

/// What an interface entry declares beyond its descriptor: its member's generic `Signature`,
/// nullability annotations and, under `-java-parameters`, `MethodParameters`, less the carrier the
/// member receives first.
struct EntryHeader {
    signature: Option<String>,
    result: Option<&'static str>,
    parameters: Vec<Option<&'static str>>,
    reflected: Vec<crate::jvm::method_parameters::MethodParameter>,
}

impl EntryHeader {
    fn of(
        ir: &IrFile,
        formatter: &JvmSignatureFormatter<'_>,
        member: u32,
        entry: &crate::ir::Bridge,
        descriptor: &str,
        java_parameters: bool,
    ) -> Self {
        let function = &ir.functions[member as usize];
        let signature = ir.signatures.get(&member).and_then(|generic| {
            let mut generic =
                super::value_class_signatures::physical_generic_signature(ir, member, generic)
                    .into_owned();
            assert_eq!(
                generic.params.len(),
                function.params.len(),
                "a lowered value-class member signs its carrier"
            );
            generic.params.remove(0);
            // The carrier occupied signed slot 0, so a later vararg keeps declaration-site
            // variance at its shifted index.
            let vararg_index = ir
                .fn_varargs
                .get(&member)
                .and_then(|vararg| vararg.index.checked_sub(1));
            formatter
                .method_signature(&generic, function, vararg_index)
                .filter(|signature| signature != descriptor)
        });
        let nullability = super::declared_nullability::declared_nullability(ir, member);
        let reflected = if java_parameters {
            crate::jvm::method_parameters::value_class_interface_entry(
                ir,
                member,
                &jvm_function_params(ir, member),
                &entry.name,
                entry.erased_params.len(),
            )
        } else {
            Vec::new()
        };
        Self {
            signature,
            result: nullability.result,
            parameters: nullability.parameters.get(1..).unwrap_or_default().to_vec(),
            reflected,
        }
    }

    /// The annotation types in kotlinc's header order: the result's, then each parameter's.
    fn annotation_types(&self) -> Vec<&'static str> {
        self.result
            .into_iter()
            .chain(self.parameters.iter().copied().flatten())
            .collect()
    }
}

/// Emit the interface entries standing for the value-class `member` on its box, right after the
/// member: kotlinc keeps each entry beside the static replacement it calls.
pub(super) fn emit_value_class_interface_entries(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    cw: &mut ClassWriter,
    member: u32,
    formatter: &JvmSignatureFormatter<'_>,
    env: &EmitEnv<'_>,
) {
    for (bridge_index, b) in c.bridges.iter().enumerate() {
        if b.kind != crate::ir::BridgeKind::ValueClassInterfaceEntry
            || b.target_function != Some(member)
        {
            continue;
        }
        let descriptor = method_descriptor(&jvm_tys(&b.erased_params), ir_ty_to_jvm(&b.erased_ret));
        let header = EntryHeader::of(ir, formatter, member, b, &descriptor, env.java_parameters);
        let class = ClassBridges::new(ir, c, env);
        emit_bridge(&class, cw, bridge_index, b, Some(&header));
        cw.set_method_nullability(&b.name, &descriptor, header.result, &header.parameters);
    }
}

fn emit_inherited_collection_bridge(
    ir: &IrFile,
    class: &crate::ir::IrClass,
    cw: &mut ClassWriter,
    bridge: &crate::jvm::inherited_collection_bridges::InheritedCollectionBridge,
) {
    let params = jvm_tys(&bridge.parameters);
    let ret = ir_ty_to_jvm(&bridge.result);
    let descriptor = method_descriptor(&params, ret);
    if cw.has_method(&bridge.name, &descriptor) {
        return;
    }
    let words: u16 = params.iter().map(|ty| slot_words(*ty)).sum();
    let mut code = CodeBuilder::new(1 + words);
    let signature = match &bridge.body {
        crate::jvm::inherited_collection_bridges::InheritedCollectionBridgeBody::SuperDelegate {
            owner,
            name,
            descriptor: super_descriptor,
            result_cast,
            signature,
        } => {
            code.aload(0);
            let mut slot = 1u16;
            for ty in &params {
                load(*ty, slot, &mut code);
                slot += slot_words(*ty);
            }
            let arg_words: i32 = params.iter().map(|ty| i32::from(slot_words(*ty))).sum();
            let owner = owner.render();
            let method = cw.methodref(&owner, name, super_descriptor);
            code.invokespecial(method, arg_words, i32::from(slot_words(ret)));
            if let Some(cast) = result_cast {
                code.checkcast(cw.class_ref(cast));
            }
            emit_return(ret, &mut code);
            signature.as_deref()
        }
        crate::jvm::inherited_collection_bridges::InheritedCollectionBridgeBody::Checked {
            synthetic: _,
            checks,
            failure,
            delegate_name,
            delegate_descriptor,
            cast_arguments,
            signature,
        } => {
            for check in checks {
                let pass = code.new_label();
                let slot = parameter_slot(&params, usize::from(check.parameter));
                if check.nullable {
                    code.aload(slot);
                    code.ifnull(pass);
                }
                code.aload(slot);
                code.instance_of(cw.class_ref(&check.class_name));
                code.ifne(pass);
                emit_inherited_failure(
                    &mut code,
                    cw,
                    failure.expect("a checked inherited collection bridge has a failure result"),
                    &params,
                );
                code.bind(pass);
            }
            code.aload(0);
            let mut slot = 1u16;
            for (index, ty) in params.iter().enumerate() {
                load(*ty, slot, &mut code);
                if let Some(cast) = cast_arguments
                    .iter()
                    .find(|cast| usize::from(cast.parameter) == index)
                {
                    code.checkcast(cw.class_ref(&cast.class_name));
                }
                slot += slot_words(*ty);
            }
            let arg_words: i32 = params.iter().map(|ty| i32::from(slot_words(*ty))).sum();
            let owner = class.fq_name();
            let method = cw.methodref(&owner, delegate_name, delegate_descriptor);
            code.invokevirtual(method, arg_words, i32::from(slot_words(ret)));
            emit_return(ret, &mut code);
            signature.as_deref()
        }
    };
    let signature = signature.filter(|signature| *signature != descriptor);
    let access = match &bridge.body {
        crate::jvm::inherited_collection_bridges::InheritedCollectionBridgeBody::SuperDelegate {
            ..
        } => 0x0041,
        crate::jvm::inherited_collection_bridges::InheritedCollectionBridgeBody::Checked {
            synthetic: true,
            ..
        } => 0x1051,
        crate::jvm::inherited_collection_bridges::InheritedCollectionBridgeBody::Checked {
            ..
        } => 0x0051,
    };
    finish_inherited(
        cw,
        &bridge.name,
        &descriptor,
        &mut code,
        1 + words,
        signature,
        access,
    );
    attach_inherited_bridge_debug(ir, class, cw, &bridge.name, &descriptor);
}

fn attach_inherited_bridge_debug(
    _ir: &IrFile,
    class: &crate::ir::IrClass,
    cw: &mut ClassWriter,
    name: &str,
    descriptor: &str,
) {
    if class.decl_line == 0 {
        return;
    }
    let line = if class.decl_start_line == 0 {
        class.decl_line
    } else {
        class.decl_start_line
    };
    cw.set_method_debug(
        name,
        descriptor,
        Some((0, line)),
        &[(String::from("this"), format!("L{};", class.fq_name()), 0)],
    );
}

fn finish_inherited(
    cw: &mut ClassWriter,
    name: &str,
    descriptor: &str,
    code: &mut CodeBuilder,
    locals: u16,
    signature: Option<&str>,
    access: u16,
) {
    match access {
        0x0041 => finish_code_sig::<0x0041>(cw, name, descriptor, code, locals, signature),
        0x0051 => finish_code_sig::<0x0051>(cw, name, descriptor, code, locals, signature),
        _ => finish_code_sig::<0x1051>(cw, name, descriptor, code, locals, signature),
    }
}

fn parameter_slot(params: &[Ty], index: usize) -> u16 {
    1 + params[..index]
        .iter()
        .map(|ty| slot_words(*ty))
        .sum::<u16>()
}

fn emit_inherited_failure(
    code: &mut CodeBuilder,
    cw: &mut ClassWriter,
    failure: crate::jvm::mapped_builtin_declarations::CollectionBridgeFailure,
    params: &[Ty],
) {
    match failure {
        crate::jvm::mapped_builtin_declarations::CollectionBridgeFailure::False => {
            code.push_int(0, cw);
            code.ireturn();
        }
        crate::jvm::mapped_builtin_declarations::CollectionBridgeFailure::Null => {
            code.aconst_null();
            code.areturn();
        }
        crate::jvm::mapped_builtin_declarations::CollectionBridgeFailure::NotFound => {
            code.push_int(-1, cw);
            code.ireturn();
        }
        crate::jvm::mapped_builtin_declarations::CollectionBridgeFailure::Argument(index) => {
            let index = usize::from(index);
            let ty = params[index];
            load(ty, parameter_slot(params, index), code);
            emit_return(ty, code);
        }
    }
}
