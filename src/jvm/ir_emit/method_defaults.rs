//! JVM realization of default-valued function and method parameters.

use super::*;
use crate::jvm::default_parameter_representation::{primitive_unbox_method, primitive_wrapper};

/// kotlinc opens an inheritable member's `$default` synthetic with a guard on the trailing marker:
/// a `super.m()` call carrying defaults cannot dispatch through the virtual forwarding stub.
fn emit_default_super_guard(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    marker_slot: u16,
    method_name: &str,
) {
    code.aload(marker_slot);
    let ok = code.new_label();
    code.ifnull(ok);
    let cls = cw.class_ref("java/lang/UnsupportedOperationException");
    code.new_obj(cls);
    code.dup();
    code.push_string(
        &format!(
            "Super calls with default arguments not supported in this target, function: {method_name}"
        ),
        cw,
    );
    let ctor = cw.methodref(
        "java/lang/UnsupportedOperationException",
        "<init>",
        "(Ljava/lang/String;)V",
    );
    code.invokespecial(ctor, 1, 0);
    code.athrow();
    code.bind(ok);
}

/// Whether a class can be inherited from, and therefore needs the member-default super-call guard.
fn owner_is_inheritable(ir: &IrFile, owner: &str) -> bool {
    ir.classes.iter().any(|class| {
        class.fq_name_matches(owner)
            && !class.is_object
            && (class.is_open || class.is_abstract || class.is_sealed || class.is_enum)
    })
}

/// Emit an instance method's mask-expanding `$default` synthetic.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_default_stub(
    ir: &IrFile,
    fid: u32,
    static_owner: Option<StaticOwner>,
    owner: &str,
    facade: &str,
    cw: &mut ClassWriter,
    defaults: &[Option<u32>],
    env: &EmitEnv,
    is_interface: bool,
) {
    let function = &ir.functions[fid as usize];
    let method_name = function.name.clone();
    let real_params = jvm_function_params(ir, fid);
    let boxed = default_stub_boxed_parameters(ir, fid);
    let stub_param_tys: Vec<Ty> = real_params
        .iter()
        .enumerate()
        .map(|(index, ty)| stub_parameter_type(ir, fid, index, *ty))
        .collect();
    let receiver_offset = usize::from(ir.extension_receiver_fns.contains(&fid));
    let logical_param_count = real_params
        .len()
        .checked_sub(receiver_offset)
        .expect("an extension receiver is a leading physical parameter");
    let ret = jvm_declared_ty(&function.ret);
    let owner_ty = Ty::obj(owner);
    // The `$DefaultImpls` copy of a PRIVATE member's stub (`static_owner.is_none()` distinguishes
    // it from the interface-side stub): `disable` publishes no interface-side declaration for a
    // private member, so kotlinc's holder stub invokes the holder's own receiver-first static and
    // is itself `public static synthetic` (measured on 2.4.20), not the private member's access.
    let holder_private_dispatch =
        is_interface && static_owner.is_none() && ir.method_visibility(fid).is_private();
    let stub_name = format!("{method_name}$default");
    let stub_desc = method_descriptor(&default_stub_params(ir, fid, owner_ty), ret);
    cw.reserve_method_name(&stub_name);
    cw.reserve_descriptor(&stub_desc);
    let mut emitter = Emitter::new(
        ir,
        cw,
        env,
        static_owner,
        owner,
        facade,
        ret,
        defaults.iter().flatten().copied(),
    );
    publish_exported_private_access(&mut emitter, fid);
    let receiver = emitter.frame.enter(FrameKey::Receiver, owner_ty);
    emitter.slots.insert(0, (receiver, owner_ty));
    let mut param_slots = Vec::new();
    for (index, ty) in stub_param_tys.iter().enumerate() {
        let value = index as u32 + 1;
        let slot = emitter.frame.enter(FrameKey::Value(value), *ty);
        emitter.slots.insert(value, (slot, *ty));
        param_slots.push((slot, *ty));
    }
    let mask_count = default_mask_count(logical_param_count);
    let mask_slots = (0..mask_count)
        .map(|mask| {
            let slot = emitter.frame.enter(
                FrameKey::Parameter((stub_param_tys.len() + mask) as u16),
                Ty::Int,
            );
            let _ = emitter.lease_temporary(slot, Ty::Int);
            slot
        })
        .collect::<Vec<_>>();
    let marker_slot = emitter.frame.enter(
        FrameKey::Parameter((stub_param_tys.len() + mask_count) as u16),
        Ty::obj("java/lang/Object"),
    );
    let _ = emitter.lease_temporary(marker_slot, Ty::obj("java/lang/Object"));

    let mut code = CodeBuilder::new(emitter.frame.size());
    if is_interface || owner_is_inheritable(ir, owner) {
        emit_default_super_guard(emitter.cw, &mut code, marker_slot, &method_name);
    }
    emit_default_param_overwrites(
        &mut emitter,
        &mut code,
        &defaults[receiver_offset..],
        receiver_offset,
        &param_slots,
        &mask_slots,
    );
    code.aload(0);
    for (index, &(slot, ty)) in param_slots.iter().enumerate() {
        load(ty, slot, &mut code);
        emit_adapted_unbox(
            ir,
            emitter.cw,
            &boxed,
            index,
            real_params[index],
            ty,
            &mut code,
        );
    }
    let argument_words: i32 = real_params.iter().map(|ty| slot_words(*ty) as i32).sum();
    let descriptor = method_descriptor(&real_params, ret);
    if holder_private_dispatch {
        let holder = format!("{owner}$DefaultImpls");
        let mut holder_params = vec![owner_ty];
        holder_params.extend_from_slice(&real_params);
        let holder_descriptor = method_descriptor(&holder_params, ret);
        let method = emitter
            .cw
            .methodref(&holder, &method_name, &holder_descriptor);
        // The receiver was loaded first, so the static target's arguments are already in order.
        code.invokestatic(method, argument_words + 1, slot_words(ret) as i32);
    } else {
        let method = if is_interface {
            emitter
                .cw
                .interface_methodref(owner, &method_name, &descriptor)
        } else {
            emitter.cw.methodref(owner, &method_name, &descriptor)
        };
        if is_interface {
            code.invokeinterface(method, argument_words, slot_words(ret) as i32);
        } else if ir.method_visibility(fid).is_private() {
            code.invokespecial(method, argument_words, slot_words(ret) as i32);
        } else {
            code.invokevirtual(method, argument_words, slot_words(ret) as i32);
        }
    }
    emit_return(ret, &mut code);
    code.ensure_locals(emitter.frame.max());
    code.link();
    // kotlinc's holder stub for a private member is `public static synthetic` (the private member
    // itself has no interface-side declaration, so nothing restricts the stub's reach).
    let access = if holder_private_dispatch {
        0x1009
    } else {
        default_stub_access(ir, fid)
    };
    emitter.cw.add_method(access, &stub_name, &stub_desc, &code);
    if let Some(&line) = ir
        .fn_sig_lines
        .get(&fid)
        .or_else(|| ir.fn_decl_lines.get(&fid))
    {
        emitter
            .cw
            .set_method_lines(&stub_name, &stub_desc, &[(0, line)]);
    }
    drop(emitter);
    emit_published_default_accessor(
        ir,
        fid,
        cw,
        PublishedDefaultAccessor {
            owner,
            stub_name: &stub_name,
            parameters: &default_stub_params(ir, fid, owner_ty),
            result: ret,
            is_interface,
        },
    );
}

/// Physical parameters of an instance method's `$default` synthetic.
pub(super) fn default_stub_params(ir: &IrFile, fid: u32, owner_ty: Ty) -> Vec<Ty> {
    let real_params = jvm_function_params(ir, fid);
    let receiver_offset = usize::from(ir.extension_receiver_fns.contains(&fid));
    let logical_param_count = real_params
        .len()
        .checked_sub(receiver_offset)
        .expect("an extension receiver is a leading physical parameter");
    let mut parameters = vec![owner_ty];
    parameters.extend(
        real_params
            .iter()
            .enumerate()
            .map(|(index, ty)| stub_parameter_type(ir, fid, index, *ty)),
    );
    parameters.extend(std::iter::repeat_n(
        Ty::Int,
        default_mask_count(logical_param_count),
    ));
    parameters.push(Ty::obj("java/lang/Object"));
    parameters
}

pub(super) fn default_stub_boxed_parameters(ir: &IrFile, fid: u32) -> HashMap<usize, Ty> {
    ir.default_stub_boxed_params
        .get(&fid)
        .map(|parameters| parameters.iter().copied().collect())
        .unwrap_or_default()
}

pub(super) fn stub_parameter_type(ir: &IrFile, fid: u32, index: usize, physical: Ty) -> Ty {
    default_stub_boxed_parameters(ir, fid)
        .get(&index)
        .copied()
        .unwrap_or(physical)
}

pub(super) fn emit_primitive_value_of(cw: &mut ClassWriter, primitive: Ty, code: &mut CodeBuilder) {
    let Some(wrapper) = primitive_wrapper(primitive) else {
        return;
    };
    let owner = wrapper
        .obj_internal()
        .expect("a primitive wrapper names its class")
        .render();
    let method = cw.methodref(
        &owner,
        "valueOf",
        &format!(
            "({}){}",
            type_descriptor(primitive),
            type_descriptor(wrapper)
        ),
    );
    code.invokestatic(method, slot_words(primitive) as i32, 1);
}

pub(super) fn emit_primitive_box_if_needed(
    cw: &mut ClassWriter,
    produced: Ty,
    slot: Ty,
    code: &mut CodeBuilder,
) {
    if primitive_wrapper(produced) == Some(slot) {
        emit_primitive_value_of(cw, produced, code);
    }
}

/// Narrow a default value to its parameter slot when the value's class is a proper subtype.
fn emit_default_store_narrowing(
    cw: &mut ClassWriter,
    produced: Ty,
    slot: Ty,
    code: &mut CodeBuilder,
) {
    if produced == Ty::Null || !produced.is_reference() || !slot.is_reference() {
        return;
    }
    if crate::jvm::names::same_type_descriptor(produced, slot) {
        return;
    }
    let Some(internal) = super::checkcast_internal(slot) else {
        return;
    };
    let class = cw.class_ref(&internal);
    code.checkcast(class);
}

/// Name and descriptor of `function`'s `$default` bridge, the method a default lambda's
/// `EnclosingMethod` names. `owner` is that bridge's class.
pub(super) fn default_stub_method(ir: &IrFile, function: u32, owner: &str) -> (String, String) {
    let declaration = &ir.functions[function as usize];
    let result = jvm_declared_ty(&declaration.ret);
    let parameters = if declaration.is_static {
        static_default_stub_params(ir, function)
    } else {
        default_stub_params(ir, function, Ty::obj(owner))
    };
    (
        format!("{}$default", declaration.name),
        method_descriptor(&parameters, result),
    )
}

pub(super) fn emit_omitted_default_placeholder(
    cw: &mut ClassWriter,
    real: Ty,
    stub: Ty,
    code: &mut CodeBuilder,
) {
    if primitive_wrapper(real) == Some(stub) {
        push_zero(real, code, cw);
        emit_primitive_value_of(cw, real, code);
        return;
    }
    push_zero(stub, code, cw);
}

fn emit_adapted_unbox(
    ir: &IrFile,
    cw: &mut ClassWriter,
    boxed: &HashMap<usize, Ty>,
    index: usize,
    real: Ty,
    stub: Ty,
    code: &mut CodeBuilder,
) {
    if primitive_wrapper(real) == Some(stub) {
        let owner = stub
            .obj_internal()
            .expect("a primitive wrapper names its class")
            .render();
        let method = primitive_unbox_method(real).expect("a JVM primitive has an unbox method");
        let unbox = cw.methodref(&owner, method, &format!("(){}", type_descriptor(real)));
        code.invokevirtual(unbox, 0, slot_words(real) as i32);
        return;
    }
    if let Some(value_class) = boxed.get(&index) {
        emit_unbox_impl(ir, cw, value_class, code);
    }
}

/// A static `$default` stub's trailing marker is constructor-specific only for a value-class
/// `constructor-impl`; every function stub uses plain `Object`.
pub(super) fn static_default_stub_marker(ir: &IrFile, fid: u32) -> Ty {
    if ir.jvm_value_class_constructor_impls.contains_key(&fid) {
        Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker")
    } else {
        Ty::obj("java/lang/Object")
    }
}

pub(super) fn static_default_stub_params(ir: &IrFile, fid: u32) -> Vec<Ty> {
    let marker = static_default_stub_marker(ir, fid);
    let function = &ir.functions[fid as usize];
    let mut parameters = jvm_function_params(ir, fid);
    for (index, parameter) in parameters.iter_mut().enumerate() {
        *parameter = stub_parameter_type(ir, fid, index, *parameter);
    }
    let receiver_prefix = usize::from(function.is_static && function.dispatch_receiver.is_some())
        + usize::from(ir.extension_receiver_fns.contains(&fid));
    let logical_parameter_count = parameters
        .len()
        .checked_sub(receiver_prefix)
        .expect("callable receivers are leading physical parameters");
    parameters.extend(std::iter::repeat_n(
        Ty::Int,
        default_mask_count(logical_parameter_count),
    ));
    parameters.push(marker);
    parameters
}

pub(super) fn default_stub_access(ir: &IrFile, fid: u32) -> u16 {
    let visibility = match ir.method_visibility(fid) {
        crate::types::Visibility::Private | crate::types::Visibility::PackagePrivate => 0x0000,
        // Generated nested and lambda classes invoke this helper directly; kotlinc therefore
        // exposes a protected declaration's `$default` stub publicly.
        crate::types::Visibility::Protected
        | crate::types::Visibility::Internal
        | crate::types::Visibility::Public => 0x0001,
    };
    visibility | 0x1008
}

pub(super) fn emit_default_param_overwrites(
    emitter: &mut Emitter<'_>,
    code: &mut CodeBuilder,
    defaults: &[Option<u32>],
    receiver_offset: usize,
    param_slots: &[(u16, Ty)],
    mask_slots: &[u16],
) {
    let logical_param_count = param_slots
        .len()
        .checked_sub(receiver_offset)
        .expect("an extension receiver is a leading physical parameter");
    for (index, default) in defaults.iter().enumerate().take(logical_param_count) {
        if let Some(expression) = default {
            let (slot, ty) = param_slots[index + receiver_offset];
            code.iload(mask_slots[index / 32]);
            code.push_int(default_mask_bit(index), emitter.cw);
            code.iand();
            let skip = code.new_label();
            code.ifeq(skip);
            if crate::jvm::lambda_classes::inline_default_value_requires_reification(
                emitter.ir,
                *expression,
            ) {
                let marker = emitter.cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "needClassReification",
                    "()V",
                );
                code.invokestatic(marker, 0, 0);
            }
            emitter.emit_value(*expression, code);
            let produced = ir_ty_to_jvm(&emitter.value_ty(*expression));
            emit_primitive_box_if_needed(emitter.cw, produced, ty, code);
            // A lambda class, or a `String` stored into `CharSequence`, is narrower than the
            // slot. kotlinc `checkcast`s to the slot's class; `Object` needs none, and `null`
            // is already that type.
            emit_default_store_narrowing(emitter.cw, produced, ty, code);
            store(ty, slot, code);
            code.bind(skip);
        }
    }
}

pub(super) fn body_has_reified_markers(ir: &IrFile, expression: crate::ir::ExprId) -> bool {
    if matches!(
        ir.expr(expression),
        IrExpr::ReifiedClassMarker { .. } | IrExpr::ReifiedTypeOp { .. }
    ) || ir
        .reified_catch_markers
        .get(&expression)
        .is_some_and(|markers| markers.iter().any(Option::is_some))
    {
        return true;
    }
    let mut found = false;
    crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
        found |= body_has_reified_markers(ir, child);
    });
    found
}

/// The stub evaluates default expressions, and an inline stub re-emits the body. Both use the
/// same private-access accessors as the method the splicer copies.
fn publish_exported_private_access(emitter: &mut Emitter<'_>, fid: u32) {
    let exports = super::static_accessors::method_exports_private_calls(
        emitter.ir,
        &emitter.run.static_accessor_plan.borrow(),
        fid,
    );
    emitter.method_exports_private_access = exports;
    emitter.export_private_calls = exports;
}

/// Emit a facade-style static function `$default` synthetic.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_facade_default_stub(
    ir: &IrFile,
    fid: u32,
    static_owner: StaticOwner,
    facade: &str,
    cw: &mut ClassWriter,
    defaults: &[Option<u32>],
    env: &EmitEnv,
    marker: Ty,
) {
    let function = &ir.functions[fid as usize];
    let method_name = function.name.clone();
    let real_params = jvm_function_params(ir, fid);
    let boxed = default_stub_boxed_parameters(ir, fid);
    let stub_param_tys = real_params
        .iter()
        .enumerate()
        .map(|(index, parameter)| stub_parameter_type(ir, fid, index, *parameter))
        .collect::<Vec<_>>();
    let ret = jvm_declared_ty(&function.ret);
    let receiver_prefix = usize::from(function.is_static && function.dispatch_receiver.is_some())
        + usize::from(ir.extension_receiver_fns.contains(&fid));
    let extension_prefix = usize::from(ir.extension_receiver_fns.contains(&fid));
    let logical_param_count = real_params
        .len()
        .checked_sub(receiver_prefix)
        .expect("callable receivers are leading physical parameters");
    {
        let mask_words = default_mask_count(logical_param_count);
        let descriptor = method_descriptor(
            &stub_param_tys
                .iter()
                .copied()
                .chain(std::iter::repeat_n(Ty::Int, mask_words))
                .chain(std::iter::once(marker))
                .collect::<Vec<_>>(),
            ret,
        );
        cw.seed_utf8(&format!("{method_name}$default"));
        cw.seed_utf8(&descriptor);
    }
    let mut emitter = Emitter::new(
        ir,
        cw,
        env,
        Some(static_owner),
        facade,
        facade,
        ret,
        defaults.iter().flatten().copied(),
    );
    publish_exported_private_access(&mut emitter, fid);
    let mut param_slots = Vec::new();
    for (index, ty) in stub_param_tys.iter().enumerate() {
        let slot = emitter.frame.enter(FrameKey::Value(index as u32), *ty);
        emitter.slots.insert(index as u32, (slot, *ty));
        param_slots.push((slot, *ty));
    }
    let mask_count = default_mask_count(logical_param_count);
    let mask_slots = (0..mask_count)
        .map(|mask| {
            let slot = emitter.frame.enter(
                FrameKey::Parameter((stub_param_tys.len() + mask) as u16),
                Ty::Int,
            );
            let _ = emitter.lease_temporary(slot, Ty::Int);
            slot
        })
        .collect::<Vec<_>>();
    let marker_slot = emitter.frame.enter(
        FrameKey::Parameter((stub_param_tys.len() + mask_count) as u16),
        marker,
    );
    let _ = emitter.lease_temporary(marker_slot, marker);
    let inline_default = crate::jvm::lambda_classes::has_inline_default_lambda(ir, fid);
    let mut code = CodeBuilder::new(emitter.frame.size());
    // The stub's first line is the signature. The function method itself does not record that
    // line: its table opens on the body, after the inline-depth marker.
    if inline_default {
        if let Some(&line) = ir
            .fn_sig_lines
            .get(&fid)
            .or_else(|| ir.fn_decl_lines.get(&fid))
        {
            code.mark_line(line);
        }
    }
    emit_default_param_overwrites(
        &mut emitter,
        &mut code,
        &defaults[extension_prefix..],
        receiver_prefix,
        &param_slots,
        &mask_slots,
    );
    let reified_body = function
        .body
        .filter(|&body| body_has_reified_markers(ir, body));
    let inline_body = function.body.filter(|_| inline_default);
    if let Some(body) = inline_body.or(reified_body) {
        let inline_marker = inline_default.then(|| {
            // The mask and marker parameters sit in the slots the body reuses. The default
            // prefix has already recorded its stack map against the method's entry locals, so
            // the frame cursor can return to the first of those slots. kotlinc's `$i$f$` marker
            // then overwrites the mask.
            release_default_stub_suffix(&mut emitter, stub_param_tys.len(), mask_count);
            // A default expression may have moved the current line into its own body. The ordinary
            // method-entry checks resume at the declaration signature; `mark_line` naturally
            // deduplicates this when the default and signature share one line.
            if let Some(&line) = ir
                .fn_sig_lines
                .get(&fid)
                .or_else(|| ir.fn_decl_lines.get(&fid))
            {
                code.mark_line(line);
            }
            super::method_entry::emit(fid, &real_params, false, None, env, &mut emitter, &mut code);
            ir.top_level_inline_functions.contains(&fid).then(|| {
                let slot = emitter
                    .frame
                    .enter_temp(super::frame_map::TempRole::InlineDepthMarker, Ty::Int)
                    .slot();
                code.push_int(0, emitter.cw);
                store(Ty::Int, slot, &mut code);
                (slot, code.bytes.len() as u16)
            })
        });
        if inline_default {
            // The signature line is already in effect at pc 0. The body restates that same line
            // on the expression after the inline-depth marker.
            code.forget_line();
        }
        emitter.emit(body, &mut code);
        if ret == Ty::Unit && !emitter.discarding_diverges(body) {
            code.ret_void();
        }
        if let Some(Some((slot, start))) = inline_marker {
            let marker_name = format!("$i$f${}", ir.functions[fid as usize].name);
            code.add_local_entry(start, None, slot, &marker_name, "I");
        }
        if inline_body.is_some() {
            record_stub_parameter_locals(ir, fid, &real_params, &mut code);
        }
    } else {
        for (index, &(slot, ty)) in param_slots.iter().enumerate() {
            load(ty, slot, &mut code);
            emit_adapted_unbox(
                ir,
                emitter.cw,
                &boxed,
                index,
                real_params[index],
                ty,
                &mut code,
            );
        }
        let argument_words = real_params.iter().map(|ty| slot_words(*ty) as i32).sum();
        let descriptor = method_descriptor(&real_params, ret);
        let method = emitter.cw.methodref(facade, &method_name, &descriptor);
        code.invokestatic(method, argument_words, slot_words(ret) as i32);
        emit_return(ret, &mut code);
    }
    code.ensure_locals(emitter.frame.max());
    code.link();
    let mut stub_params = stub_param_tys;
    stub_params.extend(std::iter::repeat_n(
        Ty::Int,
        default_mask_count(logical_param_count),
    ));
    stub_params.push(marker);
    let descriptor = method_descriptor(&stub_params, ret);
    emitter.cw.add_method(
        default_stub_access(ir, fid),
        &format!("{method_name}$default"),
        &descriptor,
        &code,
    );
    // An inline stub keeps the lines its body marked. Replacing them with the signature line
    // would drop the expression entry kotlinc records after the inline-depth marker.
    if !inline_default {
        if let Some(&line) = ir
            .fn_sig_lines
            .get(&fid)
            .or_else(|| ir.fn_decl_lines.get(&fid))
        {
            emitter.cw.set_method_lines(
                &format!("{method_name}$default"),
                &descriptor,
                &[(0, line)],
            );
        }
    }
    drop(emitter);
    emit_published_default_accessor(
        ir,
        fid,
        cw,
        PublishedDefaultAccessor {
            owner: facade,
            stub_name: &format!("{method_name}$default"),
            parameters: &stub_params,
            result: ret,
            is_interface: static_owner.is_interface(ir),
        },
    );
}

/// The JVM name of a call to `bridge_name` (`foo$default`). A non-private inline body publishes
/// `access$` plus that selected bridge; every other caller keeps the bridge name and descriptor.
pub(super) fn default_call_name(
    ir: &IrFile,
    export_private: bool,
    function: u32,
    bridge_name: &str,
) -> String {
    if super::static_accessors::inline_exports_private_call(ir, export_private, function) {
        format!("access${bridge_name}")
    } else {
        bridge_name.to_owned()
    }
}

/// Return the frame cursor to the first mask slot.
///
/// The default prefix has used the mask, and kotlinc's inlined body allocates its first local in
/// that slot. The temporary leases stay: stack maps at the prefix still describe the entry locals.
fn release_default_stub_suffix(
    emitter: &mut Emitter<'_>,
    parameter_count: usize,
    mask_count: usize,
) {
    emitter
        .frame
        .leave(FrameKey::Parameter((parameter_count + mask_count) as u16));
    for mask in (0..mask_count).rev() {
        emitter
            .frame
            .leave(FrameKey::Parameter((parameter_count + mask) as u16));
    }
}

/// The real parameters of an inline `$default` stub, covering the whole method. The mask and
/// marker are not named.
fn record_stub_parameter_locals(ir: &IrFile, fid: u32, param_tys: &[Ty], code: &mut CodeBuilder) {
    let function = &ir.functions[fid as usize];
    let local_names = crate::jvm::parameter_names::function_locals(ir, fid, param_tys);
    let mut slot = u16::from(!function.is_static);
    for (index, ty) in param_tys.iter().enumerate() {
        let name = local_names
            .as_ref()
            .and_then(|names| names.get(index))
            .cloned()
            .expect("a debug-published parameter needs its canonical source identity");
        if let Some(name) = name {
            code.add_local_entry(
                0,
                None,
                slot,
                &name,
                &crate::jvm::names::type_descriptor(*ty),
            );
        }
        slot += slot_words(*ty);
    }
}

/// Owner and descriptor of a `$default` bridge that a non-private inline function publishes.
struct PublishedDefaultAccessor<'a> {
    owner: &'a str,
    stub_name: &'a str,
    parameters: &'a [Ty],
    result: Ty,
    is_interface: bool,
}

/// `access$foo$default` forwards to the selected `foo$default` bridge with that bridge's
/// descriptor, masks, marker, and receiver. A copied inline body must not invent `access$foo`.
fn emit_published_default_accessor(
    ir: &IrFile,
    fid: u32,
    cw: &mut ClassWriter,
    access: PublishedDefaultAccessor<'_>,
) {
    if !private_default_published_by_inline(ir, fid) {
        return;
    }
    let descriptor = method_descriptor(access.parameters, access.result);
    let name = format!("access${}", access.stub_name);
    if cw.declares_method(&name, &descriptor) {
        return;
    }
    cw.reserve_method_name(&name);
    cw.reserve_descriptor(&descriptor);
    let words: u16 = access.parameters.iter().map(|ty| slot_words(*ty)).sum();
    let mut code = CodeBuilder::new(words);
    let mut slot = 0u16;
    for &ty in access.parameters {
        load(ty, slot, &mut code);
        slot += slot_words(ty);
    }
    let method = if access.is_interface {
        cw.interface_methodref(access.owner, access.stub_name, &descriptor)
    } else {
        cw.methodref(access.owner, access.stub_name, &descriptor)
    };
    code.invokestatic(method, i32::from(words), slot_words(access.result) as i32);
    emit_return(access.result, &mut code);
    code.ensure_locals(words);
    code.link();
    let flags = if access.is_interface { 0x1009 } else { 0x1019 };
    cw.add_method(flags, &name, &descriptor, &code);
    if let Some(&line) = ir
        .fn_sig_lines
        .get(&fid)
        .or_else(|| ir.fn_decl_lines.get(&fid))
    {
        cw.set_method_lines(&name, &descriptor, &[(0, line)]);
    }
}

/// Whether a non-private `inline` function, or a private `inline` function it expands, calls
/// `target`'s default-argument bridge. The accessor exists only for that publication.
fn private_default_published_by_inline(ir: &IrFile, target: u32) -> bool {
    if !ir.method_visibility(target).is_private() || ir.lifted_functions.contains_key(&target) {
        return false;
    }
    let mut pending = ir
        .functions
        .iter()
        .enumerate()
        .filter_map(|(index, _)| {
            let function = index as u32;
            (ir.inline_fns.contains(&function) && !ir.method_visibility(function).is_private())
                .then_some(function)
        })
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    while let Some(function) = pending.pop() {
        if !seen.insert(function) {
            continue;
        }
        let Some(body) = ir.functions[function as usize].body else {
            continue;
        };
        let mut expressions = vec![body];
        while let Some(expression) = expressions.pop() {
            if let Some(callee) = default_or_inline_callee(ir, expression) {
                if callee == target && calls_default_bridge(ir, expression, target) {
                    return true;
                }
                if ir.inline_fns.contains(&callee) {
                    pending.push(callee);
                }
            }
            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| expressions.push(child));
        }
    }
    false
}

/// The local function a call names, when this expression is a direct call.
fn default_or_inline_callee(ir: &IrFile, expression: crate::ir::ExprId) -> Option<u32> {
    match ir.expr(expression) {
        IrExpr::Call { callee, .. } => match callee {
            Callee::Local(function)
            | Callee::LocalDefault(function)
            | Callee::LocalWithDefaults { function, .. }
            | Callee::ClassStatic { function, .. }
            | Callee::ClassStaticDefault { function, .. }
            | Callee::ClassStaticWithDefaults { function, .. } => Some(*function),
            _ => None,
        },
        IrExpr::MethodCall { class, index, .. } => ir
            .classes
            .get(*class as usize)
            .and_then(|class| class.methods.get(*index as usize))
            .copied(),
        _ => None,
    }
}

fn calls_default_bridge(ir: &IrFile, expression: crate::ir::ExprId, target: u32) -> bool {
    match ir.expr(expression) {
        IrExpr::Call {
            callee: Callee::LocalDefault(function) | Callee::LocalWithDefaults { function, .. },
            ..
        }
        | IrExpr::Call {
            callee:
                Callee::ClassStaticDefault { function, .. }
                | Callee::ClassStaticWithDefaults { function, .. },
            ..
        } => *function == target,
        IrExpr::MethodCall {
            class, index, args, ..
        } => {
            args.iter().any(Option::is_none)
                && ir
                    .classes
                    .get(*class as usize)
                    .and_then(|class| class.methods.get(*index as usize))
                    .is_some_and(|function| *function == target)
        }
        _ => false,
    }
}

#[cfg(test)]
mod catch_marker_tests {
    use super::body_has_reified_markers;
    use crate::ir::{IrExpr, IrFile};
    use crate::types::{type_name, Ty};

    #[test]
    fn a_recorded_reified_catch_marks_its_try() {
        let mut ir = IrFile::default();
        let handler = ir.add_expr(IrExpr::UnitInstance);
        let thrown = ir.add_expr(IrExpr::UnitInstance);
        let expression = ir.add_expr(IrExpr::Try {
            body: thrown,
            catches: vec![crate::ir::IrCatch::generated(
                0,
                type_name("kotlin/Throwable"),
                handler,
            )],
            finally: None,
            result: Ty::Unit,
        });
        assert!(!body_has_reified_markers(&ir, expression));
        ir.reified_catch_markers
            .insert(expression, vec![Some("E".to_owned())]);
        assert!(body_has_reified_markers(&ir, expression));
        let wrapper = ir.add_expr(IrExpr::Block {
            stmts: vec![expression],
            value: None,
        });
        assert!(body_has_reified_markers(&ir, wrapper));
    }
}
