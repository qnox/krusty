//! Physical JVM emission of one common-IR value node.

use std::collections::HashMap;

use crate::ir::{Callee, IrExpr};
use crate::jvm::classfile::CodeBuilder;
use crate::jvm::names::{method_descriptor, type_descriptor};
use crate::jvm::value_classes::instance_representation;
use crate::types::Ty;

use super::{
    access_bridges, array_jvm_element, bottom_values, boxed_descriptor, bytecode_inline_call,
    class_ctor_jvm_tys, constant_emission, constructor_accessors, declared_jvm_interface,
    default_mask_bit, default_mask_count, emit_box_impl, emit_constructor_default_arguments,
    instance_field_jvm_name, ir_ty_to_jvm, jvm_declared_ty, jvm_function_interface,
    jvm_function_invoke_descriptor, jvm_function_params, jvm_tys, lambda_class, lambda_class_names,
    load, method_defaults, native_unsigned_impl_target, parse_descriptor_params,
    physical_call_result_words, prim_newarray_atype, push_zero, ref_class, singleton_instance_load,
    slot_words, try_emission, ty_from_descriptor_ret, vararg, JvmDefaultMode, LambdaClassPlan,
    LambdaMode, PropertyOperation, LMF_METAFACTORY_DESC,
};

impl super::Emitter<'_> {
    pub(super) fn emit_physical_value_node(
        &mut self,
        e: u32,
        node: &IrExpr,
        code: &mut CodeBuilder,
    ) {
        match node {
            IrExpr::BottomValue { producer, .. } => {
                let baseline = code.stack_height();
                self.emit_value(*producer, code);
                bottom_values::finish(self.cw, code, baseline, true);
            }
            // `break`/`continue` are `Nothing`-typed: in value position (e.g. `x ?: break`) they diverge
            // — emit the jump and push nothing; the consuming branch is dead past this point.
            IrExpr::Break { label } => {
                self.emit_loop_transfer(e, label, true, code);
            }
            IrExpr::Continue { label } => {
                self.emit_loop_transfer(e, label, false, code);
            }
            IrExpr::Const(constant) => constant_emission::emit(constant, code, self.cw),
            IrExpr::ForwardedSuperArgument { .. } => {
                self.run.set_emit_error(
                    "anonymous super forward was not bound to its constructor parameter"
                        .to_string(),
                );
            }
            IrExpr::ClassConst { internal } => {
                let name = internal
                    .as_ref()
                    .map_or_else(|| self.facade.clone(), |name| name.render());
                code.ldc_class(&name, self.cw);
            }
            IrExpr::KClassLiteral {
                classifier,
                value,
                type_argument,
            } => self.emit_kclass_literal(*classifier, *value, *type_argument, code),
            IrExpr::GetValue(i) => {
                // A slot that was never allocated means the lowering produced malformed IR (e.g. an
                // unsupported suspend shape). Don't panic — flag the file unemittable and skip it.
                let Some(&(slot, jt)) = self.slots.get(i) else {
                    crate::trace_compiler!(
                        "suspend",
                        "EMIT_BAIL GetValue unallocated slot i={i} owner={} known={:?}",
                        self.owner,
                        self.slots.keys().collect::<Vec<_>>()
                    );
                    self.run.set_emit_error(
                        "value read references a slot that was never declared".to_string(),
                    );
                    return;
                };
                self.load_constructor_value(*i, jt, slot, code);
            }
            IrExpr::PropertyRead {
                receiver,
                owner,
                name,
                ty,
                interface,
                operation,
            } => {
                let (receiver, name, ty, interface, operation) = (
                    *receiver,
                    name.clone(),
                    *ty,
                    *interface,
                    operation.unwrap_or(e),
                );
                self.emit_property_read(
                    PropertyOperation {
                        expression: operation,
                        receiver,
                        owner: *owner,
                        name: &name,
                        ty: &ty,
                        interface,
                    },
                    code,
                );
            }
            IrExpr::PropertyWrite {
                receiver,
                owner,
                name,
                value,
                ty,
                interface,
                operation,
            } => {
                let (receiver, name, value, ty, interface, operation) = (
                    *receiver,
                    name.clone(),
                    *value,
                    *ty,
                    *interface,
                    operation.unwrap_or(e),
                );
                self.emit_property_write(
                    PropertyOperation {
                        expression: operation,
                        receiver,
                        owner: *owner,
                        name: &name,
                        ty: &ty,
                        interface,
                    },
                    value,
                    code,
                );
            }
            IrExpr::EnclosingInstance {
                receiver,
                inner,
                outer,
            } => {
                self.emit_value(*receiver, code);
                let outer = instance_representation(self.ir, *outer);
                let fref = self
                    .cw
                    .fieldref(&inner.render(), "this$0", &type_descriptor(outer));
                code.getfield(fref, slot_words(outer) as i32);
            }
            IrExpr::GetField {
                receiver,
                class,
                index,
            } => self.emit_get_field(*receiver, *class, *index, code),
            IrExpr::LateinitInitialized {
                receiver,
                class,
                index,
            } => {
                // The RAW field read — no throw-if-null guard, which is the whole point: this node
                // exists so `::prop.isInitialized` can TEST the field a normal read would reject.
                // The null comparison itself is built in lowering from the ordinary comparison node,
                // so the branch/stackmap shape stays the one every other comparison uses.
                let c = &self.ir.classes[*class as usize];
                let name = instance_field_jvm_name(self.ir, c, &c.fields[*index as usize]);
                let fty = c.fields[*index as usize].ty;
                let jt = jvm_declared_ty(&fty);
                let owner = c.fq_name();
                self.emit_value(*receiver, code);
                let fref = self.cw.fieldref(&owner, &name, &type_descriptor(jt));
                code.getfield(fref, slot_words(jt) as i32);
            }
            IrExpr::GetStatic(i) => self.emit_get_static(*i, e, code),
            IrExpr::New {
                internal,
                args,
                ctor_params,
                ctor_desc,
                external_target: _,
                defaults: default_parameters,
                default_prefix_count,
            } => {
                let args = args.clone();
                if self.emit_nullable_sam_wrapper_new(e, *internal, &args, code) {
                    return;
                }
                let owner = internal.render();
                // The constructor descriptor + its argument-word count come from ONE source, identified by
                // the owner NAME (no same-file/other-file/classpath control-flow split):
                //  - a verbatim descriptor (`ctor_desc`) for a classpath ctor whose signature isn't modeled
                //    as `Ty`s — arg words come from each argument's own value type; OR
                //  - the known parameter types: the node's `ctor_params`, else the named in-IR class's
                //    primary-ctor field types.
                let (desc, use_accessor, base_parameter_count, source_parameter_count) =
                    if let Some(d) = ctor_desc {
                        debug_assert!(default_parameters.is_empty());
                        (d.clone(), false, args.len(), args.len())
                    } else {
                        let mut field_tys: Vec<Ty> = match ctor_params {
                            Some(ps) => jvm_tys(ps),
                            None => self
                                .ir
                                .class_id_by_name(*internal)
                                .map(|c| class_ctor_jvm_tys(&self.ir.classes[c as usize]))
                                .unwrap_or_default(),
                        };
                        // A class whose primary ctor takes a value-class param has a PRIVATE primary + a
                        // PUBLIC|SYNTHETIC accessor `(…args, DefaultConstructorMarker)`. Every construction
                        // routes through the accessor (a trailing `null`) — from the class itself too
                        // (`copy`, a member building a sibling instance): kotlinc leaves only the
                        // accessor calling the private primary. A selected secondary constructor uses
                        // the declaration fact recorded on this exact expression before erasure.
                        // Another class constructing through a private constructor does the same.
                        // A call supplying defaults targets the public `$default` overload instead,
                        // which reaches the accessor itself.
                        let use_accessor = constructor_accessors::uses_value_class_marker_accessor(
                            self.ir,
                            e,
                            *internal,
                            &owner,
                            default_parameters.is_empty(),
                            ctor_params.is_none(),
                        );
                        let base_parameter_count = field_tys.len();
                        let source_parameter_count = base_parameter_count
                            .checked_sub(*default_prefix_count as usize)
                            .expect("constructor default prefix exceeds its parameters");
                        if use_accessor {
                            field_tys.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
                        }
                        if !default_parameters.is_empty() {
                            field_tys.extend(std::iter::repeat_n(
                                Ty::Int,
                                default_mask_count(source_parameter_count),
                            ));
                            field_tys.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
                        }
                        (
                            method_descriptor(&field_tys, Ty::Unit),
                            use_accessor,
                            base_parameter_count,
                            source_parameter_count,
                        )
                    };
                let physical_params = self.constructor_physical_params(e, &desc);
                let aw = physical_params.iter().map(|t| slot_words(*t) as i32).sum();
                if args.iter().any(|&a| self.spills_operand_prefix(a)) {
                    // An argument that enters a handler, suspends, or leaves for a loop target can't
                    // run with `[new, dup]` on the stack. Evaluate all args into temps first (clean
                    // stack), then build. An ordinary branch keeps them, as kotlinc does.
                    let temps = self.spill_to_temps(&args, code);
                    let ci = self.cw.class_ref(&owner);
                    crate::jvm::reified_anonymous::guard_reified_construction(
                        self.ir, self.cw, code, *internal,
                    );
                    code.new_obj(ci);
                    code.dup();
                    let mut supplied = temps.iter().zip(args.iter());
                    // A defaulted construction takes the same rule as a defaulted call: the
                    // operands its `$default` ABI invents — a placeholder for an omitted argument,
                    // and the trailing marker/mask/marker group — belong to the CONSTRUCTION, so
                    // its own line goes back into effect at the start of each such run rather than
                    // only at the `invokespecial` after all of them.
                    let mut inside_run = false;
                    for (parameter, physical) in physical_params
                        .iter()
                        .copied()
                        .take(base_parameter_count)
                        .enumerate()
                    {
                        let omitted = parameter >= *default_prefix_count as usize
                            && default_parameters.contains(
                                &u32::try_from(parameter - *default_prefix_count as usize)
                                    .expect("too many constructor parameters"),
                            );
                        self.mark_synthesized_run_start(e, omitted, &mut inside_run, code);
                        if omitted {
                            push_zero(physical, code, self.cw);
                        } else {
                            let ((slot, ty, _), argument) = supplied
                                .next()
                                .unwrap_or_else(|| {
                                    panic!(
                                        "checked constructor supplied-argument count: owner={owner} \
                                         supplied={} physical={base_parameter_count} defaults={default_parameters:?} \
                                         prefix={default_prefix_count} expression={e} origin={:?}",
                                        args.len(),
                                        self.ir.fir_origins.get(&e),
                                    )
                                });
                            load(*ty, *slot, code);
                            self.adapt_physical_constructor_operand_for(
                                e, parameter, *argument, *ty, physical, code,
                            );
                        }
                    }
                    self.release_operand_spills(&temps);
                    if use_accessor || !default_parameters.is_empty() {
                        self.mark_synthesized_run_start(e, true, &mut inside_run, code);
                    }
                    if use_accessor {
                        code.aconst_null();
                    }
                    emit_constructor_default_arguments(
                        default_parameters,
                        source_parameter_count,
                        code,
                        self.cw,
                    );
                    let m = self.cw.methodref(&owner, "<init>", &desc);
                    self.mark_call_start(e, code);
                    code.invokespecial(m, aw, 0);
                } else {
                    let ci = self.cw.class_ref(&owner);
                    crate::jvm::reified_anonymous::guard_reified_construction(
                        self.ir, self.cw, code, *internal,
                    );
                    code.new_obj(ci);
                    code.dup();
                    let mut supplied = args.iter().copied();
                    let mut inside_run = false;
                    for (parameter, physical) in physical_params
                        .iter()
                        .copied()
                        .take(base_parameter_count)
                        .enumerate()
                    {
                        let omitted = parameter >= *default_prefix_count as usize
                            && default_parameters.contains(
                                &u32::try_from(parameter - *default_prefix_count as usize)
                                    .expect("too many constructor parameters"),
                            );
                        self.mark_synthesized_run_start(e, omitted, &mut inside_run, code);
                        if omitted {
                            push_zero(physical, code, self.cw);
                        } else {
                            let argument = supplied
                                .next()
                                .unwrap_or_else(|| {
                                    panic!(
                                        "checked constructor supplied-argument count: owner={owner} \
                                         supplied={} physical={base_parameter_count} defaults={default_parameters:?} \
                                         prefix={default_prefix_count} expression={e} origin={:?}",
                                        args.len(),
                                        self.ir.fir_origins.get(&e),
                                    )
                                });
                            self.emit_value(argument, code);
                            self.adapt_physical_constructor_operand_for(
                                e,
                                parameter,
                                argument,
                                self.value_ty(argument),
                                physical,
                                code,
                            );
                        }
                    }
                    if use_accessor || !default_parameters.is_empty() {
                        self.mark_synthesized_run_start(e, true, &mut inside_run, code);
                    }
                    if use_accessor {
                        code.aconst_null();
                    }
                    emit_constructor_default_arguments(
                        default_parameters,
                        source_parameter_count,
                        code,
                        self.cw,
                    );
                    let m = self.cw.methodref(&owner, "<init>", &desc);
                    self.mark_call_start(e, code);
                    code.invokespecial(m, aw, 0);
                }
            }
            IrExpr::MethodCall {
                class,
                index,
                receiver,
                args,
            } => {
                let c = &self.ir.classes[*class as usize];
                let fid = c.methods[*index as usize];
                let f = &self.ir.functions[fid as usize];
                let param_tys = jvm_function_params(self.ir, fid);
                let ret = jvm_declared_ty(&self.override_results.physical_result(self.ir, fid));
                let name = f.name.clone();
                let owner = c.fq_name();
                let is_iface = c.is_interface;
                if args.iter().any(|a| a.is_none()) {
                    // Some arguments are omitted — invoke the `<name>$default(self, params…, mask, marker)`
                    // stub: receiver, each provided arg (or a zero placeholder for an omitted one with its
                    // mask bit set), the mask, then a null marker. A nullable-underlying value-class param
                    // is BOXED in the stub signature (matching `emit_default_stub`), so a provided arg is
                    // `box-impl`d and the placeholder/descriptor use the boxed type. A non-null type
                    // parameter bounded by a JVM primitive is likewise the JDK wrapper on the stub:
                    // the placeholder is `valueOf` of the primitive zero, and a supplied primitive is
                    // boxed the same way.
                    let boxed: HashMap<usize, Ty> = self
                        .ir
                        .default_stub_boxed_params
                        .get(&fid)
                        .map(|v| v.iter().copied().collect())
                        .unwrap_or_default();
                    let stub_param_tys: Vec<Ty> = param_tys
                        .iter()
                        .enumerate()
                        .map(|(i, t)| method_defaults::stub_parameter_type(self.ir, fid, i, *t))
                        .collect();
                    let args = args.clone();
                    self.emit_value(*receiver, code);
                    // Mask bits are LOGICAL (kotlinc numbers them over the declared value
                    // parameters) — a member EXTENSION's physical receiver at params[0] does not
                    // shift them, and is never omitted.
                    let recv_offset = usize::from(self.ir.extension_receiver_fns.contains(&fid));
                    let logical_param_count = param_tys
                        .len()
                        .checked_sub(recv_offset)
                        .expect("an extension receiver is a leading physical parameter");
                    let mut masks = vec![0i32; default_mask_count(logical_param_count)];
                    // Every operand the CALL synthesizes — a placeholder standing in for an omitted
                    // argument, and the trailing mask/marker group — carries the call's own line, so
                    // an argument supplied between two of them puts its own line in effect and the
                    // next group restores the call's. A call that omits nothing synthesizes nothing,
                    // which is why an ordinary call's dispatch mark sits at its invoke.
                    for (i, arg) in args.iter().enumerate() {
                        match arg {
                            Some(a) => {
                                self.emit_value(*a, code);
                                if let Some(vc) = boxed.get(&i) {
                                    emit_box_impl(self.ir, self.cw, vc, code);
                                }
                                method_defaults::emit_primitive_box_if_needed(
                                    self.cw,
                                    ir_ty_to_jvm(&self.value_ty(*a)),
                                    stub_param_tys[i],
                                    code,
                                );
                            }
                            None => {
                                self.mark_dispatch_line(e, code);
                                method_defaults::emit_omitted_default_placeholder(
                                    self.cw,
                                    param_tys[i],
                                    stub_param_tys[i],
                                    code,
                                );
                                let li = i
                                    .checked_sub(recv_offset)
                                    .expect("an extension receiver cannot be omitted");
                                masks[li / 32] |= default_mask_bit(li);
                            }
                        }
                    }
                    self.mark_dispatch_line(e, code);
                    for mask in masks {
                        code.push_int(mask, self.cw);
                    }
                    code.aconst_null();
                    let mut stub_params = vec![Ty::obj_name(c.fq_name)];
                    stub_params.extend(stub_param_tys.iter().copied());
                    stub_params.extend(std::iter::repeat_n(
                        Ty::Int,
                        default_mask_count(logical_param_count),
                    ));
                    stub_params.push(Ty::obj("java/lang/Object"));
                    let aw: i32 = stub_params.iter().map(|t| slot_words(*t) as i32).sum();
                    let stub_desc = method_descriptor(&stub_params, ret);
                    let plain_stub = format!("{name}$default");
                    // The `$default` stub of an INTERFACE method is a STATIC interface method —
                    // referenced via an `InterfaceMethodref` constant (a plain `Methodref` is an
                    // `IncompatibleClassChangeError`), still invoked with `invokestatic`. Under
                    // `enable`/`no-compatibility` kotlinc puts that stub on the interface and call
                    // sites use it; under `disable` the interface holds nothing executable and the
                    // stub exists only on `<Iface>$DefaultImpls`, so a call site aimed at the
                    // interface would link to a method that was never emitted. The inline
                    // `access$` bridge is published on the owner next to its `$default` stub, so
                    // the holder path keeps the stub name.
                    let holder;
                    let (stub_owner, stub_on_interface, stub_name) =
                        if is_iface && self.jvm_default == JvmDefaultMode::Disable {
                            holder = format!("{owner}$DefaultImpls");
                            (&holder, false, plain_stub)
                        } else {
                            let stub_name = method_defaults::default_call_name(
                                self.ir,
                                self.export_private_calls,
                                fid,
                                &plain_stub,
                            );
                            (&owner, is_iface, stub_name)
                        };
                    let m = if stub_on_interface {
                        self.cw
                            .interface_methodref(stub_owner, &stub_name, &stub_desc)
                    } else {
                        self.cw.methodref(stub_owner, &stub_name, &stub_desc)
                    };
                    self.mark_call_start(e, code);
                    code.invokestatic(m, aw, physical_call_result_words(ret));
                    return;
                }
                let call_args: Vec<u32> = args.iter().map(|a| a.unwrap()).collect();
                let protected_bridge = self
                    .run
                    .protected_member_access_bridges
                    .borrow()
                    .get(&e)
                    .cloned();
                let call_param_tys = protected_bridge
                    .as_ref()
                    .map_or(param_tys.as_slice(), |bridge| {
                        bridge.bridge_parameters.as_slice()
                    });
                // An argument-count/descriptor mismatch can only come from a pass that rewrote the
                // callee's ABI without fixing this call site (a suspend call the coroutine flattener
                // failed to thread a continuation into — an unmodeled shape). Never emit the
                // unverifiable call: the operand contract refuses, this arm bails the file (the gate
                // SKIPS it) and pushes a typed zero so the dead code that follows still assembles.
                let desc = method_descriptor(&param_tys, ret);
                let operand_result = if let Some(bridge) = protected_bridge.as_ref() {
                    let mut operands = Vec::with_capacity(call_args.len() + 1);
                    operands.push(*receiver);
                    operands.extend(call_args.iter().copied());
                    let mut physical = Vec::with_capacity(call_param_tys.len() + 1);
                    physical.push(Ty::obj_name(bridge.owner));
                    physical.extend(call_param_tys.iter().copied());
                    self.emit_source_access_bridge_operands(&operands, &physical, code)
                } else {
                    self.emit_descriptor_virtual_operands(
                        e,
                        crate::jvm::ir_emit::call_operands::VirtualCallTarget {
                            owner: &owner,
                            name: &name,
                            descriptor: &desc,
                        },
                        *receiver,
                        &call_args,
                        call_param_tys,
                        code,
                    )
                };
                if let Err(mismatch) = operand_result {
                    self.bail_descriptor_arity(&mismatch, ret, code);
                    return;
                }
                let aw: i32 = call_param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                crate::trace_compiler!(
                    "resolve",
                    "emit MethodCall {}.{} fid={fid} private={} iface={is_iface}",
                    owner,
                    name,
                    self.ir.method_visibility(fid).is_private()
                );
                if let Some(bridge) = protected_bridge {
                    let mut bridge_params = Vec::with_capacity(bridge.bridge_parameters.len() + 1);
                    bridge_params.push(Ty::obj_name(bridge.owner));
                    bridge_params.extend(bridge.bridge_parameters.iter().copied());
                    let bridge_desc = method_descriptor(&bridge_params, ret);
                    let bridge_name = format!("access${name}");
                    let method =
                        self.cw
                            .methodref(&bridge.owner.render(), &bridge_name, &bridge_desc);
                    self.mark_call_start(e, code);
                    code.invokestatic(method, aw + 1, physical_call_result_words(ret));
                } else if self.ir.method_visibility(fid).is_private() {
                    // A PRIVATE method is non-virtual — `invokespecial` (an interface private method uses an
                    // `InterfaceMethodref`), so it never dispatches to a same-named override. Under
                    // `disable` the body moved to the holder, and an `invokespecial` naming the
                    // interface from another class is not even verifiable.
                    if (self.owner != owner || self.export_private_calls)
                        && self
                            .run
                            .private_member_access_bridges
                            .borrow()
                            .contains(&fid)
                    {
                        let mut bridge_params = Vec::with_capacity(param_tys.len() + 1);
                        bridge_params.push(Ty::obj_name(c.fq_name));
                        bridge_params.extend(param_tys.iter().copied());
                        let bridge_desc = method_descriptor(&bridge_params, ret);
                        let bridge_name = format!("access${name}");
                        let method = if is_iface {
                            self.cw
                                .interface_methodref(&owner, &bridge_name, &bridge_desc)
                        } else {
                            self.cw.methodref(&owner, &bridge_name, &bridge_desc)
                        };
                        self.mark_call_start(e, code);
                        code.invokestatic(method, aw + 1, physical_call_result_words(ret));
                    } else if let Some((holder, holder_desc)) = is_iface
                        .then(|| self.holder_call(&owner, &desc, true))
                        .flatten()
                    {
                        let m = self.cw.methodref(&holder, &name, &holder_desc);
                        self.mark_call_start(e, code);
                        code.invokestatic(m, aw + 1, physical_call_result_words(ret));
                    } else {
                        let m = if is_iface {
                            self.cw.interface_methodref(&owner, &name, &desc)
                        } else {
                            self.cw.methodref(&owner, &name, &desc)
                        };
                        self.mark_call_start(e, code);
                        code.invokespecial(m, aw, physical_call_result_words(ret));
                    }
                } else {
                    let Some((owner, is_iface)) =
                        self.module_member_call_owner(e, c.fq_name_id(), is_iface)
                    else {
                        return;
                    };
                    let (name, desc, result_narrow) = self
                        .ir
                        .jvm_overridden_call_realizations
                        .get(&e)
                        .map_or_else(
                            || (name, desc.clone(), None),
                            |realization| {
                                let narrow = (realization.descriptor != desc && ret.is_reference())
                                    .then(|| crate::jvm::names::instanceof_internal_name(ret));
                                (
                                    realization.physical_name.clone(),
                                    realization.descriptor.clone(),
                                    narrow,
                                )
                            },
                        );
                    if is_iface {
                        // Dispatch through an interface — `invokeinterface I.m`.
                        let m = self.cw.interface_methodref(&owner, &name, &desc);
                        self.mark_call_start(e, code);
                        code.invokeinterface(m, aw, physical_call_result_words(ret));
                    } else {
                        let m = self.cw.methodref(&owner, &name, &desc);
                        self.mark_call_start(e, code);
                        code.invokevirtual(m, aw, physical_call_result_words(ret));
                    }
                    if let Some(internal) = result_narrow {
                        let class = self.cw.class_ref(&internal);
                        code.checkcast(class);
                    }
                }
            }
            IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } => match callee {
                // `jvm::module_calls::realize` rewrites every `super` call into
                // `Callee::Special` before emission.
                Callee::Super { .. } => {
                    unreachable!("a super call must be realized before JVM emission")
                }
                Callee::Local(fid) => {
                    self.emit_static_function_call(e, *fid, None, args, code);
                }
                Callee::ClassStatic { owner, function } => {
                    self.emit_static_function_call(e, *function, Some(*owner), args, code);
                }
                Callee::ClassStaticDefault { owner, function } => {
                    let f = &self.ir.functions[*function as usize];
                    let param_tys = method_defaults::static_default_stub_params(self.ir, *function);
                    let ret = jvm_declared_ty(&f.ret);
                    if let Err(mismatch) =
                        self.emit_source_default_call_operands(e, args, &param_tys, code)
                    {
                        self.bail_descriptor_arity(&mismatch, ret, code);
                        return;
                    }
                    let argument_words: i32 =
                        param_tys.iter().map(|ty| slot_words(*ty) as i32).sum();
                    let descriptor = method_descriptor(&param_tys, ret);
                    let owner = owner.render();
                    let method = self.cw.methodref(
                        &owner,
                        &method_defaults::default_call_name(
                            self.ir,
                            self.export_private_calls,
                            *function,
                            &format!("{}$default", f.name),
                        ),
                        &descriptor,
                    );
                    self.mark_call_start(e, code);
                    code.invokestatic(method, argument_words, physical_call_result_words(ret));
                }
                Callee::LocalDefault(fid) => {
                    // The `foo$default(realparams, mask..., Object marker)` synthetic on the self facade
                    // (emitted by `emit_facade_default_stub`). Args already include mask words + marker.
                    let f = &self.ir.functions[*fid as usize];
                    let param_tys = method_defaults::static_default_stub_params(self.ir, *fid);
                    let ret = jvm_declared_ty(&f.ret);
                    let name = method_defaults::default_call_name(
                        self.ir,
                        self.export_private_calls,
                        *fid,
                        &format!("{}$default", f.name),
                    );
                    let args = args.clone();
                    if let Err(mismatch) =
                        self.emit_source_default_call_operands(e, &args, &param_tys, code)
                    {
                        self.bail_descriptor_arity(&mismatch, ret, code);
                        return;
                    }
                    let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                    let owner = self.facade.clone();
                    let m = self
                        .cw
                        .methodref(&owner, &name, &method_descriptor(&param_tys, ret));
                    self.mark_call_start(e, code);
                    code.invokestatic(m, aw, physical_call_result_words(ret));
                }
                Callee::Intrinsic { operation, .. } => match operation {
                    crate::ir::IrIntrinsic::Assert { mode } => {
                        self.emit_assertion(*mode, args, code)
                    }
                    crate::ir::IrIntrinsic::TypeOf { ty } => {
                        let parameters =
                            super::super::type_of::TypeParameters::new(self.ir, &self.facade);
                        let mut instructions = Vec::new();
                        match super::super::type_of::generate(*ty, &parameters, &mut instructions) {
                            Ok(()) => super::super::type_of::encode(&instructions, code, self.cw),
                            Err(error) => self.run.set_emit_error(error.to_string()),
                        }
                    }
                    crate::ir::IrIntrinsic::ArrayGet => {
                        self.emit_array_get(dispatch_receiver.unwrap(), args[0], code)
                    }
                    crate::ir::IrIntrinsic::ArraySet => {
                        self.emit_array_set(dispatch_receiver.unwrap(), args[0], args[1], code)
                    }
                    op @ (crate::ir::IrIntrinsic::ArraySize
                    | crate::ir::IrIntrinsic::StringGet
                    | crate::ir::IrIntrinsic::StringLength
                    | crate::ir::IrIntrinsic::EnumName
                    | crate::ir::IrIntrinsic::NullableAnyToString) => {
                        self.emit_builtin_member(e, op, dispatch_receiver.unwrap(), args, code)
                    }
                    crate::ir::IrIntrinsic::EnumEntries { classifier } => {
                        self.emit_enum_entries(*classifier, code);
                    }
                    crate::ir::IrIntrinsic::EnumValueOf { classifier } => {
                        self.emit_enum_value_of(e, *classifier, args[0], code);
                    }
                    crate::ir::IrIntrinsic::PrimitiveCompare { operand, .. } => {
                        let receiver = dispatch_receiver
                            .expect("checked primitive compare has a dispatch receiver");
                        let [argument] = args.as_slice() else {
                            unreachable!("checked primitive compare has one argument")
                        };
                        self.emit_value(receiver, code);
                        self.emit_value(*argument, code);
                        let (owner, descriptor) = match *operand {
                            Ty::Int => ("java/lang/Integer", "(II)I"),
                            Ty::Long => ("java/lang/Long", "(JJ)I"),
                            Ty::Float => ("java/lang/Float", "(FF)I"),
                            Ty::Double => ("java/lang/Double", "(DD)I"),
                            _ => unreachable!(
                                "checked primitive compare carries a scalar comparison operand"
                            ),
                        };
                        let method = self.cw.methodref(owner, "compare", descriptor);
                        // This intrinsic's lowering IS a call, so it takes the dispatch rule: the
                        // source call's line returns at the `invokestatic`, after the operands have
                        // marked theirs. Intrinsics lowered to an instruction instead — an array
                        // read, an arithmetic op — deliberately do not.
                        self.mark_dispatch_line(e, code);
                        code.invokestatic(method, (slot_words(*operand) * 2) as i32, 1);
                    }
                    crate::ir::IrIntrinsic::CoroutineContext => {
                        unreachable!(
                            "CoroutineContext intrinsic must be realized by the CPS pass before emit"
                        )
                    }
                    // kotlinc's string concatenation renders a value-class operand through the
                    // class's own `toString-impl`, not the `toString()` intrinsic an explicit call
                    // selects.
                    crate::ir::IrIntrinsic::UnsignedToString { source } => {
                        let receiver = dispatch_receiver.expect("unsigned toString has a receiver");
                        let (owner, carrier) = native_unsigned_impl_target(*source)
                            .expect("checked unsigned conversion carries unsigned type");
                        self.emit_value(receiver, code);
                        let descriptor = method_descriptor(&[carrier], Ty::String);
                        let method =
                            self.cw
                                .methodref(&owner.render(), "toString-impl", &descriptor);
                        code.invokestatic(method, slot_words(carrier) as i32, 1);
                    }
                    crate::ir::IrIntrinsic::PrimitiveArrayNew { element } => {
                        self.emit_value(args[0], code);
                        code.newarray(prim_newarray_atype(*element));
                    }
                    crate::ir::IrIntrinsic::GeneratedPropertyEquals { ty } => {
                        let left = args[0];
                        let right = args[1];
                        if self.emit_value_class_property_equals(*ty, left, right, code) {
                            return;
                        }
                        self.emit_value(left, code);
                        self.box_scalar_operand(self.value_ty(left), code);
                        self.emit_value(right, code);
                        self.box_scalar_operand(self.value_ty(right), code);
                        let method = self.cw.methodref(
                            "kotlin/jvm/internal/Intrinsics",
                            "areEqual",
                            "(Ljava/lang/Object;Ljava/lang/Object;)Z",
                        );
                        code.invokestatic(method, 2, 1);
                    }
                    crate::ir::IrIntrinsic::GeneratedPropertyHash { ty } => {
                        self.emit_generated_property_hash(*ty, args[0], code)
                    }
                    crate::ir::IrIntrinsic::Ieee754Equals { operand } => {
                        self.emit_nullable_ieee754_equals(e, *operand, args, code)
                    }
                    crate::ir::IrIntrinsic::DataClassArrayToString { ty } => {
                        self.emit_value(args[0], code);
                        let descriptor =
                            crate::jvm::array_representation::arrays_to_string_descriptor(*ty);
                        let method = self
                            .cw
                            .methodref("java/util/Arrays", "toString", &descriptor);
                        code.invokestatic(method, 1, 1);
                    }
                },
                Callee::CrossFile {
                    facade,
                    name,
                    params,
                    ret,
                    module_target,
                    ..
                } => {
                    // A top-level function from another file → `invokestatic <facade>.<name>(desc)`.
                    let param_tys = jvm_tys(params);
                    let ret = jvm_declared_ty(ret);
                    let owner_is_interface =
                        super::super::module_calls::static_owner_is_jvm_interface(
                            self.ir,
                            *facade,
                            *module_target,
                        );
                    let (facade, name) = (facade.render(), name.clone());
                    // A private function is reachable from this file only because a non-private
                    // inline function called it. The caller's class file uses the public accessor
                    // the declaring file published; naming the private method is illegal.
                    let name = if access_bridges::private_module_callable(self.ir, *module_target) {
                        format!("access${name}")
                    } else {
                        name
                    };
                    let args = args.clone();
                    if let Err(mismatch) =
                        self.emit_call_descriptor_operands(e, 0, &args, &param_tys, code)
                    {
                        self.bail_descriptor_arity(&mismatch, ret, code);
                        return;
                    }
                    let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                    let desc = method_descriptor(&param_tys, ret);
                    // `-jvm-default=disable` puts the `$default` synthetic on the holder, not on the
                    // interface, so a call site aimed at the interface links to a method that was
                    // never emitted (`NoSuchMethodError` on the first defaulted call).
                    let holder;
                    let (target_owner, owner_is_interface) = if self.jvm_default
                        == JvmDefaultMode::Disable
                        && owner_is_interface
                        && name.ends_with("$default")
                    {
                        holder = format!("{facade}$DefaultImpls");
                        (&holder, false)
                    } else {
                        (&facade, owner_is_interface)
                    };
                    let m = if owner_is_interface {
                        self.cw.interface_methodref(target_owner, &name, &desc)
                    } else {
                        self.cw.methodref(target_owner, &name, &desc)
                    };
                    self.mark_call_start(e, code);
                    code.invokestatic(m, aw, physical_call_result_words(ret));
                }
                Callee::Module { .. }
                | Callee::ModuleWithDefaults { .. }
                | Callee::LocalWithDefaults { .. }
                | Callee::ClassStaticWithDefaults { .. }
                | Callee::External { .. } => {
                    unreachable!("stable calls must be realized before JVM emission")
                }
                Callee::Static {
                    owner,
                    name,
                    descriptor,
                    inline,
                } => {
                    let owner_identity = *owner;
                    let (owner, name, descriptor, inline) = (
                        owner.render(),
                        name.clone(),
                        self.physical_call_descriptor(e, descriptor),
                        *inline,
                    );
                    let args = args.clone();
                    crate::trace_compiler!(
                        "resolve",
                        "emit static {owner}.{name}{descriptor} inline={inline:?}"
                    );
                    let reified = crate::jvm::reified_operations::splice_arguments(
                        self.ir,
                        e,
                        &self.facade,
                        &|ty| self.rendered_inlined_cast_target(ty),
                    );
                    // `@InlineOnly`/non-public inline functions must splice. Public inline functions have
                    // callable bytecode, so a failed optional splice can fall back to a real call. An
                    // ordinary `$default` synthetic is an ABI dispatcher whose mask prologue must run
                    // as emitted. Only a metadata-normalized splice-only declaration may bypass it.
                    if inline.can_inline() && (!name.ends_with("$default") || inline.must_inline())
                    {
                        let call_frame = self.frame.mark();
                        let spliced = self.try_splice_static_inline(
                            bytecode_inline_call::StaticSpliceRequest {
                                call_expression: e,
                                owner: &owner,
                                name: &name,
                                descriptor: &descriptor,
                                args: &args,
                                dispatch_receiver: *dispatch_receiver,
                                inline,
                                reified: &reified,
                            },
                            code,
                        );
                        // The call's temporaries go with its frame, as kotlinc's `leaveTemps` does.
                        self.frame.drop_to(call_frame);
                        if spliced {
                            return;
                        }
                        // The selected declaration already owns fallback legality. `MustInline`
                        // includes both inaccessible `@InlineOnly` bodies and metadata-declared
                        // reified functions; a substitution map is only a specialization operand.
                        // An operand elided because the body never reads it needs the splice too.
                        let elided = args
                            .iter()
                            .any(|&arg| self.ir.is_unread_inline_operand(arg));
                        if inline.must_inline() || elided {
                            crate::trace_compiler!(
                                "emit",
                                "inline splice failed for {owner}.{name}{descriptor}"
                            );
                            self.run.set_inline_bail("inline splice failed");
                        }
                    }
                    let mut physical_params = parse_descriptor_params(&descriptor)
                        .expect("static call descriptor must be valid");
                    let (physical_args, leading_non_argument_operands) =
                        match dispatch_receiver.as_ref() {
                            Some(&recv) if physical_params.len() == args.len() + 1 => {
                                (self.receiver_operands(recv, &args, &mut physical_params), 1)
                            }
                            _ => (args, 0),
                        };
                    let ret = ty_from_descriptor_ret(&descriptor);
                    if let Err(mismatch) = self.emit_call_descriptor_operands(
                        e,
                        leading_non_argument_operands,
                        &physical_args,
                        &physical_params,
                        code,
                    ) {
                        self.bail_descriptor_arity(&mismatch, ret, code);
                        return;
                    }
                    let aw: i32 = physical_params.iter().map(|t| slot_words(*t) as i32).sum();
                    // A static method DECLARED ON AN INTERFACE (a Kotlin interface's `foo$default` synthetic,
                    // reached when a call omits an interface-declared default) must be an `InterfaceMethodref`
                    // even for `invokestatic` — else the JVM throws `IncompatibleClassChangeError`. Classes
                    // (stdlib facades, the common case) stay `Methodref`.
                    let owner_is_interface = self.bodies.owner_is_interface_name(owner_identity);
                    // A private value-class `-impl` another class calls goes through its bridge.
                    let bridged = self.ir.jvm_member_targets.get(&e);
                    let name = match bridged
                        .filter(|&&function| self.reaches_through_bridge(owner_identity, function))
                    {
                        Some(_) => format!("access${name}"),
                        None => name,
                    };
                    let m = if owner_is_interface {
                        self.cw.interface_methodref(&owner, &name, &descriptor)
                    } else {
                        self.cw.methodref(&owner, &name, &descriptor)
                    };
                    self.mark_call_start(e, code);
                    code.invokestatic(m, aw, slot_words(ret) as i32);
                }
                Callee::Virtual {
                    owner,
                    name,
                    descriptor,
                    params,
                    interface,
                    module_target,
                    target: _,
                } => {
                    let recv = dispatch_receiver.expect("virtual call needs a receiver");
                    let interface = *interface || declared_jvm_interface(self.ir, *owner);
                    // A sibling-file user method carries its signature as `Ty`s (`params`): build the
                    // descriptor and emit a plain virtual/interface call. The classpath-operator
                    // special-casing below only applies to the `descriptor` form (a classpath receiver).
                    if let Some((param_tys, ret_ty)) = params {
                        let owner_identity = *owner;
                        // Array `clone` names the JVM array type. That decision reads the receiver's
                        // type; the dispatch classifier still selects every other owner.
                        let semantic_receiver = self.value_ty(recv);
                        let Some((owner, interface)) = self.source_virtual_call_owner(
                            e,
                            owner_identity,
                            interface,
                            semantic_receiver,
                        ) else {
                            return;
                        };
                        let mut name = name.clone();
                        let ptys = jvm_tys(param_tys);
                        let ret = self.physical_call_result(e, jvm_declared_ty(ret_ty));
                        let mut descriptor = method_descriptor(&ptys, ret);
                        let mut ops = vec![recv];
                        ops.extend(args.iter().copied());
                        // The receiver is already of the class the call names; only the arguments
                        // are materialized at the parameter types.
                        let bridge = self
                            .run
                            .protected_member_access_bridges
                            .borrow()
                            .get(&e)
                            .cloned();
                        let ordinary_virtual = access_bridges::select_member_invocation(
                            self.ir,
                            self.run,
                            self.static_owner,
                            e,
                            owner_identity,
                            bridge.is_some(),
                            self.export_private_calls,
                        ) == access_bridges::MemberInvocation::Virtual;
                        let result_narrow = ordinary_virtual
                            .then(|| self.ir.jvm_overridden_call_realizations.get(&e))
                            .flatten()
                            .map(|realization| {
                                let narrow = (realization.descriptor != descriptor
                                    && ret.is_reference())
                                .then(|| crate::jvm::names::instanceof_internal_name(ret));
                                name = realization.physical_name.clone();
                                descriptor = realization.descriptor.clone();
                                narrow
                            })
                            .flatten();
                        let call_parameters = bridge.as_ref().map_or(ptys.as_slice(), |bridge| {
                            bridge.bridge_parameters.as_slice()
                        });
                        let mut physical = vec![bridge.as_ref().map_or_else(
                            || self.value_ty(recv),
                            |bridge| Ty::obj_name(bridge.owner),
                        )];
                        physical.extend(call_parameters.iter().copied());
                        let operand_result = if bridge.is_some() {
                            self.emit_source_access_bridge_operands(&ops, &physical, code)
                        } else {
                            self.emit_source_call_operands(e, 1, &ops, &physical, code)
                        };
                        if let Err(mismatch) = operand_result {
                            self.bail_descriptor_arity(&mismatch, ret, code);
                            return;
                        }
                        let aw: i32 = ptys.iter().map(|t| slot_words(*t) as i32).sum();
                        self.mark_call_start(e, code);
                        let call = access_bridges::SelectedMemberCall {
                            expression: e,
                            owner_identity,
                            owner: &owner,
                            name: &name,
                            descriptor: &descriptor,
                            parameters: &ptys,
                            result: ret,
                            interface_owner: interface,
                            argument_words: aw,
                            protected: bridge.as_ref(),
                            export_private_calls: self.export_private_calls,
                        };
                        // The private member is not a function of this file, so it has no bridge
                        // id here. The declaring class still published `access$<name>(Owner)`.
                        if access_bridges::private_module_callable(self.ir, *module_target) {
                            access_bridges::emit_private_member_extension_call(
                                self.cw, code, &call,
                            );
                            return;
                        }
                        access_bridges::emit_selected_member_call(
                            self.ir,
                            self.run,
                            self.static_owner,
                            self.cw,
                            code,
                            &call,
                        );
                        if let Some(internal) = result_narrow {
                            let class = self.cw.class_ref(&internal);
                            code.checkcast(class);
                        }
                        return;
                    }
                    let owner_identity = *owner;
                    let (owner, mut name, mut descriptor) = (
                        owner_identity.render(),
                        name.clone(),
                        self.physical_call_descriptor(e, descriptor),
                    );
                    let declared_descriptor = descriptor.clone();
                    let protected_bridge = self
                        .run
                        .protected_member_access_bridges
                        .borrow()
                        .get(&e)
                        .cloned();
                    if protected_bridge.is_none() {
                        if let Some(realization) = self.ir.jvm_overridden_call_realizations.get(&e)
                        {
                            name = realization.physical_name.clone();
                            descriptor = realization.descriptor.clone();
                        }
                    }
                    let args = args.clone();
                    if self.emit_primitive_inc_dec_virtual(
                        &owner,
                        &name,
                        &descriptor,
                        recv,
                        &args,
                        code,
                    ) {
                        return;
                    }
                    // A `@JvmStatic` member of an `object`/companion (`Dispatchers.IO`): an ordinary
                    // member call in the language — resolved and lowered with a receiver — that kotlinc
                    // emits as a static taking none. Drop the receiver and `invokestatic`. The receiver is
                    // still EVALUATED when it can have an effect; kotlinc emits the same `…; pop;
                    // invokestatic` and elides a bare singleton/local read entirely.
                    if self.bodies.method_is_static(&owner, &name, &descriptor)
                        && parse_descriptor_params(&descriptor)
                            .is_some_and(|params| params.len() == args.len())
                    {
                        // The receiver is dropped, not skipped: it is still an expression the source
                        // program evaluates. Elide it only when it can run no code, or when it merely
                        // reads a static of the very class this `invokestatic` initializes anyway (the
                        // `Obj.INSTANCE` singleton read) — which is exactly what kotlinc elides.
                        let initializes_owner = matches!(
                            self.ir.expr(recv),
                            IrExpr::ExternalStaticField { owner: field_owner, .. }
                                if field_owner.matches(&owner)
                        );
                        if !crate::ir::expr_runs_no_code(self.ir, recv) && !initializes_owner {
                            self.emit_value(recv, code);
                            code.pop();
                        }
                        let physical_params = parse_descriptor_params(&descriptor)
                            .expect("static method descriptor must be valid");
                        let ret = ty_from_descriptor_ret(&descriptor);
                        if let Err(mismatch) =
                            self.emit_call_descriptor_operands(e, 0, &args, &physical_params, code)
                        {
                            self.bail_descriptor_arity(&mismatch, ret, code);
                            return;
                        }
                        let aw: i32 = physical_params.iter().map(|t| slot_words(*t) as i32).sum();
                        let m = self.cw.methodref(&owner, &name, &descriptor);
                        self.mark_call_start(e, code);
                        code.invokestatic(m, aw, slot_words(ret) as i32);
                        return;
                    }
                    if parse_descriptor_params(&descriptor)
                        .is_some_and(|params| params.len() == args.len() + 1)
                    {
                        let mut physical_args = Vec::with_capacity(args.len() + 1);
                        physical_args.push(recv);
                        physical_args.extend(args.iter().copied());
                        let physical_params = parse_descriptor_params(&descriptor)
                            .expect("static extension descriptor must be valid");
                        let ret = ty_from_descriptor_ret(&descriptor);
                        if let Err(mismatch) = self.emit_call_descriptor_operands(
                            e,
                            1,
                            &physical_args,
                            &physical_params,
                            code,
                        ) {
                            self.bail_descriptor_arity(&mismatch, ret, code);
                            return;
                        }
                        let aw: i32 = physical_params.iter().map(|t| slot_words(*t) as i32).sum();
                        let m = self.cw.methodref(&owner, &name, &descriptor);
                        self.mark_call_start(e, code);
                        code.invokestatic(m, aw, slot_words(ret) as i32);
                        return;
                    }
                    let physical_params = parse_descriptor_params(&descriptor)
                        .expect("virtual call descriptor must be valid");
                    let call_parameters = protected_bridge
                        .as_ref()
                        .map_or(physical_params.as_slice(), |bridge| {
                            bridge.bridge_parameters.as_slice()
                        });
                    crate::trace_compiler!(
                        "emit",
                        "virtual {owner}.{name}{descriptor} receiver={recv} {:?} receiver_ty={:?} args={args:?}",
                        self.ir.expr(recv),
                        self.value_ty(recv),
                    );
                    let ret = ty_from_descriptor_ret(&descriptor);
                    let operand_result = if let Some(bridge) = protected_bridge.as_ref() {
                        let mut operands = Vec::with_capacity(args.len() + 1);
                        operands.push(recv);
                        operands.extend(args.iter().copied());
                        let mut physical = Vec::with_capacity(call_parameters.len() + 1);
                        physical.push(Ty::obj_name(bridge.owner));
                        physical.extend(call_parameters.iter().copied());
                        self.emit_source_access_bridge_operands(&operands, &physical, code)
                    } else {
                        self.emit_descriptor_virtual_operands(
                            e,
                            crate::jvm::ir_emit::call_operands::VirtualCallTarget {
                                owner: &owner,
                                name: &name,
                                descriptor: &descriptor,
                            },
                            recv,
                            &args,
                            call_parameters,
                            code,
                        )
                    };
                    if let Err(mismatch) = operand_result {
                        self.bail_descriptor_arity(&mismatch, ret, code);
                        return;
                    }
                    let aw: i32 = call_parameters.iter().map(|t| slot_words(*t) as i32).sum();
                    if let Some(bridge) = protected_bridge {
                        let mut bridge_params =
                            Vec::with_capacity(bridge.bridge_parameters.len() + 1);
                        bridge_params.push(Ty::obj_name(bridge.owner));
                        bridge_params.extend(bridge.bridge_parameters.iter().copied());
                        let bridge_descriptor = method_descriptor(&bridge_params, ret);
                        let bridge_name = format!("access${name}");
                        let method = self.cw.methodref(
                            &bridge.owner.render(),
                            &bridge_name,
                            &bridge_descriptor,
                        );
                        self.mark_call_start(e, code);
                        code.invokestatic(method, aw + 1, slot_words(ret) as i32);
                    } else if interface {
                        let m = self.cw.interface_methodref(&owner, &name, &descriptor);
                        self.mark_call_start(e, code);
                        code.invokeinterface(m, aw, slot_words(ret) as i32);
                    } else {
                        let m = self.cw.methodref(&owner, &name, &descriptor);
                        self.mark_call_start(e, code);
                        code.invokevirtual(m, aw, slot_words(ret) as i32);
                    }
                    let widened_ret = ty_from_descriptor_ret(&descriptor);
                    let declared_ret = ty_from_descriptor_ret(&declared_descriptor);
                    if declared_ret != widened_ret
                        && declared_ret.is_reference()
                        && !crate::jvm::ir_emit::declaration_types::jvm_is_erased_top(widened_ret)
                    {
                        let internal = crate::jvm::names::instanceof_internal_name(declared_ret);
                        let class = self.cw.class_ref(&internal);
                        code.checkcast(class);
                    }
                }
                Callee::Special {
                    owner,
                    name,
                    descriptor,
                    interface,
                    source_member,
                    source,
                } => {
                    let (owner, name, descriptor, interface) = (
                        owner.render(),
                        name.clone(),
                        self.physical_call_descriptor(e, descriptor),
                        *interface,
                    );
                    let recv = dispatch_receiver.expect("special call needs a receiver");
                    let args = args.clone();
                    let physical_params = parse_descriptor_params(&descriptor)
                        .unwrap_or_else(|| panic!("special call descriptor must be valid: owner={owner} name={name} desc={descriptor:?}"));
                    let ret = ty_from_descriptor_ret(&descriptor);
                    if let Err(mismatch) = self.emit_descriptor_virtual_operands(
                        e,
                        crate::jvm::ir_emit::call_operands::VirtualCallTarget {
                            owner: &owner,
                            name: &name,
                            descriptor: &descriptor,
                        },
                        recv,
                        &args,
                        &physical_params,
                        code,
                    ) {
                        self.bail_descriptor_arity(&mismatch, ret, code);
                        return;
                    }
                    let aw: i32 = physical_params.iter().map(|t| slot_words(*t) as i32).sum();
                    // A diamond `super.f()` to a superinterface DEFAULT method: `invokespecial` on an
                    // `InterfaceMethodref` (JVM allows a direct-superinterface default this way) —
                    // unless `disable` moved that body to the holder, where it is a plain static.
                    if let Some((holder, holder_desc)) = interface
                        .then(|| {
                            self.holder_call(
                                &owner,
                                &descriptor,
                                source.is_some() || source_member.is_some(),
                            )
                        })
                        .flatten()
                    {
                        let m = self.cw.methodref(&holder, &name, &holder_desc);
                        self.mark_call_start(e, code);
                        code.invokestatic(m, aw + 1, slot_words(ret) as i32);
                    } else {
                        let m = if interface {
                            self.cw.interface_methodref(&owner, &name, &descriptor)
                        } else {
                            self.cw.methodref(&owner, &name, &descriptor)
                        };
                        self.mark_call_start(e, code);
                        code.invokespecial(m, aw, slot_words(ret) as i32);
                    }
                }
            },
            IrExpr::TypeOp {
                op,
                arg,
                type_operand,
            } => self.emit_type_operation(e, *op, *arg, *type_operand, code),
            IrExpr::Equality { .. } => self.emit_comparison(e, code),
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => self.emit_binop(e, *op, *lhs, *rhs, code),
            IrExpr::PrimitiveNeg { operand, ty } => {
                self.emit_value(*operand, code);
                match ir_ty_to_jvm(ty) {
                    Ty::Long => code.lneg(),
                    Ty::Float => code.fneg(),
                    Ty::Double => code.dneg(),
                    _ => code.ineg(),
                }
            }
            IrExpr::StringConcat(parts) => self.emit_string_concat(parts, code),
            IrExpr::EnumEntry { classifier, name } => {
                let fq_name = classifier.render();
                let desc = format!("L{fq_name};");
                let f = self.cw.fieldref(&fq_name, name, &desc);
                code.getstatic(f, 1);
            }
            IrExpr::StaticInstance { owner, ty, field } => {
                let owner_fq = self.ir.classes[*owner as usize].fq_name();
                let ty_fq = self.ir.classes[*ty as usize].fq_name();
                let f = self.cw.fieldref(&owner_fq, field, &format!("L{ty_fq};"));
                code.getstatic(f, 1);
            }
            IrExpr::SingletonValue { classifier } => {
                let Some(published) = singleton_instance_load::published_singleton(
                    self.ir,
                    *classifier,
                    self.bodies.singleton_storage(*classifier),
                ) else {
                    *self.run.emit_error.borrow_mut() = Some(format!(
                        "missing JVM storage for singleton {}",
                        classifier.render()
                    ));
                    return;
                };
                let (owner, field) = singleton_instance_load::loaded_instance(
                    self.self_companion,
                    *classifier,
                    published,
                );
                let owner = owner.render();
                let ty = classifier.render();
                let f = self.cw.fieldref(&owner, &field, &format!("L{ty};"));
                code.getstatic(f, 1);
            }
            IrExpr::ExternalStaticInstance { owner, ty, field } => {
                let (owner, field) = singleton_instance_load::loaded_instance(
                    self.self_companion,
                    *ty,
                    singleton_instance_load::PublishedSingleton {
                        owner: *owner,
                        field: field.clone(),
                    },
                );
                let owner = crate::jvm::names::classfile_internal_name_of(owner);
                let ty = crate::jvm::names::classfile_internal_name_of(*ty);
                let f = self.cw.fieldref(owner, &field, &format!("L{ty};"));
                code.getstatic(f, 1);
            }
            IrExpr::ExternalStaticField {
                owner,
                name,
                descriptor,
            } => {
                let owner = owner.render();
                let f = self.cw.fieldref(&owner, name, descriptor);
                let words = if descriptor == "J" || descriptor == "D" {
                    2
                } else {
                    1
                };
                code.getstatic(f, words);
            }
            IrExpr::EnumValues { classifier } => {
                let fq = classifier.render();
                let m = self.cw.methodref(&fq, "values", &format!("()[L{fq};"));
                code.invokestatic(m, 0, 1);
            }
            IrExpr::EnumEntries { classifier } => {
                let fq = classifier.render();
                let m = self
                    .cw
                    .methodref(&fq, "getEntries", "()Lkotlin/enums/EnumEntries;");
                code.invokestatic(m, 0, 1);
            }
            IrExpr::ReifiedClassMarker {
                name,
                erased,
                kclass,
            } => {
                // kotlinc's reified placeholder: `reifiedOperationMarker(4, "T")` then the erased
                // class constant — a splicer patches the pair with the call-site class.
                code.push_int(4, self.cw);
                code.push_string(name, self.cw);
                let m = self.cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "reifiedOperationMarker",
                    "(ILjava/lang/String;)V",
                );
                code.invokestatic(m, 2, 0);
                code.ldc_class(&erased.render(), self.cw);
                if *kclass {
                    let reflection = self.cw.methodref(
                        "kotlin/jvm/internal/Reflection",
                        "getOrCreateKotlinClass",
                        "(Ljava/lang/Class;)Lkotlin/reflect/KClass;",
                    );
                    code.invokestatic(reflection, 1, 1);
                }
            }
            IrExpr::ReifiedTypeOp {
                cast,
                negated,
                arg,
                name,
                erased,
            } => {
                self.emit_value(*arg, code);
                // kotlinc's reified is/as placeholder: marker(3) + instanceof, marker(1) + checkcast.
                code.push_int(if *cast { 1 } else { 3 }, self.cw);
                code.push_string(name, self.cw);
                let m = self.cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "reifiedOperationMarker",
                    "(ILjava/lang/String;)V",
                );
                code.invokestatic(m, 2, 0);
                let ci = self.cw.class_ref(&erased.render());
                if *cast {
                    code.checkcast(ci);
                } else {
                    code.instance_of(ci);
                    if *negated {
                        self.emit_negated_instance_result(code);
                    }
                }
            }
            IrExpr::EnumValueOf {
                classifier,
                arg,
                declaration,
            } => {
                let fq = classifier.render();
                if *declaration == crate::ir::EnumValueOfDeclaration::StandardLibraryTopLevel {
                    self.mark_inline_call_site_line(e, code);
                }
                self.emit_value(*arg, code);
                let m = self
                    .cw
                    .methodref(&fq, "valueOf", &format!("(Ljava/lang/String;)L{fq};"));
                // Both declarations reach the same `E.valueOf`, and they take opposite line
                // rules: the classifier's own MEMBER is an ordinary dispatch, so the call's line
                // returns at the invoke; the standard library's top-level `enumValueOf<E>` is
                // INLINE, so what follows is its expansion and kotlinc marks the call site
                // instead. That is why the selected declaration is recorded rather than inferred.
                match declaration {
                    crate::ir::EnumValueOfDeclaration::Member => self.mark_dispatch_line(e, code),
                    crate::ir::EnumValueOfDeclaration::StandardLibraryTopLevel => {}
                }
                code.invokestatic(m, 1, 1);
            }
            IrExpr::When { branches } => {
                if !self.emit_short_circuit_value(e, code) {
                    self.emit_when(e, branches, false, code);
                }
            }
            IrExpr::Block { stmts, value } => self.emit_value_block(e, stmts, *value, code),
            IrExpr::Lambda {
                impl_fn,
                arity,
                captures,
                sam,
                ..
            } => {
                // This lambda becomes a real closure. Record the implementation so the dead-lambda
                // pass keeps it; separately record only the indy realization for target-version checks.
                self.run.used_lambdas.borrow_mut().insert(*impl_fn);
                let function_adapter = sam.as_ref().is_some_and(|target| target.function_adapter);
                let mut lambda_mode = self.lambda_modes.for_lambda(sam.as_ref(), *arity);
                // This implementation executes a runtime reified operation. The common IR records
                // that semantic requirement; the JVM realizes it as a class independently of the
                // file's ordinary lambda strategy.
                if self
                    .ir
                    .runtime_reified_lambda_implementations
                    .contains(impl_fn)
                    || self.ir.inline_anonymous_lambdas.contains(impl_fn)
                {
                    lambda_mode = LambdaMode::Class;
                }
                // A bounded fun-interface slot cannot be bootstrapped when the lambda parameter
                // erased to `Object`. Decide that here, before access flags are read, so the
                // implementation is package-visible to the class that calls it.
                if sam.as_ref().is_some_and(|target| {
                    lambda_class::bounded_erasure_needs_class(
                        self.ir,
                        *impl_fn,
                        target,
                        captures.len(),
                    )
                }) {
                    lambda_mode = LambdaMode::Class;
                }
                if lambda_mode == LambdaMode::Indy {
                    if self.ir.jvm_unrealized_lambda_classes.contains(impl_fn) {
                        // The class is discarded; a placeholder keeps its frames computable.
                        self.run
                            .set_emit_error(lambda_class::unrealized(self.ir, *impl_fn));
                        return code.aconst_null();
                    }
                    self.run.used_indy_lambdas.borrow_mut().insert(*impl_fn);
                }
                let f = &self.ir.functions[*impl_fn as usize];
                let impl_name = f.name.clone();
                let impl_params = jvm_function_params(self.ir, *impl_fn);
                let impl_ret = jvm_declared_ty(&f.ret);
                // The impl method's parameters are the captured variables (bound at the call site)
                // followed by the lambda's own parameters. Only the latter form the SAM/instantiated
                // method types; the captures parameterize the `invokedynamic` itself.
                // The IR carries the exact capture list. Do not reconstruct this boundary from the
                // source arity: a suspend implementation has an additional physical Continuation
                // parameter, which belongs to the SAM side of the boundary rather than the captures.
                let n_cap = captures.len();
                if impl_params.len() < n_cap {
                    self.run.set_emit_error(
                        "lambda implementation has fewer parameters than captured values"
                            .to_string(),
                    );
                    return;
                }
                let (cap_tys, lam_tys) = impl_params.split_at(n_cap);
                let impl_desc = method_descriptor(&impl_params, impl_ret);
                // For a Kotlin lambda the target is `FunctionN.invoke` (samMethodType erased to
                // `(Object,…)Object`, instantiatedMethodType the boxed actuals); for a user SAM
                // conversion the target is the interface's single method, whose descriptor is the
                // lambda's concrete signature (no erasure/boxing).
                let (iface, sam_method, sam_desc, inst_desc) = match sam {
                    Some(target) => {
                        let descriptors = match lambda_class::sam_bootstrap_descriptors(
                            self.ir,
                            *impl_fn,
                            target,
                            captures.len(),
                        ) {
                            Ok(descriptors) => descriptors,
                            Err(error) => {
                                self.run.set_emit_error(error.to_string());
                                return;
                            }
                        };
                        crate::trace_compiler!(
                            "emit",
                            "selected SAM impl={impl_name} own={lam_tys:?} target={}.{}{} instantiated={}",
                            target.classifier,
                            target.method,
                            descriptors.erased_method,
                            descriptors.instantiated_method,
                        );
                        (
                            target.classifier.render(),
                            target.method.clone(),
                            descriptors.erased_method,
                            descriptors.instantiated_method,
                        )
                    }
                    None => {
                        let iface = jvm_function_interface(*arity);
                        let inst_params: Vec<String> =
                            lam_tys.iter().map(|t| boxed_descriptor(*t)).collect();
                        let inst_desc =
                            format!("({}){}", inst_params.concat(), boxed_descriptor(impl_ret));
                        (
                            iface,
                            "invoke".to_string(),
                            jvm_function_invoke_descriptor(*arity),
                            inst_desc,
                        )
                    }
                };
                crate::trace_compiler!(
                    "emit",
                    "lambda indy impl={impl_name}{impl_desc} captures={cap_tys:?} own={lam_tys:?} target={iface}.{sam_method}{sam_desc} instantiated={inst_desc}"
                );
                // The impl method lives on whichever class owns it (a class-member lambda's impl is a
                // method of the enclosing class, so it can access that class's privates); top-level
                // lambdas keep theirs on the file facade.
                let impl_class = self.ir.classes.iter().find(|c| c.methods.contains(impl_fn));
                let impl_owner_is_interface = impl_class.is_some_and(|c| c.is_interface);
                let impl_owner = impl_class
                    .map(|c| c.fq_name())
                    .unwrap_or_else(|| self.facade.clone());
                if lambda_mode == LambdaMode::Class {
                    let (internal, identity) = lambda_class_names::class_name(
                        self.ir,
                        *impl_fn,
                        &impl_name,
                        &impl_owner,
                        &self.facade,
                        self.lambda_modes,
                    );
                    self.run.lambda_classes.borrow_mut().push(LambdaClassPlan {
                        internal: internal.clone(),
                        iface: iface.clone(),
                        sam_method: sam_method.clone(),
                        sam_desc: sam_desc.clone(),
                        impl_owner: impl_owner.clone(),
                        impl_name: impl_name.clone(),
                        impl_desc: impl_desc.clone(),
                        captures: cap_tys.to_vec(),
                        arity: *arity as u32,
                        kotlin_function: sam.is_none(),
                        function_adapter,
                        identity,
                        owner_is_interface: impl_owner_is_interface,
                    });
                    let nullable = sam.as_ref().is_some_and(|target| target.nullable);
                    if nullable {
                        self.emit_capturing_lambda_class(
                            code, &internal, captures, cap_tys, nullable,
                        );
                    } else if captures.is_empty() {
                        // Nothing captured, so every evaluation yields the same instance — kotlinc
                        // holds it in a static and the call site just reads it.
                        let field =
                            self.cw
                                .fieldref(&internal, "INSTANCE", &format!("L{internal};"));
                        code.getstatic(field, 1);
                        let target = self.cw.class_ref(&iface);
                        code.checkcast(target);
                    } else {
                        self.emit_capturing_lambda_class(
                            code, &internal, captures, cap_tys, nullable,
                        );
                    }
                    return;
                }
                // kotlinc interns the BOOTSTRAP ARGUMENTS first (erased SAM MethodType, the impl
                // MethodHandle with its `$lambda$N` refs, the instantiated MethodType), and the
                // LambdaMetafactory handle only after them — pool order follows that visit.
                let sam_mt = self.cw.method_type(&sam_desc);
                let impl_ref = if impl_owner_is_interface {
                    self.cw
                        .interface_methodref(&impl_owner, &impl_name, &impl_desc)
                } else {
                    self.cw.methodref(&impl_owner, &impl_name, &impl_desc)
                };
                let impl_mh = self.cw.method_handle_ref(6, impl_ref);
                let inst_mt = self.cw.method_type(&inst_desc);
                let meta = self.cw.method_handle_static(
                    "java/lang/invoke/LambdaMetafactory",
                    "metafactory",
                    LMF_METAFACTORY_DESC,
                );
                let bsm = self.cw.add_bootstrap(meta, vec![sam_mt, impl_mh, inst_mt]);
                // The `invokedynamic` takes the captured values and yields the interface instance.
                let cap_descs: String = cap_tys.iter().map(|t| type_descriptor(*t)).collect();
                let indy =
                    self.cw
                        .invoke_dynamic(bsm, &sam_method, &format!("({cap_descs})L{iface};"));
                let cap_words: i32 = cap_tys.iter().map(|t| slot_words(*t) as i32).sum();
                self.emit_indy_lambda(
                    code,
                    indy,
                    cap_words,
                    captures,
                    sam.as_ref().is_some_and(|target| target.nullable),
                );
            }
            IrExpr::UnitInstance => {
                let f = self.cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
                code.getstatic(f, 1);
            }
            IrExpr::CurrentContinuation if self.emit_fake_continuation(e, code) => {}
            IrExpr::CurrentContinuation => {
                // The CPS pass rewrites this to a `GetValue` of the continuation slot for every
                // function whose machine it owns. It leaves the node in place for a function whose
                // machine EMISSION owns — a suspension inside a body this emitter splices — because
                // which slot holds the continuation is then an emission decision: the `$completion`
                // parameter while discovering the frame, the machine's own local while building it.
                let Some(slot) = self.continuation_slot else {
                    unreachable!("CurrentContinuation outside a suspend function reaches emit")
                };
                code.aload(slot);
            }
            IrExpr::NotNullAssert { operand, message } => {
                self.emit_value(*operand, code);
                // Flow typing can prove a stable nullable scalar non-null before a later, redundant
                // `!!`. Its checked FIR assertion remains visible, but the selected smart-cast
                // conversion means the operand is already a physical JVM scalar by this point. A
                // scalar cannot be null, and passing its stack word to Intrinsics.checkNotNull(Object)
                // is invalid bytecode. The ordinary nullable-scalar shape is different: the assert
                // sees the boxed wrapper and an enclosing coercion unboxes only after this check.
                if self.value_ty(*operand).is_jvm_scalar() {
                    return;
                }
                code.dup();
                // A platform value narrowed to a declared non-null type names the checked expression
                // in its failure (`getenv(...) must not be null`); `x!!` has no such name and uses the
                // one-argument form. Both consume the duplicate and leave the value in place.
                let m = match message {
                    Some(message) => {
                        code.push_string(message, self.cw);
                        self.cw.methodref(
                            "kotlin/jvm/internal/Intrinsics",
                            "checkNotNullExpressionValue",
                            "(Ljava/lang/Object;Ljava/lang/String;)V",
                        )
                    }
                    None => self.cw.methodref(
                        "kotlin/jvm/internal/Intrinsics",
                        "checkNotNull",
                        "(Ljava/lang/Object;)V",
                    ),
                };
                code.invokestatic(m, if message.is_some() { 2 } else { 1 }, 0);
            }
            IrExpr::LateinitCheck { operand, name } => {
                // A `lateinit var` local read: throw `UninitializedPropertyAccessException` while the slot
                // is still null. Same guard as the member-field lateinit read (`dup; ifnonnull L; ldc
                // name; invokestatic throwUninitializedPropertyAccessException; L:`).
                self.emit_value(*operand, code);
                code.dup();
                let lbl = code.new_label();
                code.ifnonnull(lbl);
                code.push_string(name, self.cw);
                let m = self.cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "throwUninitializedPropertyAccessException",
                    "(Ljava/lang/String;)V",
                );
                code.invokestatic(m, 1, 0);
                // `value_ty` already yields the JVM type of the operand (a reference here); the surviving
                // (non-null) value is on the stack at the branch target.
                self.bind(lbl, code);
            }
            IrExpr::Throw { operand } => {
                self.emit_value(*operand, code);
                code.athrow();
            }
            // `return v` in value position (`x ?: return v`): emit the return; control transfers away, so
            // (like `throw`) nothing is left for the surrounding merge.
            IrExpr::Return(value) => self.emit_return_node(e, *value, code),
            IrExpr::Vararg {
                array_type,
                elements,
                spreads,
            } => vararg::emit(self, array_type, elements, spreads, code),
            IrExpr::NewArray { array_type, size } => {
                let et = array_jvm_element(array_type);
                self.emit_value(*size, code);
                if et.is_jvm_scalar() {
                    code.newarray(prim_newarray_atype(et));
                } else {
                    // Peel a nullable element's `?`: `Array<Int?>` = `Integer[]`, so the `anewarray` class
                    // is `java/lang/Integer` (the `?` only tells `Array.get`/`.set` to keep it boxed).
                    let ci = self
                        .cw
                        .class_ref(&crate::jvm::names::anewarray_element_class(et));
                    code.anewarray(ci);
                }
            }
            IrExpr::Try {
                body,
                catches,
                finally,
                result,
            } => {
                let catches = catches.clone();
                let parts = try_emission::TryParts {
                    body: *body,
                    catches: &catches,
                    finally: *finally,
                    result: *result,
                };
                self.emit_try(e, parts, false, code);
            }
            IrExpr::RefNew { elem, init } => self.emit_shared_cell(*elem, *init, code),
            IrExpr::RefGet { holder, elem } => {
                self.emit_value(*holder, code);
                let (cls, fdesc) = ref_class(elem);
                let f = self.cw.fieldref(cls, "element", fdesc);
                let ejvm = ir_ty_to_jvm(elem);
                code.getfield(f, slot_words(ejvm) as i32);
                // An `ObjectRef.element` is typed `Object`; narrow to the boxed value's reference type.
                if ejvm.is_reference()
                    && crate::jvm::names::instanceof_internal_name(ejvm) != "java/lang/Object"
                {
                    let cc = self
                        .cw
                        .class_ref(&crate::jvm::names::instanceof_internal_name(ejvm));
                    code.checkcast(cc);
                }
            }
            IrExpr::RefSet {
                holder,
                elem,
                value,
            } => {
                // A value that cannot carry the operand stack (`msg = try {…} finally {}`) can't
                // run with the holder on it. Spill it first (as `RefNew` does).
                if self.spills_operand_prefix(*value) {
                    let temps = self.spill_to_temps(&[*value], code);
                    self.emit_value(*holder, code);
                    for &(slot, t, _) in &temps {
                        load(t, slot, code);
                    }
                    self.release_operand_spills(&temps);
                } else {
                    self.emit_value(*holder, code);
                    self.emit_value(*value, code);
                }
                let (cls, fdesc) = ref_class(elem);
                let f = self.cw.fieldref(cls, "element", fdesc);
                code.putfield(f, slot_words(ir_ty_to_jvm(elem)) as i32);
            }
            IrExpr::InvokeFunction {
                func,
                args,
                params,
                ret,
            } => {
                self.emit_function_invocation(e, *func, args, params, code);
                // A transformed suspension materializes its declared result when the point
                // closes. Narrowing here would checkcast the erased `Object` first.
                if self.transformed_result(e).is_none() && !self.erased_invocations.remove(&e) {
                    self.narrow_invocation_result(*ret, code);
                }
            }
            _ => {}
        }
    }
}
