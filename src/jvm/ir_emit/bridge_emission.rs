//! JVM emission of bridge methods.
//!
//! A bridge exists because a supertype's erased signature differs from the override's. Everything
//! that adapts the two — the type-safe barrier, the argument checkcast/unbox/convert, the delegated
//! call, and the RETURN adapter — lives here rather than in the emitter facade, so the physical
//! boxing and carrier decisions stay in one JVM-owned place.

use super::{
    box_prim_free, discard, emit_num_conv, emit_return, finish_code, finish_code_sig, ir_ty_to_jvm,
    jvm_declared_ty, jvm_function_params, jvm_method_signature, jvm_tys, load, local_variable_desc,
    method_descriptor, slot_words, throw_assertion_error, type_descriptor, unbox_prim_from,
    verif_for_jvm_free, ClassWriter, CodeBuilder, EmitRun, JvmSignatureFormatter, VerifType,
};
use crate::ir::IrFile;
use crate::types::Ty;

fn finish_bridge(
    cw: &mut ClassWriter,
    name: &str,
    desc: &str,
    code: &mut CodeBuilder,
    locals: u16,
    kind: crate::ir::BridgeKind,
    signature: Option<&str>,
) {
    if kind == crate::ir::BridgeKind::ValueClassInterfaceEntry {
        finish_code_sig::<0x0001>(cw, name, desc, code, locals, signature);
    } else {
        finish_code::<{ 0x0001 | 0x0040 | 0x1000 }>(cw, name, desc, code, locals);
    }
}

fn emit_bridge_barrier_outcome(
    outcome: crate::jvm::backend::BridgeBarrierOutcome,
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
) {
    match outcome {
        crate::jvm::backend::BridgeBarrierOutcome::False => {
            code.push_int(0, cw);
            code.ireturn();
        }
        crate::jvm::backend::BridgeBarrierOutcome::NotFound => {
            code.push_int(-1, cw);
            code.ireturn();
        }
        crate::jvm::backend::BridgeBarrierOutcome::Null => {
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
            Some(format!(
                "$this${}",
                crate::jvm::debug_local_names::escaped(&bridge.name)
            ))
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
        bridge.parameter_identities.get(index),
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
    return_adaptations: &crate::jvm::bridge_return_adaptations::BridgeReturnAdaptations,
    run: &EmitRun,
) {
    let class = ClassBridges {
        ir,
        class: c,
        return_adaptations,
        run,
    };
    for (bridge_index, b) in c.bridges.iter().enumerate() {
        // An interface entry stands beside its static member, which emits it.
        if b.kind != crate::ir::BridgeKind::ValueClassInterfaceEntry {
            emit_bridge(&class, cw, bridge_index, b, None);
        }
    }
}

/// The class whose bridges are being emitted, with the run's bridge facts.
#[derive(Clone, Copy)]
struct ClassBridges<'a> {
    ir: &'a IrFile,
    class: &'a crate::ir::IrClass,
    return_adaptations: &'a crate::jvm::bridge_return_adaptations::BridgeReturnAdaptations,
    run: &'a EmitRun,
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
        return_adaptations,
        run,
    } = *class;
    let return_unboxing = u32::try_from(bridge_index)
        .ok()
        .and_then(|index| return_adaptations.get(c.fq_name_id(), index));
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
        |(_, function)| jvm_declared_ty(&function.ret),
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
        cw.reserve_method_pool(
            &b.name,
            &erased_desc,
            header.signature.as_deref(),
            &header.annotation_types(),
        );
    }
    if b.kind == crate::ir::BridgeKind::ValueClassInterfaceEntry {
        emit_interface_entry_checks(ir, b, &ep, cw, &mut code);
    }
    let body_pc = u16::try_from(code.bytes.len()).expect("parameter checks fit a method");
    if let Some(barrier) = crate::jvm::backend::bridge_barrier(b) {
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
        let concrete = crate::jvm::names::instanceof_internal_name(cp[barrier.parameter]);
        let concrete_class = cw.class_ref(&concrete);
        code.instance_of(concrete_class);
        code.ifne(dispatch);
        emit_bridge_barrier_outcome(barrier.outcome, cw, &mut code);
        let mut locals = vec![VerifType::ObjectName(c.fq_name())];
        locals.extend(ep.iter().map(|ty| verif_for_jvm_free(cw, *ty)));
        code.bind(dispatch);
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
        let field_ty = jvm_declared_ty(&field.ty);
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
    let mut slot = 1u16;
    for (k, (et, ct)) in ep.iter().zip(&cp).enumerate() {
        load(*et, slot, &mut code);
        slot += slot_words(*et);
        // A boxed value-class param (a generic supertype method `f(Object,…)` delegating to a mangled
        // concrete override taking the underlying): checkcast the incoming `Object` to the boxed `X`,
        // then `unbox-impl` it to the underlying `ct` the target expects.
        if let Some(Some(vc)) = b.unbox_params.get(k) {
            let vc = vc.render();
            let ci = cw.class_ref(&vc);
            code.checkcast(ci);
            let m = cw.methodref(&vc, "unbox-impl", &format!("(){}", type_descriptor(*ct)));
            code.invokevirtual(m, 0, slot_words(*ct) as i32);
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
            &b.name,
            &erased_desc,
            &mut code,
            1 + pw,
            b.kind,
            entry.and_then(|header| header.signature.as_deref()),
        );
        attach_bridge_debug_tables(ir, c, cw, b, &erased_desc, body_pc);
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
            &b.name,
            &erased_desc,
            &mut code,
            1 + pw,
            b.kind,
            entry.and_then(|header| header.signature.as_deref()),
        );
        attach_bridge_debug_tables(ir, c, cw, b, &erased_desc, body_pc);
        return;
    }
    if let Some(owner) = &b.box_ret {
        let owner = owner.render();
        let bi = cw.methodref(
            &owner,
            "box-impl",
            &format!(
                "({}){}",
                type_descriptor(cr),
                type_descriptor(Ty::obj(&owner))
            ),
        );
        code.invokestatic(bi, slot_words(cr) as i32, 1);
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
            && cr.is_reference()
            && crate::jvm::names::instanceof_internal_name(cr) == "java/lang/Void"
        {
            // `Nothing?` has only the value `null`, but its concrete JVM descriptor is
            // `java/lang/Void`. A bridge returning a narrower reference (for example a nullable
            // value class box) must refine the verifier type before `areturn`.
            let ci = cw.class_ref(&crate::jvm::names::instanceof_internal_name(er));
            code.checkcast(ci);
        } else if er.is_reference()
            && !er.is_array()
            && crate::jvm::names::instanceof_internal_name(cr) == "java/lang/Object"
        {
            // Covariant generic DIAMOND: the inherited concrete getter returns the erased
            // `Object` (`val x: T` in a generic base), but an interface in the hierarchy requires
            // a NARROWER reference type (`override val x: String`). This bridge's declared return
            // (`er`) is that narrower type, so the `Object` on the stack must be `checkcast` to it
            // before `areturn` — otherwise the verifier rejects it ("Bad return type"). The usual
            // direction (concrete is a SUBtype of erased) needs no cast; this is the inverse.
            // Restricted to a plain object type (`Ty::Obj`): an array `er` would need a descriptor-
            // form class ref, and that narrowing direction doesn't arise here.
            let ci = cw.class_ref(&crate::jvm::names::instanceof_internal_name(er));
            code.checkcast(ci);
        } // reference→reference (concrete is a subtype of erased): no cast needed
    }
    emit_return(er, &mut code);
    finish_bridge(
        cw,
        &b.name,
        &erased_desc,
        &mut code,
        1 + pw,
        b.kind,
        entry.and_then(|header| header.signature.as_deref()),
    );
    attach_bridge_debug_tables(ir, c, cw, b, &erased_desc, body_pc);
}

/// kotlinc gives every bridge a `LineNumberTable` rooted at the CLASS declaration and a
/// `LocalVariableTable` naming its receiver and parameters.
///
/// The bridge has no source of its own — it exists because a supertype's erased signature differs
/// from the override's — so the NAMES come from the override it delegates to, while the descriptors
/// are the ERASED ones the bridge actually receives (`item Ljava/lang/Object;`, not `String`). A
/// parameter the override does not name keeps the JVM's positional spelling. A property-setter
/// bridge has no source function identity; its generated parameter uses kotlinc's accessor spelling.
/// Attached as each bridge is written, not in a pass afterwards: the local-variable table's
/// strings are interned when they are recorded, and kotlinc interns them with the method they
/// belong to. Deferring the whole set moved `Ljava/lang/Object;` past the next bridge's descriptor.
fn attach_bridge_debug_tables(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    cw: &mut ClassWriter,
    bridge: &crate::ir::Bridge,
    erased_desc: &str,
    body_pc: u16,
) {
    if bridge.kind == crate::ir::BridgeKind::ValueClassInterfaceEntry {
        attach_interface_entry_debug_tables(ir, c, cw, bridge, erased_desc, body_pc);
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
            bridge.parameter_identities.len(),
            bridge.erased_params.len(),
            "bridge debug identities exactly match physical arity"
        );
        let mut locals = vec![(String::from("this"), this_desc.clone(), 0u16)];
        let mut slot = 1u16;
        let parameter_names = crate::jvm::parameter_names::resolved_local_variables(
            &bridge.parameter_identities,
            &bridge.concrete_params,
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

/// What an interface entry declares beyond its descriptor: its member's generic `Signature` and
/// nullability annotations, less the carrier the member receives first.
struct EntryHeader {
    signature: Option<String>,
    result: Option<&'static str>,
    parameters: Vec<Option<&'static str>>,
}

impl EntryHeader {
    fn of(
        ir: &IrFile,
        formatter: &JvmSignatureFormatter<'_>,
        member: u32,
        descriptor: &str,
    ) -> Self {
        let function = &ir.functions[member as usize];
        let signature = ir.signatures.get(&member).and_then(|generic| {
            let mut generic = super::value_class_signatures::physical_generic_signature(
                ir, member, function, generic,
            )
            .into_owned();
            assert_eq!(
                generic.params.len(),
                function.params.len(),
                "a lowered value-class member signs its carrier"
            );
            generic.params.remove(0);
            jvm_method_signature(formatter, &generic, function)
                .filter(|signature| signature != descriptor)
        });
        let nullability = super::declared_nullability::declared_nullability(ir, member);
        Self {
            signature,
            result: nullability.result,
            parameters: nullability.parameters.get(1..).unwrap_or_default().to_vec(),
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
    return_adaptations: &crate::jvm::bridge_return_adaptations::BridgeReturnAdaptations,
    run: &EmitRun,
) {
    for (bridge_index, b) in c.bridges.iter().enumerate() {
        if b.kind != crate::ir::BridgeKind::ValueClassInterfaceEntry
            || b.target_function != Some(member)
        {
            continue;
        }
        let descriptor = method_descriptor(&jvm_tys(&b.erased_params), ir_ty_to_jvm(&b.erased_ret));
        let header = EntryHeader::of(ir, formatter, member, &descriptor);
        let class = ClassBridges {
            ir,
            class: c,
            return_adaptations,
            run,
        };
        emit_bridge(&class, cw, bridge_index, b, Some(&header));
        cw.set_method_nullability(&b.name, &descriptor, header.result, &header.parameters);
    }
}
