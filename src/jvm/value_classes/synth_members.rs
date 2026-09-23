//! The members a `@JvmInline value class` gets that its source never wrote.
//!
//! `unbox-impl`, `box-impl`, `constructor-impl`, `equals-impl0` and the structural
//! `equals`/`hashCode`/`toString` are JVM realization, not Kotlin declarations, so they are
//! synthesized here rather than in common lowering. The plain single-field class — the field, the
//! `<init>` and the underlying getter — is already in the IR by the time this runs.

use super::*;

/// Synthesize a value class's unboxed-support members directly in the IR (a JVM concern, so it lives in
/// this pass, not common lowering): `unbox-impl`/`box-impl`/`constructor-impl`/`equals-impl0` plus structural
/// `equals`/`hashCode`/`toString` (skipped where the user defined one). The plain single-field class
/// (field, `<init>`, getter) is already present in common IR.
pub(super) fn synth_value_members(
    ir: &mut IrFile,
    class_id: u32,
    under: &Under,
    has_init: bool,
    constructor_default: Option<ExprId>,
    synthesized_instance_entries: &mut HashSet<u32>,
) -> bool {
    let internal = ir.classes[class_id as usize].fq_name();
    let fname = ir.classes[class_id as usize].fields[0].name.clone();
    let internal_name = type_name(&internal);
    let u_ir = under.get(&internal_name).copied().unwrap_or(Ty::Error);
    // The FULLY-ERASED underlying: a NESTED value class erases through its chain to the first type that
    // stops unboxing — `NZ2(NZ1)` where `NZ1(Z?)` erases to a BOXED `Z` (`LZ;`), not `LNZ1;`. The static
    // `-impl` members take this erased type (matching kotlinc), so their hardcoded delegation descriptors
    // must use it too, or the operand-stack type won't match the actual method signature (a VerifyError).
    let eu = erase(&u_ir, under);
    // The underlying JVM descriptor (`Ljava/lang/String;`, `I`, `LZ;`, …) — the argument type of the
    // static `-impl` members, which the instance methods delegate to (matching kotlinc's value-class shape).
    let udesc = type_descriptor(ir_ty_to_jvm(&eu));
    let x_ir = Ty::obj(&internal);
    let bool_ir = Ty::Boolean;
    let int_ir = Ty::Int;
    let str_ir = Ty::String;
    let any_ir = Ty::obj("kotlin/Any");

    // A value class's declared accessors are static implementations over its carrier. The source IR
    // initially represents a computed accessor like every ordinary member (implicit receiver in slot
    // zero); replacing that receiver with an explicit carrier parameter preserves every body value id
    // while giving emission and metadata the ABI kotlinc exposes.
    let computed_accessors = ir.classes[class_id as usize]
        .properties
        .iter()
        .enumerate()
        .filter(|(_, property)| property.backing_field.is_none())
        .map(|(index, property)| {
            (
                index,
                property.getter,
                property.setter,
                property.name.clone(),
                property.is_open,
            )
        })
        .collect::<Vec<_>>();
    for (property_index, getter, setter, property_name, overrides_supertype) in computed_accessors {
        if let Some(getter) = getter {
            let source_name = property_getter_name(&property_name);
            let jvm_name = format!("{}-impl", property_getter_name(&property_name));
            let ret = {
                let function = &mut ir.functions[getter as usize];
                function.name.clone_from(&jvm_name);
                function.params.insert(0, u_ir);
                function.is_static = true;
                function.ret
            };
            crate::jvm::method_parameters::prepend_value_class_receiver(ir, getter, "arg0");
            ir.classes[class_id as usize].properties[property_index].getter_jvm_name =
                Some(jvm_name.clone());
            // The static `getX-impl(U)` is how an unboxed value calls its accessor. If the property
            // overrides a supertype declaration, the BOX must additionally implement the ordinary
            // virtual `getX()` entry point. Delegate from that instance method to the same implementation;
            // bridge derivation has already established the override, so no hierarchy lookup occurs here.
            if overrides_supertype {
                let this = ir.add_expr(IrExpr::GetValue(0));
                let carrier = ir.add_expr(IrExpr::GetField {
                    receiver: this,
                    class: class_id,
                    index: 0,
                });
                let call = ir.add_expr(IrExpr::Call {
                    callee: Callee::Static {
                        owner: internal_name,
                        name: jvm_name,
                        descriptor: format!("({}){}", desc(&u_ir), desc(&ret)),
                        inline: crate::libraries::InlineKind::None,
                    },
                    dispatch_receiver: None,
                    args: vec![carrier],
                });
                let returned = ir.add_expr(IrExpr::Return(Some(call)));
                let body = ir.add_expr(IrExpr::Block {
                    stmts: vec![returned],
                    value: None,
                });
                let fid = ir.add_fun(crate::ir::IrFunction {
                    name: source_name,
                    params: vec![],
                    ret,
                    body: Some(body),
                    is_static: false,
                    dispatch_receiver: Some(internal_name),
                    param_checks: vec![],
                });
                ir.classes[class_id as usize].methods.push(fid);
                synthesized_instance_entries.insert(fid);
                ir.open_methods.insert(fid);
            }
        }
        if let Some(setter) = setter {
            let jvm_name = format!(
                "{}-impl",
                crate::names::property_setter_name(&property_name)
            );
            {
                let function = &mut ir.functions[setter as usize];
                function.name.clone_from(&jvm_name);
                function.params.insert(0, u_ir);
                function.is_static = true;
            }
            crate::jvm::method_parameters::prepend_value_class_receiver(ir, setter, "arg0");
            ir.classes[class_id as usize].properties[property_index].setter_jvm_name =
                Some(jvm_name);
        }
    }

    // User-written Any overrides become the static `-impl` body. Its former receiver slot 0 is exactly
    // the first static parameter slot, so the body itself needs no slot rewrite; value-class property
    // reads inside it are lowered to the carrier later in this pass. The ordinary instance override is
    // synthesized below as the ABI delegator back to this implementation.
    let mut custom_equals = false;
    let mut custom_hash_code = false;
    let mut custom_to_string = false;
    let mut custom_carrier_functions = Vec::new();
    for &fid in &ir.classes[class_id as usize].methods.clone() {
        let Some(function) = ir.functions.get_mut(fid as usize) else {
            continue;
        };
        let custom_impl = match (function.name.as_str(), function.params.as_slice()) {
            ("equals", [_]) => {
                custom_equals = true;
                Some("equals-impl")
            }
            ("hashCode", []) => {
                custom_hash_code = true;
                Some("hashCode-impl")
            }
            ("toString", []) => {
                custom_to_string = true;
                Some("toString-impl")
            }
            _ => None,
        };
        if let Some(name) = custom_impl {
            function.name = name.to_string();
            function.params.insert(0, u_ir);
            function.is_static = true;
            custom_carrier_functions.push(fid);
        }
    }
    for function in custom_carrier_functions {
        crate::jvm::method_parameters::prepend_value_class_receiver(ir, function, "arg0");
    }

    let add_static = |ir: &mut IrFile, name: &str, params: Vec<Ty>, ret: Ty, body: ExprId| -> u32 {
        let fid = ir.add_fun(crate::ir::IrFunction {
            name: name.to_string(),
            params,
            ret,
            body: Some(body),
            is_static: true,
            dispatch_receiver: Some(internal_name),
            param_checks: Vec::new(),
        });
        ir.classes[class_id as usize].methods.push(fid);
        fid
    };
    let add_inst =
        |ir: &mut IrFile, name: &str, params: Vec<Ty>, ret: Ty, body: ExprId| -> Option<u32> {
            // Don't synthesize over a user-defined member of the same name.
            let exists = ir.classes[class_id as usize]
                .methods
                .iter()
                .any(|&m| ir.functions.get(m as usize).is_some_and(|f| f.name == name));
            if exists {
                return None;
            }
            let fid = ir.add_fun(crate::ir::IrFunction {
                name: name.to_string(),
                params,
                ret,
                body: Some(body),
                is_static: false,
                dispatch_receiver: Some(internal_name),
                param_checks: Vec::new(),
            });
            ir.classes[class_id as usize].methods.push(fid);
            Some(fid)
        };
    let this_field = |ir: &mut IrFile| {
        let recv = ir.add_expr(IrExpr::GetValue(0));
        ir.add_expr(IrExpr::GetField {
            receiver: recv,
            class: class_id,
            index: 0,
        })
    };
    let str_const = |ir: &mut IrFile, s: String| {
        ir.add_expr(IrExpr::Const(crate::ir::IrConst::String(
            crate::kt_string::KtString::from(s),
        )))
    };
    let ret_block = |ir: &mut IrFile, v: ExprId| {
        let r = ir.add_expr(IrExpr::Return(Some(v)));
        ir.add_expr(IrExpr::Block {
            stmts: vec![r],
            value: None,
        })
    };

    // unbox-impl(): U — kotlinc marks it ACC_SYNTHETIC (a compiler-manufactured box adapter).
    {
        let g = this_field(ir);
        let body = ret_block(ir, g);
        if let Some(fid) = add_inst(ir, "unbox-impl", vec![], u_ir, body) {
            ir.synthetic_methods.insert(fid);
            ir.jvm_value_class_representation_order.insert(fid, 2);
        }
    }
    // box-impl(U): X  — `new X(u)`. Also ACC_SYNTHETIC.
    {
        let arg = ir.add_expr(IrExpr::GetValue(0));
        let box_internal = ir.classes[class_id as usize].fq_name_id();
        let new = ir.add_expr(IrExpr::New {
            internal: box_internal,
            args: vec![arg],
            ctor_params: Some(vec![u_ir]),
            ctor_desc: None,
            external_target: None,
            defaults: Box::new([]),
            default_prefix_count: 0,
        });
        let body = ret_block(ir, new);
        let fid = add_static(ir, "box-impl", vec![u_ir], x_ir, body);
        ir.synthetic_methods.insert(fid);
        ir.jvm_value_class_representation_order.insert(fid, 1);
    }
    // constructor-impl(U): U  — runs the `init { … }` block (side effects/validation), then returns the
    // arg. The init runs HERE, not in `box-impl`/`<init>`: `box-impl` only wraps an already-built value, so
    // it must NOT re-run the init. MOVE `init_body` out of the class (clearing it, so `<init>` keeps only
    // the field assignment) and inline it: common lowering built it in an INSTANCE frame (`this`@0, ctor param
    // @1), so a sole-field read `this.<field>` is the param — rewrite it to the param, then shift every
    // value slot down by one. The resulting body still runs over the UNBOXED param (slot 0), so step-4
    // rewrites its nested value-class accesses (see the `constructor-impl` entry added to `s4_bodies`).
    {
        let mut stmts = Vec::new();
        if has_init {
            if let Some(init_root) = ir.classes[class_id as usize].init_body {
                let mut reach = HashSet::new();
                collect_reachable(&ir.exprs, init_root, &mut reach);
                let class_fq = ir.classes[class_id as usize].fq_name;
                for id in reach {
                    // The sole field read — as an indexed field read, or as the property it is.
                    let sole_field_read = match &ir.exprs[id as usize] {
                        IrExpr::GetField { class, .. } => *class == class_id,
                        IrExpr::PropertyRead { owner, .. } => *owner == class_fq,
                        _ => false,
                    };
                    if sole_field_read {
                        ir.exprs[id as usize] = IrExpr::GetValue(1); // sole field == the ctor param (slot 1)
                    }
                }
                shift_slots(ir, init_root); // slot 1 (param) → 0; no `this` use remains
                if let IrExpr::Block { stmts: bs, value } = &ir.exprs[init_root as usize] {
                    stmts.extend(bs.iter().copied());
                    if let Some(v) = value {
                        stmts.push(*v);
                    }
                } else {
                    stmts.push(init_root);
                }
                ir.classes[class_id as usize].init_body = None;
            }
        }
        let arg = ir.add_expr(IrExpr::GetValue(0));
        stmts.push(ir.add_expr(IrExpr::Return(Some(arg))));
        let body = ir.add_expr(IrExpr::Block { stmts, value: None });
        let cfid = add_static(ir, "constructor-impl", vec![u_ir], u_ir, body);
        ir.jvm_value_class_representation_order.insert(cfid, 0);
        crate::jvm::method_parameters::record_function(ir, cfid, &[&fname], &[]);
        // Unlike a source value-class member converted to `member-impl`, this generated function's
        // carrier is its declared constructor parameter, not a former dispatch receiver. Keeping an
        // owner marker here would make the default-stub emitter exclude the parameter from Kotlin's
        // mask ordinals and silently skip its checked default.
        ir.functions[cfid as usize].dispatch_receiver = None;
        ir.open_methods.insert(cfid); // kotlinc emits `constructor-impl` `public static` (non-final)
                                      // A default on the single underlying property (`ItemId(val value: String = …)`) → register it as
                                      // `constructor-impl`'s param default so the backend emits `constructor-impl$default(U, int, marker)`
                                      // (kotlinc's synthetic). The generic constructor default was reframed to this static layout above.
        if let Some(def) = constructor_default {
            ir.fn_params.insert(
                cfid,
                crate::ir::FnParamInfo::defaults(vec![fname.clone()], vec![Some(def)]),
            );
        }
    }
    // hashCode/equals/toString operate on the value class's IMMEDIATE erased underlying, NOT the final
    // primitive of a nested chain: `ZN(val z: Z1?)` erases to a BOXED `Z1` (`LZ1;`), so it hashes/compares
    // as a reference (`Objects.hashCode`/`areEqual` → `Z1`'s own members), not as the final `Int`.
    let is_ref_under = is_ref(&eu);
    // Keep the exact terminal semantic type through synthesis. A nullable primitive underlying is
    // represented as a reference and therefore takes the null-safe reference branch below; no
    // spelling conversion is needed to distinguish it from the primitive carrier.
    let terminal_underlying = eu.non_null();
    // equals-impl0(U, U): Boolean
    {
        let a = ir.add_expr(IrExpr::GetValue(0));
        let b = ir.add_expr(IrExpr::GetValue(1));
        let cmp = if custom_equals {
            let boxed = ir.add_expr(IrExpr::Call {
                callee: Callee::Static {
                    owner: internal_name,
                    name: "box-impl".to_string(),
                    descriptor: format!("({udesc})L{internal};"),
                    inline: InlineKind::None,
                },
                dispatch_receiver: None,
                args: vec![b],
            });
            ir.add_expr(IrExpr::Call {
                callee: Callee::Static {
                    owner: internal_name,
                    name: "equals-impl".to_string(),
                    descriptor: format!("({udesc}Ljava/lang/Object;)Z"),
                    inline: InlineKind::None,
                },
                dispatch_receiver: None,
                args: vec![a, boxed],
            })
        } else {
            vc_underlying_eq(ir, a, b, is_ref_under, terminal_underlying)
        };
        let body = ret_block(ir, cmp);
        let function = add_static(ir, "equals-impl0", vec![u_ir, u_ir], bool_ir, body);
        crate::jvm::method_parameters::record_function(ir, function, &["p1", "p2"], &[]);
        ir.jvm_value_class_representation_order.insert(function, 3);
        ir.jvm_nullability_unannotated_methods.insert(function);
    }
    // kotlinc emits the logic in a static `<name>-impl(U)` operating on the unboxed value, and the
    // instance method delegates to it (`toString()` → `toString-impl(this.field)`). The instance methods
    // and the `-impl` statics are all `open` (non-`final`).
    // toString-impl(U v): "X(field=" + v + ")" ; toString(): return toString-impl(this.field)
    {
        let simple = internal
            .rsplit('/')
            .next()
            .unwrap_or(&internal)
            .replace('$', ".");
        if !custom_to_string {
            let v = ir.add_expr(IrExpr::GetValue(0));
            // ONE `StringConcat` (not nested `+`): kotlinc builds a single `StringBuilder` and appends the
            // 1-char closing paren via `append(C)` — a nested concat would emit a second builder.
            let prefix = str_const(ir, format!("{simple}({fname}="));
            let close = str_const(ir, ")".to_string());
            let acc = ir.add_expr(IrExpr::StringConcat(vec![prefix, v, close]));
            let sbody = ret_block(ir, acc);
            let impl_fid = add_static(ir, "toString-impl", vec![u_ir], str_ir, sbody);
            crate::jvm::method_parameters::record_function(ir, impl_fid, &["arg0"], &[0]);
            ir.open_methods.insert(impl_fid);
            ir.jvm_nullability_unannotated_methods.insert(impl_fid);
        }
        let fv = this_field(ir);
        let call = ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: internal_name,
                name: "toString-impl".to_string(),
                descriptor: format!("({udesc})Ljava/lang/String;"),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![fv],
        });
        let ibody = ret_block(ir, call);
        if let Some(fid) = add_inst(ir, "toString", vec![], str_ir, ibody) {
            ir.open_methods.insert(fid);
            if !custom_to_string {
                ir.jvm_nullability_unannotated_methods.insert(fid);
            }
        }
    }
    // hashCode-impl(U v): v.hashCode() ; hashCode(): return hashCode-impl(this.field)
    {
        if !custom_hash_code {
            let v = ir.add_expr(IrExpr::GetValue(0));
            // A NON-NULL reference underlying hashes through its OWN `hashCode()` (kotlinc's shape,
            // `String.hashCode()`), not the null-safe `Objects.hashCode` — that is only for a nullable (or
            // boxed-primitive) underlying, which can actually be null.
            // Only a real non-null reference CLASS underlying: an ARRAY has no such class (`kotlin/IntArray`
            // is not a JVM type — a virtual call on it is a `NoClassDefFoundError`), and a nullable or
            // boxed-primitive underlying must keep the null-safe `Objects.hashCode`.
            let nonnull_ref_owner: Option<TypeName> = (is_ref_under
                && !eu.is_nullable()
                && !eu.non_null().is_array()
                && matches!(eu.non_null(), Ty::String | Ty::Obj(..)))
            .then(|| eu.non_null().kotlin_class_internal())
            .flatten();
            let h = field_hash_ir(ir, v, terminal_underlying, nonnull_ref_owner);
            let sbody = ret_block(ir, h);
            let impl_fid = add_static(ir, "hashCode-impl", vec![u_ir], int_ir, sbody);
            crate::jvm::method_parameters::record_function(ir, impl_fid, &["arg0"], &[0]);
            ir.open_methods.insert(impl_fid);
            ir.jvm_nullability_unannotated_methods.insert(impl_fid);
        }
        let fv = this_field(ir);
        let call = ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: internal_name,
                name: "hashCode-impl".to_string(),
                descriptor: format!("({udesc})I"),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![fv],
        });
        let ibody = ret_block(ir, call);
        if let Some(fid) = add_inst(ir, "hashCode", vec![], int_ir, ibody) {
            ir.open_methods.insert(fid);
            if !custom_hash_code {
                ir.jvm_nullability_unannotated_methods.insert(fid);
            }
        }
    }
    // equals-impl(U v, Object other): other is X && equals-impl0(v, other.unbox-impl())
    // equals(other): return equals-impl(this.field, other)
    {
        if !custom_equals {
            // static: v = slot 0, other = slot 1.
            let mut stmts = Vec::new();
            let other = ir.add_expr(IrExpr::GetValue(1));
            let not_inst = ir.add_expr(IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::NotInstanceOf,
                arg: other,
                type_operand: x_ir,
            });
            stmts.push(guard_false(ir, not_inst));
            let other_v = ir.add_expr(IrExpr::GetValue(1));
            let ocast = ir.add_expr(IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::Cast,
                arg: other_v,
                type_operand: x_ir,
            });
            let ounbox = ir.add_expr(IrExpr::Call {
                callee: Callee::Virtual {
                    owner: internal_name,
                    name: "unbox-impl".to_string(),
                    descriptor: format!("(){udesc}"),
                    params: None,
                    interface: false,
                },
                dispatch_receiver: Some(ocast),
                args: vec![],
            });
            // kotlinc INLINES the underlying comparison here (it does not call `equals-impl0`): the
            // other value is unboxed into a temporary and compared as `arg0 == tmp`, then guarded
            // `if (!eq) return false; return true`. A primitive temporary stays in a local and the
            // guard branches on the negated comparison itself.
            let (tmp, not_eq) = if is_ref_under {
                let v = ir.add_expr(IrExpr::GetValue(0));
                let eq = vc_underlying_eq(ir, v, ounbox, true, terminal_underlying);
                let zero = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(0)));
                let not_eq = ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: crate::ir::IrBinOp::Eq,
                    lhs: eq,
                    rhs: zero,
                });
                (None, not_eq)
            } else {
                const TMP: u32 = 2;
                let declare = ir.add_expr(IrExpr::Variable {
                    index: TMP,
                    ty: u_ir,
                    init: Some(ounbox),
                    named: false,
                });
                let v = ir.add_expr(IrExpr::GetValue(0));
                let tmp = ir.add_expr(IrExpr::GetValue(TMP));
                let not_eq = vc_underlying_ne(ir, v, tmp, terminal_underlying);
                (Some(declare), not_eq)
            };
            stmts.extend(tmp);
            stmts.push(guard_false(ir, not_eq));
            let t = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Boolean(true)));
            stmts.push(ir.add_expr(IrExpr::Return(Some(t))));
            let sbody = ir.add_expr(IrExpr::Block { stmts, value: None });
            let impl_fid = add_static(ir, "equals-impl", vec![u_ir, any_ir], bool_ir, sbody);
            crate::jvm::method_parameters::record_function(ir, impl_fid, &["arg0", "other"], &[0]);
            ir.open_methods.insert(impl_fid);
            ir.jvm_nullability_unannotated_methods.insert(impl_fid);
        }
        // instance equals(other) → return equals-impl(this.field, other)
        let fv = this_field(ir);
        let other_i = ir.add_expr(IrExpr::GetValue(1));
        let call = ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: internal_name,
                name: "equals-impl".to_string(),
                descriptor: format!("({udesc}Ljava/lang/Object;)Z"),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![fv, other_i],
        });
        let ibody = ret_block(ir, call);
        if let Some(fid) = add_inst(ir, "equals", vec![any_ir], bool_ir, ibody) {
            crate::jvm::method_parameters::record_function(ir, fid, &["other"], &[]);
            ir.open_methods.insert(fid);
            if !custom_equals {
                ir.jvm_nullability_unannotated_methods.insert(fid);
            }
        }
    }

    // A secondary constructor becomes a static `constructor-impl` overload. Preserve its semantic
    // declaration shape before consuming the instance-constructor IR: downstream Kotlin compilation
    // selects the source constructor from metadata, then follows the exact static realization handle.
    let secondary_metadata = ir.classes[class_id as usize]
        .secondary_ctors
        .iter()
        .filter_map(|constructor| {
            let metadata_visibility = constructor.metadata_visibility?;
            Some(crate::ir::IrJvmValueClassSecondaryCtor {
                params: constructor.named_params.clone(),
                param_defaults: constructor.defaults.iter().map(Option::is_some).collect(),
                vararg_index: constructor.vararg_index,
                annotations: constructor.annotations.clone(),
                metadata_visibility,
                descriptor: method_descriptor(
                    &jvm_tys(
                        &constructor
                            .params
                            .iter()
                            .map(|parameter| erase(parameter, under))
                            .collect::<Vec<_>>(),
                    ),
                    ir_ty_to_jvm(&eu),
                ),
            })
        })
        .collect::<Vec<_>>();
    if !secondary_metadata.is_empty() {
        ir.jvm_value_class_secondary_ctors
            .insert(internal_name, secondary_metadata);
    }
    let secs = std::mem::take(&mut ir.classes[class_id as usize].secondary_ctors);
    if !secs.is_empty() {
        for sc in secs {
            if !sc.prefix_params.is_empty() {
                return false;
            }
            let crate::ir::CtorDelegateTarget::This {
                target_params,
                to_primary: _,
                default_masks,
            } = &sc.delegate
            else {
                return false;
            };
            if !default_masks.is_empty()
                || !sc.default_parameters.is_empty()
                || sc.delegate_args.len() != target_params.len()
            {
                return false;
            }
            let target_params = target_params
                .iter()
                .map(|parameter| erase(parameter, under))
                .collect::<Vec<_>>();
            let target_descriptor = method_descriptor(&jvm_tys(&target_params), ir_ty_to_jvm(&eu));

            let mut roots = sc.delegate_prelude.clone();
            roots.extend(sc.delegate_args.iter().copied());
            roots.extend(sc.body);
            let delegated_value = max_value_slot(ir, &roots).max(
                u32::try_from(sc.params.len()).expect("value-class constructor parameter count"),
            );

            for &statement in &sc.delegate_prelude {
                shift_slots(ir, statement);
            }
            for &a in &sc.delegate_args {
                shift_slots(ir, a);
            }
            if let Some(body) = sc.body {
                reframe_value_class_secondary(ir, body, delegated_value);
            }

            let mut stmts = sc.delegate_prelude.clone();
            let call = ir.add_expr(IrExpr::Call {
                callee: Callee::Static {
                    owner: internal_name,
                    name: "constructor-impl".to_string(),
                    descriptor: target_descriptor,
                    inline: InlineKind::None,
                },
                dispatch_receiver: None,
                args: sc.delegate_args.clone(),
            });
            stmts.push(ir.add_expr(IrExpr::Variable {
                index: delegated_value,
                ty: u_ir,
                init: Some(call),
                named: false,
            }));
            if let Some(body) = sc.body {
                if let IrExpr::Block { stmts: bs, value } = &ir.exprs[body as usize] {
                    stmts.extend(bs.iter().copied());
                    if let Some(value) = value {
                        stmts.push(*value);
                    }
                } else {
                    stmts.push(body);
                }
            }
            let result = ir.add_expr(IrExpr::GetValue(delegated_value));
            stmts.push(ir.add_expr(IrExpr::Return(Some(result))));
            let body = ir.add_expr(IrExpr::Block { stmts, value: None });
            let constructor = add_static(ir, "constructor-impl", sc.params.clone(), u_ir, body);
            let names = sc
                .named_params
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>();
            crate::jvm::method_parameters::record_function(ir, constructor, &names, &[]);
            ir.fn_source_order.insert(constructor, sc.source_order);
            // `public static`, non-final — like the primary's `constructor-impl`.
            ir.open_methods.insert(constructor);
        }
    }
    true
}

fn max_value_slot(ir: &IrFile, roots: &[ExprId]) -> u32 {
    let mut reachable = HashSet::new();
    for &root in roots {
        collect_reachable_scoped(&ir.exprs, root, &mut reachable);
    }
    reachable
        .into_iter()
        .filter_map(|id| match &ir.exprs[id as usize] {
            IrExpr::GetValue(index)
            | IrExpr::SetValue { var: index, .. }
            | IrExpr::Variable { index, .. } => Some(*index),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

fn reframe_value_class_secondary(ir: &mut IrFile, root: ExprId, this_value: u32) {
    let mut reachable = HashSet::new();
    collect_reachable_scoped(&ir.exprs, root, &mut reachable);
    for id in reachable {
        let index = match &mut ir.exprs[id as usize] {
            IrExpr::GetValue(index)
            | IrExpr::SetValue { var: index, .. }
            | IrExpr::Variable { index, .. } => index,
            _ => continue,
        };
        if *index == 0 {
            *index = this_value;
        } else {
            *index -= 1;
        }
    }
}

/// The value-class underlying-value equality kotlinc emits: a reference compares via
/// `Intrinsics.areEqual`, an IEEE float/double by TOTAL ORDER (`Float.compare(a,b) == 0`, so
/// `NaN == NaN` and `0.0 != -0.0`), every other primitive natively. Shared by `equals-impl0` and the
/// inlined comparison inside `equals-impl`.
fn vc_underlying_eq(
    ir: &mut IrFile,
    a: ExprId,
    b: ExprId,
    is_ref_under: bool,
    underlying: Ty,
) -> ExprId {
    if is_ref_under {
        return ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: type_name("kotlin/jvm/internal/Intrinsics"),
                name: "areEqual".into(),
                descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)Z".into(),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![a, b],
        });
    }
    if matches!(underlying, Ty::Float | Ty::Double) {
        let (owner, desc) = if underlying == Ty::Float {
            ("java/lang/Float", "(FF)I")
        } else {
            ("java/lang/Double", "(DD)I")
        };
        let call = ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: type_name(owner),
                name: "compare".into(),
                descriptor: desc.into(),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![a, b],
        });
        let zero = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(0)));
        return ir.add_expr(IrExpr::PrimitiveBinOp {
            op: crate::ir::IrBinOp::Eq,
            lhs: call,
            rhs: zero,
        });
    }
    ir.add_expr(IrExpr::PrimitiveBinOp {
        op: crate::ir::IrBinOp::Eq,
        lhs: a,
        rhs: b,
    })
}

/// `if (cond) return false`.
/// `a != b` over a primitive underlying, as kotlinc's value-class `equals-impl` negates it: the
/// floating-point types through their wrapper's `compare`, everything else by value.
fn vc_underlying_ne(ir: &mut IrFile, a: ExprId, b: ExprId, underlying: Ty) -> ExprId {
    if matches!(underlying, Ty::Float | Ty::Double) {
        let (owner, desc) = if underlying == Ty::Float {
            ("java/lang/Float", "(FF)I")
        } else {
            ("java/lang/Double", "(DD)I")
        };
        let call = ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: type_name(owner),
                name: "compare".into(),
                descriptor: desc.into(),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![a, b],
        });
        let zero = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(0)));
        return ir.add_expr(IrExpr::PrimitiveBinOp {
            op: crate::ir::IrBinOp::Ne,
            lhs: call,
            rhs: zero,
        });
    }
    ir.add_expr(IrExpr::PrimitiveBinOp {
        op: crate::ir::IrBinOp::Ne,
        lhs: a,
        rhs: b,
    })
}

fn guard_false(ir: &mut IrFile, cond: ExprId) -> ExprId {
    let f = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Boolean(false)));
    let r = ir.add_expr(IrExpr::Return(Some(f)));
    let blk = ir.add_expr(IrExpr::Block {
        stmts: vec![r],
        value: None,
    });
    ir.add_expr(IrExpr::When {
        branches: vec![(Some(cond), blk)],
    })
}

/// `field.hashCode()` for an underlying type (primitive → its wrapper's static `hashCode`, unsigned
/// → its own `hashCode-impl`, reference → `hashCode()` or the null-safe `Objects.hashCode`).
fn field_hash_ir(
    ir: &mut IrFile,
    v: ExprId,
    underlying: Ty,
    nonnull_ref_owner: Option<TypeName>,
) -> ExprId {
    let call = |ir: &mut IrFile, owner: &str, desc: &str, v: ExprId| {
        ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: owner.into(),
                name: "hashCode".into(),
                descriptor: desc.into(),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![v],
        })
    };
    match underlying {
        // kotlinc hashes every primitive through its wrapper's static `hashCode`, and an unsigned
        // underlying through that unsigned class's own `hashCode-impl` over its carrier.
        Ty::Int => call(ir, "java/lang/Integer", "(I)I", v),
        Ty::Short => call(ir, "java/lang/Short", "(S)I", v),
        Ty::Byte => call(ir, "java/lang/Byte", "(B)I", v),
        Ty::Char => call(ir, "java/lang/Character", "(C)I", v),
        Ty::UByte | Ty::UShort | Ty::UInt | Ty::ULong => {
            let carrier = match underlying {
                Ty::UByte => "B",
                Ty::UShort => "S",
                Ty::UInt => "I",
                _ => "J",
            };
            ir.add_expr(IrExpr::Call {
                callee: Callee::Static {
                    owner: underlying
                        .obj_internal()
                        .expect("an unsigned builtin has a classifier identity"),
                    name: "hashCode-impl".into(),
                    descriptor: format!("({carrier})I"),
                    inline: InlineKind::None,
                },
                dispatch_receiver: None,
                args: vec![v],
            })
        }
        Ty::Boolean => call(ir, "java/lang/Boolean", "(Z)I", v),
        Ty::Long => call(ir, "java/lang/Long", "(J)I", v),
        Ty::Double => call(ir, "java/lang/Double", "(D)I", v),
        Ty::Float => call(ir, "java/lang/Float", "(F)I", v),
        _ => match nonnull_ref_owner {
            // `v.hashCode()` on the underlying's own class.
            Some(owner) => ir.add_expr(IrExpr::Call {
                callee: Callee::Virtual {
                    owner,
                    name: "hashCode".into(),
                    descriptor: "()I".into(),
                    params: None,
                    interface: false,
                },
                dispatch_receiver: Some(v),
                args: vec![],
            }),
            None => call(ir, "java/util/Objects", "(Ljava/lang/Object;)I", v),
        },
    }
}
