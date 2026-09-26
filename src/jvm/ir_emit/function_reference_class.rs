//! The class a callable reference compiles to when it is not an `invokedynamic`: its carrier
//! superclass, constructor, `invoke` dispatch and singleton instance.

use super::*;

/// Emit a synthesized function-reference subclass (`<Owner>$ref$N extends FunctionReferenceImpl
/// implements Function<arity>`): an UNBOUND ref gets a `public static final INSTANCE` + a no-arg ctor
/// `super(arity, owner.class, name, sig, flags)`; a BOUND ref gets a `(Object)` ctor delegating to
/// `super(arity, receiver, owner.class, name, sig, flags)` (the base stores the receiver). The single
/// erased `invoke(Object…)Object` casts/unboxes its args and dispatches to the target, boxing the
/// result (or returning the `Unit` singleton for a `void` target). Reference EQUALITY (`::f == ::f`,
/// `a::m != b::m`) is inherited from `FunctionReferenceImpl` (compares owner/name/signature/receiver).
pub(super) fn emit_func_ref_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) -> Vec<u8> {
    use crate::ir::FrDispatch;
    let fr = c.func_ref.as_ref().unwrap();
    let physical_arity = u8::try_from(fr.param_tys.len())
        .expect("JVM function-reference arity must fit its runtime carrier");
    // Structural adapters name their exact common-IR target. Derive the physical JVM parameters
    // from that declaration so sparse representation facts (notably shared mutable-capture cells)
    // cannot disagree between the helper descriptor and the reference carrier's invoke call.
    let target_param_tys = fr
        .local_target
        .map(|target| jvm_function_params(ir, target))
        .unwrap_or_else(|| jvm_tys(&fr.target_param_tys));
    let field_capture_count = fr.field_capture_count as usize;
    let field_capture_tys = target_param_tys
        .get(..field_capture_count)
        .expect("callable-reference captures are a prefix of its adapter parameters");
    let field_capture_descs = field_capture_tys
        .iter()
        .map(|capture| type_descriptor(*capture))
        .collect::<Vec<_>>();
    // A missing `owner_class`/`call_owner` is the facade sentinel (a top-level function lives on the
    // file facade, whose name isn't known until emit) — resolve it here.
    let owner_class = fr.owner_class_or_facade(facade);
    let owner_class = crate::jvm::jvm_class_map::to_jvm_internal(&owner_class).to_string();
    let call_owner = fr.call_owner_or_facade(facade);
    let call_owner = crate::jvm::jvm_class_map::to_jvm_internal(&call_owner).to_string();
    let fq = c.fq_name();
    let superclass = if fr.adapted {
        "kotlin/jvm/internal/AdaptedFunctionReference".to_string()
    } else {
        c.superclass()
    };
    // The carrier's generic header: its runtime base class, then the Kotlin function type it
    // implements (and the suspend marker interface), written as a supertype, without wildcards.
    let suspend = fr.is_suspend || matches!(fr.dispatch, FrDispatch::SuspendConvert);
    let signature = JvmSignatureFormatter::new(ir, env)
        .ty_at(&fr.function_type, Wildcards::Suppressed)
        .map(|function| {
            let marker = if suspend {
                "Lkotlin/coroutines/jvm/internal/SuspendFunction;"
            } else {
                ""
            };
            format!("L{superclass};{function}{marker}")
        });
    let mut cw = new_writer_generic(&fq, signature.as_deref(), &superclass, opts);
    if let Some(signature) = &signature {
        cw.set_signature(signature);
    }
    // Package-private, kotlinc's shape — EXCEPT when the class lands cross-package (an
    // INLINE-SPLICED reference regenerates the callee module's adapter under the callee's package
    // while the caller lives elsewhere) or is referenced from a PUBLIC INLINE body
    // (`IrFile::public_synthetics`) — package-private there is an IllegalAccessError (corpus
    // `adaptedSuspendFunctionReference.kt`).
    let cross_package =
        fq.rsplit_once('/').map(|(p, _)| p) != facade.rsplit_once('/').map(|(p, _)| p);
    let inline_reachable = ir.public_synthetics.contains(&c.fq_name_id());
    // kotlinc marks every callable-reference carrier ACC_SYNTHETIC.
    cw.set_access(if cross_package || inline_reachable {
        0x1000 | 0x0001 | 0x0010 | 0x0020 // SYNTHETIC | PUBLIC | FINAL | SUPER
    } else {
        0x1000 | 0x0010 | 0x0020 // SYNTHETIC | FINAL | SUPER
    });
    // kotlinc's enclosure record: the scope the reference is written in, and an inner-only
    // `InnerClasses` entry for the class itself and each class it names.
    if let Some((owner, method)) = class_enclosure(ir, c, facade) {
        match method {
            Some((name, descriptor)) => cw.set_enclosing_method(&owner, &name, &descriptor),
            None => cw.set_enclosing_class(&owner),
        }
    }
    env.inner_classes.register(&mut cw);
    cw.add_interface(&jvm_function_interface(physical_arity));
    if suspend {
        // The suspend-conversion adapter also carries kotlinc's suspend-function marker interface.
        cw.add_interface("kotlin/coroutines/jvm/internal/SuspendFunction");
    }
    for (index, descriptor) in field_capture_descs.iter().enumerate() {
        cw.add_field(0x0012, &format!("$captured${index}"), descriptor);
    }

    // The call argument param types begin AFTER the receiver for an unbound member ref.
    let first_arg = match fr.dispatch {
        FrDispatch::VirtualUnbound => 1usize,
        _ => 0,
    };
    // For `StaticBound` the captured receiver is target arg 0, so invoke arg `k` maps to
    // `target_param_tys[k + 1]`.
    let target_offset =
        field_capture_tys.len() + usize::from(matches!(fr.dispatch, FrDispatch::StaticBound));
    let target_ret_ty = fr
        .local_target
        .map(|target| ir.functions[target as usize].ret)
        .unwrap_or(fr.target_ret_ty);
    let target_ret_jvm = jvm_declared_ty(&target_ret_ty);
    let target_returns_void = matches!(target_ret_ty, Ty::Unit | Ty::Nothing);
    let coerce_unit = fr.ret_ty == Ty::Unit && !target_returns_void;
    // Reflection records the physical target descriptor without an unbound receiver.
    let mut signature_desc = String::from("(");
    let reflection_parameters = fr
        .reflection_target_param_tys
        .as_deref()
        .unwrap_or(&target_param_tys);
    let reflection_first_arg = if fr.reflection_target_param_tys.is_none() {
        first_arg.max(usize::from(fr.reflection_receiver_parameter))
    } else {
        0
    };
    for pt in reflection_parameters.iter().skip(reflection_first_arg) {
        signature_desc.push_str(&ir_type_desc(pt));
    }
    signature_desc.push(')');
    let reflection_target_ret = fr.reflection_target_ret_ty.unwrap_or(fr.target_ret_ty);
    let signature_ret = if matches!(reflection_target_ret, Ty::Unit | Ty::Nothing) {
        "V".to_string()
    } else {
        type_descriptor(jvm_declared_ty(&reflection_target_ret))
    };
    signature_desc.push_str(&signature_ret);
    let reflection_name = fr.reflection_name.as_deref().unwrap_or(&fr.fn_name);
    let signature_name = match fr.dispatch {
        FrDispatch::Static | FrDispatch::StaticBound | FrDispatch::SuspendConvert => {
            reflection_name
        }
        FrDispatch::VirtualUnbound | FrDispatch::VirtualBound => {
            mapped_builtin_virtual_name(&call_owner, reflection_name, &signature_desc)
        }
    };
    let signature = format!("{signature_name}{signature_desc}");

    let call_desc = if matches!(fr.dispatch, FrDispatch::SuspendConvert) {
        // The delegated call is the wrapped value's ERASED `Function{n}.invoke` — `n` erased Object
        // parameters (the invoke's trailing continuation is dropped), Object return.
        let mut d = String::from("(");
        for _ in 0..physical_arity as usize - 1 {
            d.push_str("Ljava/lang/Object;");
        }
        d.push_str(")Ljava/lang/Object;");
        d
    } else {
        let mut d = String::from("(");
        for pt in target_param_tys.iter().skip(first_arg) {
            d.push_str(&ir_type_desc(pt));
        }
        d.push(')');
        let ret_desc = if target_returns_void {
            "V".to_string()
        } else {
            type_descriptor(target_ret_jvm)
        };
        d.push_str(&ret_desc);
        d
    };

    if !field_capture_tys.is_empty() {
        let capture_words: u16 = field_capture_tys.iter().map(|ty| slot_words(*ty)).sum();
        let ctor_locals = 1 + capture_words + u16::from(fr.bound);
        let mut ctor = CodeBuilder::new(ctor_locals);
        let mut slot = 1u16;
        for (index, ty) in field_capture_tys.iter().copied().enumerate() {
            ctor.aload(0);
            load(ty, slot, &mut ctor);
            let field = cw.fieldref(
                &fq,
                &format!("$captured${index}"),
                &field_capture_descs[index],
            );
            ctor.putfield(field, slot_words(ty) as i32 + 1);
            slot += slot_words(ty);
        }
        ctor.aload(0);
        ctor.push_int(physical_arity as i32, &mut cw);
        if fr.bound {
            ctor.aload(slot);
        }
        ctor.ldc_class(&owner_class, &mut cw);
        ctor.push_string(&fr.fn_name, &mut cw);
        ctor.push_string(&signature, &mut cw);
        ctor.push_int(fr.flags, &mut cw);
        let super_descriptor = if fr.bound {
            "(ILjava/lang/Object;Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V"
        } else {
            "(ILjava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V"
        };
        let sup = cw.methodref(&superclass, "<init>", super_descriptor);
        ctor.invokespecial(sup, if fr.bound { 6 } else { 5 }, 0);
        ctor.ret_void();
        let descriptor = format!(
            "({}{})V",
            field_capture_descs.concat(),
            if fr.bound { "Ljava/lang/Object;" } else { "" }
        );
        if cross_package || inline_reachable {
            finish_code::<0x0001>(&mut cw, "<init>", &descriptor, &mut ctor, ctor_locals);
        } else {
            finish_code::<0x0000>(&mut cw, "<init>", &descriptor, &mut ctor, ctor_locals);
        }
    } else if fr.bound {
        // `<init>(Object)V`: super(arity, receiver, owner.class, name, sig, flags).
        cw.seed_utf8("<init>");
        cw.seed_utf8("(Ljava/lang/Object;)V");
        let mut ctor = CodeBuilder::new(2);
        ctor.aload(0);
        ctor.push_int(physical_arity as i32, &mut cw);
        ctor.aload(1);
        ctor.ldc_class(&owner_class, &mut cw);
        ctor.push_string(&fr.fn_name, &mut cw);
        ctor.push_string(&signature, &mut cw);
        ctor.push_int(fr.flags, &mut cw);
        let sup = cw.methodref(
            &superclass,
            "<init>",
            "(ILjava/lang/Object;Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V",
        );
        ctor.invokespecial(sup, 6, 0);
        ctor.ret_void();
        let locals =
            function_reference_invoke::reference_constructor_locals(&mut cw, &fq, &["receiver0"]);
        // The ctor's access mirrors the class's: a PUBLIC synthetic is constructed from other
        // packages by spliced code.
        if cross_package || inline_reachable {
            finish_code::<0x0001>(&mut cw, "<init>", "(Ljava/lang/Object;)V", &mut ctor, 2);
        } else {
            finish_code::<0x0000>(&mut cw, "<init>", "(Ljava/lang/Object;)V", &mut ctor, 2);
        }
        cw.set_method_debug("<init>", "(Ljava/lang/Object;)V", None, &locals);
    } else {
        // `<init>()V`: super(arity, owner.class, name, sig, flags).
        cw.seed_utf8("<init>");
        cw.seed_utf8("()V");
        let mut ctor = CodeBuilder::new(1);
        ctor.aload(0);
        ctor.push_int(physical_arity as i32, &mut cw);
        ctor.ldc_class(&owner_class, &mut cw);
        ctor.push_string(&fr.fn_name, &mut cw);
        ctor.push_string(&signature, &mut cw);
        ctor.push_int(fr.flags, &mut cw);
        let sup = cw.methodref(
            &superclass,
            "<init>",
            "(ILjava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V",
        );
        ctor.invokespecial(sup, 5, 0);
        ctor.ret_void();
        let locals = function_reference_invoke::reference_constructor_locals(&mut cw, &fq, &[]);
        if cross_package || inline_reachable {
            finish_code::<0x0001>(&mut cw, "<init>", "()V", &mut ctor, 1);
        } else {
            finish_code::<0x0000>(&mut cw, "<init>", "()V", &mut ctor, 1);
        }
        cw.set_method_debug("<init>", "()V", None, &locals);
    }
    // A singleton carrier's `<clinit>` follows its methods, as kotlinc orders them.
    let singleton = field_capture_tys.is_empty() && !fr.bound;
    if let Some(invoke) = fr.invoke {
        emit_method(ir, invoke, &fq, facade, &mut cw, true, env);
        function_reference_invoke::emit_reference_invoke_bridge(
            ir,
            &mut cw,
            &fq,
            fr,
            invoke,
            physical_arity,
        );
        if singleton {
            emit_singleton_instance_clinit(&mut cw, &fq);
            add_singleton_instance_field(&mut cw, &fq);
        }
        return finish_local_synthetic_class(cw);
    }

    // Numbered JVM function interfaces stop at arity 22. Larger Kotlin function types use the
    // single `FunctionN.invoke(Object[])Object` carrier; their semantic arity remains unchanged.
    let arity = physical_arity as u16;
    let high_arity = is_high_arity_function(physical_arity);
    let invoke_desc = jvm_function_invoke_descriptor(physical_arity);
    let invoke_locals = if high_arity { 2 } else { 1 + arity };
    let mut inv = CodeBuilder::new(invoke_locals);
    for (index, ty) in field_capture_tys.iter().copied().enumerate() {
        inv.aload(0);
        let field = cw.fieldref(
            &fq,
            &format!("$captured${index}"),
            &field_capture_descs[index],
        );
        inv.getfield(field, slot_words(ty) as i32);
    }
    // Push the receiver for a member dispatch (`first_arg`, computed above, skips it in the arg loop).
    match fr.dispatch {
        FrDispatch::VirtualBound | FrDispatch::SuspendConvert => {
            inv.aload(0);
            let recv_f = cw.fieldref(&superclass, "receiver", "Ljava/lang/Object;");
            inv.getfield(recv_f, 1);
            let owner_ref = cw.class_ref(&call_owner);
            inv.checkcast(owner_ref);
        }
        FrDispatch::VirtualUnbound => {
            function_reference_invoke::load_erased_function_argument(
                &mut cw, &mut inv, high_arity, 0,
            );
            let owner_ref = cw.class_ref(&call_owner);
            inv.checkcast(owner_ref);
        }
        FrDispatch::Static => {}
        FrDispatch::StaticBound => {
            // The captured receiver is the FIRST static argument: load `this.receiver`, cast to the
            // target receiver type (after any leading ordinary capture fields).
            inv.aload(0);
            let recv_f = cw.fieldref(&superclass, "receiver", "Ljava/lang/Object;");
            inv.getfield(recv_f, 1);
            if let Some(vc) = &fr.staticbound_recv_unbox {
                // A VALUE-CLASS receiver (`Z(42)::ext`) is stored BOXED: `checkcast` to the box class then
                // `unbox-impl` to the underlying the mangled target expects (`Z`→`int`).
                let vc = vc.render();
                let cref = cw.class_ref(&vc);
                inv.checkcast(cref);
                let under = jvm_declared_ty(
                    target_param_tys
                        .get(field_capture_count)
                        .copied()
                        .as_ref()
                        .unwrap_or(&Ty::Error),
                );
                let m = cw.methodref(&vc, "unbox-impl", &format!("(){}", type_descriptor(under)));
                inv.invokevirtual(m, 0, slot_words(under) as i32);
            } else if let Some(primitive) = target_param_tys
                .get(field_capture_count)
                .map(jvm_declared_ty)
                .filter(|ty| ty.is_jvm_scalar())
            {
                unbox_prim_from(&mut cw, &mut inv, Ty::obj("java/lang/Object"), primitive);
            } else if let Some(internal) = target_param_tys
                .get(field_capture_count)
                .map(jvm_declared_ty)
                .and_then(checkcast_internal)
            {
                let cref = cw.class_ref(&internal);
                inv.checkcast(cref);
            }
        }
    };
    // Push the call arguments (cast/unbox each erased `Object`).
    let mut call_arg_words = field_capture_tys
        .iter()
        .map(|ty| slot_words(*ty) as i32)
        .sum::<i32>()
        + match fr.dispatch {
            // The captured receiver already pushed above occupies one (reference) target slot.
            FrDispatch::StaticBound => target_param_tys
                .get(field_capture_count)
                .map_or(0, |t| slot_words(jvm_declared_ty(t)) as i32),
            _ => 0,
        };
    for (k, pt) in fr.param_tys.iter().enumerate().skip(first_arg) {
        // Suspend conversion: the trailing continuation parameter is NOT forwarded — the wrapped
        // plain function never suspends and takes only the value arguments.
        if matches!(fr.dispatch, FrDispatch::SuspendConvert) && k == fr.param_tys.len() - 1 {
            continue;
        }
        function_reference_invoke::load_erased_function_argument(&mut cw, &mut inv, high_arity, k);
        let jt = ir_ty_to_jvm(pt);
        let target_jt = target_param_tys
            .get(k + target_offset)
            .map(jvm_declared_ty)
            .unwrap_or(jt);
        let value_class_unbox = fr
            .unbox_params
            .get(k)
            .and_then(|value| value.as_ref())
            .filter(|_| !jt.is_jvm_scalar());
        if value_class_unbox.is_some() {
            // `FunctionN.invoke` receives the boxed VALUE CLASS as Object. Its physical target can
            // use the underlying reference type (for example `Value(String)` -> `String`), but that
            // does not license casting the incoming object to the underlying type. The boxed class
            // owns the representation boundary: cast to it, then call `unbox-impl` below.
        } else if jt.is_jvm_scalar() && target_jt.is_jvm_scalar() {
            let adapter = semantic_scalar_adapter(*pt, jt);
            let wref = cw.class_ref(
                crate::jvm::jvm_class_map::wrapper_internal(adapter).unwrap_or("java/lang/Object"),
            );
            inv.checkcast(wref);
            unbox_prim(&mut cw, &mut inv, adapter);
        } else if jt.is_jvm_scalar() && target_jt.is_reference() {
            let target = crate::jvm::names::instanceof_internal_name(target_jt);
            if target != "java/lang/Object" {
                let cref = cw.class_ref(&target);
                inv.checkcast(cref);
            }
        } else if let Some(internal) = checkcast_internal(target_jt) {
            let cref = cw.class_ref(&internal);
            inv.checkcast(cref);
        }
        if let Some(vc) = value_class_unbox {
            emit_value_class_unbox_adapter(
                &mut cw,
                &mut inv,
                *vc,
                target_jt,
                fr.unbox_param_nullable.get(k).copied().unwrap_or(false),
            );
        }
        call_arg_words += slot_words(target_jt) as i32;
    }
    // Dispatch to the target.
    let ret_words = if target_returns_void {
        0
    } else {
        slot_words(target_ret_jvm) as i32
    };
    // A reference to a PRIVATE same-file top-level function can't invokestatic it from this
    // (separate) class — call kotlinc's `access$<name>` facade bridge instead (`emit_pass` emits it
    // for exactly these referenced targets).
    let static_call_name = if fr.call_owner_is_facade()
        && function_reference_target(ir, fr)
            .is_some_and(|target| ir.private_methods.contains(&target))
    {
        format!("access${}", fr.call_name)
    } else {
        fr.call_name.clone()
    };
    match fr.dispatch {
        FrDispatch::Static | FrDispatch::StaticBound => {
            let m = cw.methodref(&call_owner, &static_call_name, &call_desc);
            inv.invokestatic(m, call_arg_words, ret_words);
        }
        // A bound reference to a mapped-builtin member (`"KOTLIN"::get`) invokes the same PHYSICAL JVM
        // method a direct call would (`String.get` → `charAt`) — apply the backend's name mapping here too.
        _ if fr.call_interface => {
            let vn = mapped_builtin_virtual_name(&call_owner, &fr.call_name, &call_desc);
            let m = cw.interface_methodref(&call_owner, vn, &call_desc);
            inv.invokeinterface(m, call_arg_words, ret_words);
        }
        _ => {
            let vn = mapped_builtin_virtual_name(&call_owner, &fr.call_name, &call_desc);
            let m = cw.methodref(&call_owner, vn, &call_desc);
            inv.invokevirtual(m, call_arg_words, ret_words);
        }
    }
    // Adapt the result to `Object`: a `void` target yields the `Unit` singleton; a value-class-returning
    // reference boxes the erased underlying back to the value class; a plain primitive is wrapper-boxed.
    if target_returns_void {
        let unit = cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
        inv.getstatic(unit, 1);
    } else if coerce_unit {
        discard(target_ret_jvm, &mut inv);
        let unit = cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
        inv.getstatic(unit, 1);
    } else if let Some(owner) = &fr.box_ret {
        let owner = owner.render();
        // A value-class-returning reference: the target returns the ERASED underlying (primitive or the
        // reference underlying) — exactly what `call_desc` requested. Box it back to the value class via
        // `box-impl` so the `Function` result is the boxed VC (`X` object) the invariant requires — a VC in
        // a `FunctionN` slot is boxed. Without it a `typeAdapter::decode` returning `X` hands back the bare
        // underlying that the caller then `checkcast X`es → `ClassCastException`.
        let bi = cw.methodref(
            &owner,
            "box-impl",
            &format!("({})L{};", type_descriptor(target_ret_jvm), owner),
        );
        inv.invokestatic(bi, slot_words(target_ret_jvm) as i32, 1);
    } else if target_ret_jvm.is_jvm_scalar() {
        // `invoke` returns `Object` regardless of the target descriptor. Preserve the function
        // reference's semantic return while selecting that wrapper; the target carrier alone cannot
        // distinguish `UInt` from `Int`.
        box_prim_free(
            &mut cw,
            &mut inv,
            semantic_scalar_adapter(fr.ret_ty, target_ret_jvm),
        );
    }
    inv.areturn();
    finish_code::<0x0001>(&mut cw, "invoke", &invoke_desc, &mut inv, invoke_locals);
    // kotlinc visits a carrier's fields after its methods.
    if singleton {
        emit_singleton_instance_clinit(&mut cw, &fq);
        add_singleton_instance_field(&mut cw, &fq);
    }
    finish_local_synthetic_class(cw)
}
