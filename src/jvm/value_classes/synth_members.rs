//! The members a `@JvmInline value class` gets that its source never wrote.
//!
//! `unbox-impl`, `box-impl`, `constructor-impl`, `equals-impl0` and the structural
//! `equals`/`hashCode`/`toString` are JVM realization, not Kotlin declarations, so they are
//! synthesized here rather than in common lowering. The plain single-field class — the field, the
//! `<init>` and the underlying getter — is already in the IR by the time this runs.

use super::*;
use member_signatures::RepresentationMember::*;

mod member_signatures;

/// Exact function identities whose JVM realization this synthesis already finalized, so the
/// signature pass that follows does not lower them a second time.
#[derive(Default)]
pub(super) struct SynthesizedValueMembers {
    /// Computed property accessors realized as static implementations over the carrier, with the
    /// name the box answers to for each. Both names were chosen from the declared accessor
    /// signature; the carrier parameter they gained here is not part of that signature and must
    /// not be hashed into either name again.
    pub(super) accessors: HashMap<u32, String>,
    /// The static `constructor-impl` realizing each source constructor, by its class and its
    /// [`crate::ir::IrConstructorTarget::ordinal`], so a construction reaches the exact overload
    /// its checked selection named.
    pub(super) constructor_impls: HashMap<(TypeName, u32), u32>,
    /// Exact synthesized expressions that have already rendered a nested value-class carrier to a
    /// String while retaining the nested class as their logical concat-boundary type.
    pub(super) rendered_value_class_text: HashSet<ExprId>,
    /// Exact generated instance methods that delegate Kotlin `Any` members to their static
    /// carrier implementations. A same-spelled source overload is not one of these functions.
    pub(super) any_delegators: HashSet<u32>,
}

impl SynthesizedValueMembers {
    /// Every user value-class declaration realized as a static over the carrier: the `lowered`
    /// member functions and the computed property accessors. Their bodies, returns, interface
    /// entries and bridge targets follow that one realized ABI alike.
    pub(super) fn static_members(&self, lowered: &HashSet<u32>) -> HashSet<u32> {
        lowered
            .iter()
            .chain(self.accessors.keys())
            .copied()
            .collect()
    }
}

/// A declaration written in a value class's constructor or `init` block (a local class, a lambda's
/// class) is enclosed by the static `constructor-impl` realizing that constructor, the primary's
/// for an `init` block, as kotlinc's `EnclosingMethod` names it: the instance constructor does not
/// exist on the JVM. A callable lifted
/// out of a constructor or an `init` block is likewise contained by that `constructor-impl`, whose
/// name kotlinc gives it (`constructor_impl$lambda$0`).
pub(super) fn enclose_in_constructor_impls(ir: &mut IrFile, realized: &SynthesizedValueMembers) {
    let names = ir
        .classes
        .iter()
        .map(|class| class.fq_name)
        .collect::<Vec<_>>();
    let constructor_impl = |enclosure: Option<crate::ir::IrEnclosure>| match enclosure? {
        crate::ir::IrEnclosure::Constructor { class, ordinal } => realized
            .constructor_impls
            .get(&(names[class as usize], ordinal))
            .copied(),
        crate::ir::IrEnclosure::ClassInitializer(class) => realized
            .constructor_impls
            .get(&(names[class as usize], 0))
            .copied(),
        _ => None,
    };
    for declaration in &mut ir.classes {
        if let Some(function) = constructor_impl(declaration.enclosure) {
            declaration.enclosure = Some(crate::ir::IrEnclosure::Function(function));
        }
    }
    for entry in ir
        .lifting_sequences
        .values_mut()
        .flat_map(|entries| entries.values_mut())
    {
        if let Some(function) = constructor_impl(entry.container) {
            entry.container = Some(crate::ir::IrEnclosure::Function(function));
        }
    }
}

/// Record every function whose JVM name the value-class pass chose: one it mangled or renamed
/// `-impl`, a static accessor implementation, and each `constructor-impl`.
pub(super) fn record_renamed_functions(
    ir: &mut IrFile,
    realized: &SynthesizedValueMembers,
    renamed: &HashSet<u32>,
) {
    ir.value_class_renamed_functions.extend(
        renamed
            .iter()
            .chain(realized.accessors.keys())
            .chain(realized.constructor_impls.values()),
    );
}

/// Source functions that the frontend bound to Kotlin `Any` members, keyed by their exact common
/// IR realization. A same-spelled function without that checked override edge is deliberately
/// absent, regardless of its physical name or arity.
fn custom_any_roles(ir: &IrFile, owner: TypeName) -> HashMap<u32, crate::types::SemanticCallRole> {
    ir.function_overrides
        .get(&owner)
        .into_iter()
        .flatten()
        .filter_map(|edge| {
            let role = edge.overridden_semantic_role?;
            if !matches!(
                role,
                crate::types::SemanticCallRole::KotlinAnyEquals
                    | crate::types::SemanticCallRole::KotlinAnyHashCode
                    | crate::types::SemanticCallRole::KotlinAnyToString
            ) {
                return None;
            }
            let crate::fir::ResolvedFunctionOverrideTarget::Module(declaration) =
                edge.implementation
            else {
                return None;
            };
            ir.checked_callable_functions
                .get(&declaration)
                .copied()
                .map(|function| (function, role))
        })
        .collect()
}

/// Synthesize a value class's unboxed-support members directly in the IR (a JVM concern, so it lives in
/// this pass, not common lowering): `unbox-impl`/`box-impl`/`constructor-impl`/`equals-impl0` plus structural
/// `equals`/`hashCode`/`toString` (skipped where the user defined one). The plain single-field class
/// (field, `<init>`, getter) is already present in common IR. `callable_under` is the value-class set
/// JVM member-name mangling consults (it additionally knows the native unsigned classes).
pub(super) fn synth_value_members(
    ir: &mut IrFile,
    class_id: u32,
    under: &Under,
    callable_under: &Under,
    has_init: bool,
    constructor_default: Option<ExprId>,
    realized: &mut SynthesizedValueMembers,
) -> bool {
    let internal_name = ir.classes[class_id as usize].fq_name;
    let fname = ir.classes[class_id as usize].fields[0].name.clone();
    let u_ir = under.get(&internal_name).copied().unwrap_or(Ty::Error);
    // The FULLY-ERASED underlying: a NESTED value class erases through its chain to the first type that
    // stops unboxing — `NZ2(NZ1)` where `NZ1(Z?)` erases to a BOXED `Z` (`LZ;`), not `LNZ1;`. The static
    // `-impl` members take this erased type (matching kotlinc), so their hardcoded delegation descriptors
    // must use it too, or the operand-stack type won't match the actual method signature (a VerifyError).
    let eu = erase(&u_ir, under);
    // The underlying JVM descriptor (`Ljava/lang/String;`, `I`, `LZ;`, …) — the argument type of the
    // static `-impl` members, which the instance methods delegate to (matching kotlinc's value-class shape).
    let udesc = type_descriptor(ir_ty_to_jvm(&eu));
    let x_ir = Ty::obj_name(internal_name);
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
        .flat_map(|(index, property)| {
            [
                property
                    .getter
                    .map(|getter| (index, getter, property_getter_name(&property.name))),
                property.setter.map(|setter| {
                    (
                        index,
                        setter,
                        crate::names::property_setter_name(&property.name),
                    )
                }),
            ]
        })
        .flatten()
        .collect::<Vec<_>>();
    for (property_index, accessor, source_name) in computed_accessors {
        // Named from the declared accessor signature, exactly like every other value-class member:
        // `getX-impl`, or kotlinc's hash when that signature mentions a value class. The box
        // answers to the same signature under its entry name (`getX`, or that hash).
        let (jvm_name, entry_name, declared, ret) = {
            let function = &ir.functions[accessor as usize];
            (
                vc_member_impl_name(
                    &source_name,
                    &function.params,
                    &function.ret,
                    callable_under,
                    false,
                ),
                vc_member_entry_name(
                    &source_name,
                    &function.params,
                    &function.ret,
                    callable_under,
                    false,
                ),
                function.params.clone(),
                function.ret,
            )
        };
        realized.accessors.insert(accessor, entry_name);
        let is_getter = {
            let function = &mut ir.functions[accessor as usize];
            function.name.clone_from(&jvm_name);
            function.params.insert(0, u_ir);
            function.is_static = true;
            ir.classes[class_id as usize].properties[property_index].getter == Some(accessor)
        };
        let member = Receiver {
            rest: &declared,
            ret,
        };
        member_signatures::record(ir, class_id, accessor, member);
        crate::jvm::method_parameters::prepend_value_class_receiver(ir, accessor, "arg0");
        let property = &mut ir.classes[class_id as usize].properties[property_index];
        if is_getter {
            property.getter_jvm_name = Some(jvm_name);
        } else {
            property.setter_jvm_name = Some(jvm_name);
        }
    }

    // User-written Any overrides become the static `-impl` body. Its former receiver slot 0 is exactly
    // the first static parameter slot, so the body itself needs no slot rewrite; value-class property
    // reads inside it are lowered to the carrier later in this pass. The ordinary instance override is
    // synthesized below as the ABI delegator back to this implementation.
    let custom_any_roles = custom_any_roles(ir, internal_name);
    let mut custom_equals = false;
    let mut custom_hash_code = false;
    let mut custom_to_string = false;
    let mut custom_carrier_functions = Vec::new();
    for &fid in &ir.classes[class_id as usize].methods.clone() {
        let Some(function) = ir.functions.get_mut(fid as usize) else {
            continue;
        };
        let custom_impl = match custom_any_roles.get(&fid) {
            Some(crate::types::SemanticCallRole::KotlinAnyEquals) => {
                custom_equals = true;
                Some("equals-impl")
            }
            Some(crate::types::SemanticCallRole::KotlinAnyHashCode) => {
                custom_hash_code = true;
                Some("hashCode-impl")
            }
            Some(crate::types::SemanticCallRole::KotlinAnyToString) => {
                custom_to_string = true;
                Some("toString-impl")
            }
            _ => None,
        };
        if let Some(name) = custom_impl {
            let declared = (function.params.clone(), function.ret);
            // Metadata describes the override as source declared it; only its JVM realization
            // becomes the static `-impl` over the carrier.
            ir.vc_declared_sigs
                .insert(fid, (function.name.clone(), declared.0.clone(), declared.1));
            function.name = name.to_string();
            function.params.insert(0, u_ir);
            function.is_static = true;
            custom_carrier_functions.push((fid, declared));
        }
    }
    for (function, (rest, ret)) in custom_carrier_functions {
        let member = Receiver { rest: &rest, ret };
        member_signatures::record(ir, class_id, function, member);
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
    let add_inst = |ir: &mut IrFile, name: &str, params: Vec<Ty>, ret: Ty, body: ExprId| -> u32 {
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
        fid
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
        let fid = add_inst(ir, "unbox-impl", vec![], u_ir, body);
        ir.synthetic_methods.insert(fid);
        ir.jvm_value_class_representation_order.insert(fid, 2);
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
    // constructed value. The init runs HERE, not in `box-impl`/`<init>`: `box-impl` only wraps an
    // already-built value. MOVE `init_body` out of the class (so `<init>` keeps only the field
    // assignment). Like kotlinc, the constructed value is first an unnamed temporary over the carrier
    // (slot 1), which the init block reads as `this` (instance slot 0), while the parameter moves from
    // instance slot 1 to 0. The temporary is typed as the value class, so a read of its property is the
    // carrier and a `this` passed as a reference is boxed (see the `constructor-impl` entry in `s4_bodies`).
    {
        let mut stmts = Vec::new();
        let mut result = 0;
        if let Some(init_root) = ir.classes[class_id as usize]
            .init_body
            .take()
            .filter(|_| has_init)
        {
            let mut reach = HashSet::new();
            collect_reachable_scoped(&ir.exprs, init_root, &mut reach);
            // `this` is the temporary; the parameter, which is the class's sole property, is read
            // as that property of the temporary. Later locals keep their slots.
            for id in reach {
                match ir.exprs[id as usize] {
                    IrExpr::GetValue(0) => ir.exprs[id as usize] = IrExpr::GetValue(1),
                    IrExpr::GetValue(1) => {
                        let receiver = ir.add_expr(IrExpr::GetValue(1));
                        ir.exprs[id as usize] = IrExpr::GetField {
                            receiver,
                            class: class_id,
                            index: 0,
                        };
                    }
                    _ => {}
                }
            }
            let parameter = ir.add_expr(IrExpr::GetValue(0));
            stmts.push(ir.add_expr(IrExpr::Variable {
                index: 1,
                ty: x_ir,
                init: Some(parameter),
                named: false,
            }));
            match ir.exprs[init_root as usize].clone() {
                IrExpr::Block { stmts: bs, value } => stmts.extend(bs.into_iter().chain(value)),
                _ => stmts.push(init_root),
            }
            result = 1;
        }
        let arg = ir.add_expr(IrExpr::GetValue(result));
        stmts.push(ir.add_expr(IrExpr::Return(Some(arg))));
        let body = ir.add_expr(IrExpr::Block { stmts, value: None });
        let cfid = add_static(ir, "constructor-impl", vec![u_ir], u_ir, body);
        ir.functions[cfid as usize].param_checks = vec![ir.classes[class_id as usize]
            .ctor_args
            .first()
            .and_then(|argument| argument.check.as_ref())
            .map(|_| crate::ir::IrParameterCheck::NonNull)];
        let declared = [ir.classes[class_id as usize].fields[0].ty];
        member_signatures::record(ir, class_id, cfid, Constructor(&declared));
        ir.jvm_value_class_representation_order.insert(cfid, 0);
        realized.constructor_impls.insert((internal_name, 0), cfid);
        ir.jvm_value_class_constructor_impls.insert(cfid, 0);
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
                crate::ir::FnParamInfo::source_defaults(vec![fname.clone()], vec![Some(def)]),
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
                    descriptor: format!("({udesc})L{};", internal_name.render()),
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
        member_signatures::record(ir, class_id, function, SpecializedEquals);
        ir.fn_params.insert(
            function,
            crate::ir::FnParamInfo::identities(
                [1, 2]
                    .map(|ordinal| {
                        crate::ir::IrParameterIdentity::generated(
                            crate::ir::IrGeneratedParameterRole::ValueClassEqualsOperand {
                                ordinal,
                            },
                            None,
                        )
                    })
                    .to_vec(),
            ),
        );
        ir.jvm_value_class_representation_order.insert(function, 3);
        ir.jvm_nullability_unannotated_methods.insert(function);
    }
    // kotlinc emits the logic in a static `<name>-impl(U)` operating on the unboxed value, and the
    // instance method delegates to it (`toString()` → `toString-impl(this.field)`). The instance methods
    // and the `-impl` statics are all `open` (non-`final`).
    // toString-impl(U v): "X(field=" + v + ")" ; toString(): return toString-impl(this.field)
    {
        let simple = internal_name.segment_ref().replace('$', ".");
        if !custom_to_string {
            let carrier = ir.add_expr(IrExpr::GetValue(0));
            // A non-null nested value class is this slot's carrier. Name it through its own
            // `toString-impl` (`Outer(i=Inner(x=20))`); the concat then appends that String.
            let rendered = nested_value_text(
                ir,
                u_ir,
                under,
                carrier,
                &mut realized.rendered_value_class_text,
            );
            // ONE `StringConcat` (not nested `+`): kotlinc builds a single `StringBuilder` and appends the
            // 1-char closing paren via `append(C)` — a nested concat would emit a second builder.
            let prefix = str_const(ir, format!("{simple}({fname}="));
            let close = str_const(ir, ")".to_string());
            let acc = ir.add_expr(IrExpr::StringConcat(vec![prefix, rendered, close]));
            let sbody = ret_block(ir, acc);
            let impl_fid = add_static(ir, "toString-impl", vec![u_ir], str_ir, sbody);
            ir.jvm_value_class_generated_any
                .insert(impl_fid, crate::ir::IrValueClassAnyMember::ToString);
            let member = Receiver {
                rest: &[],
                ret: str_ir,
            };
            member_signatures::record(ir, class_id, impl_fid, member);
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
        let fid = add_inst(ir, "toString", vec![], str_ir, ibody);
        realized.any_delegators.insert(fid);
        ir.open_methods.insert(fid);
        if !custom_to_string {
            ir.jvm_nullability_unannotated_methods.insert(fid);
        }
    }
    // hashCode-impl(U v): v.hashCode() ; hashCode(): return hashCode-impl(this.field)
    {
        if !custom_hash_code {
            let h = property_hash(ir, u_ir);
            let sbody = ret_block(ir, h);
            let impl_fid = add_static(ir, "hashCode-impl", vec![u_ir], int_ir, sbody);
            ir.jvm_value_class_generated_any
                .insert(impl_fid, crate::ir::IrValueClassAnyMember::HashCode);
            let member = Receiver {
                rest: &[],
                ret: int_ir,
            };
            member_signatures::record(ir, class_id, impl_fid, member);
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
        let fid = add_inst(ir, "hashCode", vec![], int_ir, ibody);
        realized.any_delegators.insert(fid);
        ir.open_methods.insert(fid);
        if !custom_hash_code {
            ir.jvm_nullability_unannotated_methods.insert(fid);
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
            // `unbox-impl`'s return is erased to the fully erased carrier before emission. The
            // temporary is declared as the immediate underlying and erased to that same carrier.
            // The call descriptor has to name the carrier now: a nested value class would otherwise
            // return its box into the primitive store.
            let ounbox = super::unboxing_rewrites::unbox_call(ir, ocast, internal_name, &eu);
            // kotlinc INLINES the underlying comparison here (it does not call `equals-impl0`): the
            // other value is unboxed into a temporary and compared as `arg0 == tmp`, then guarded
            // `if (!eq) return false; return true`. Temporary elimination later keeps a reference
            // temporary on the stack (`aload_0; swap`); a primitive one stays in its local.
            const TMP: u32 = 2;
            let declare = ir.add_expr(IrExpr::Variable {
                index: TMP,
                ty: u_ir,
                init: Some(ounbox),
                named: false,
            });
            let v = ir.add_expr(IrExpr::GetValue(0));
            let tmp = ir.add_expr(IrExpr::GetValue(TMP));
            let not_eq = if is_ref_under {
                // The declared type's own equality, as a data class compares a property: a nested
                // value class's `equals-impl0`, otherwise `Intrinsics.areEqual`.
                let eq = ir.add_expr(IrExpr::Call {
                    callee: Callee::Intrinsic {
                        operation: crate::ir::IrIntrinsic::GeneratedPropertyEquals { ty: u_ir },
                        ret: bool_ir,
                    },
                    dispatch_receiver: None,
                    args: vec![v, tmp],
                });
                let zero = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(0)));
                ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: crate::ir::IrBinOp::Eq,
                    lhs: eq,
                    rhs: zero,
                })
            } else {
                vc_underlying_ne(ir, v, tmp, terminal_underlying)
            };
            stmts.push(declare);
            stmts.push(guard_false(ir, not_eq));
            let t = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Boolean(true)));
            stmts.push(ir.add_expr(IrExpr::Return(Some(t))));
            let sbody = ir.add_expr(IrExpr::Block { stmts, value: None });
            let impl_fid = add_static(ir, "equals-impl", vec![u_ir, any_ir], bool_ir, sbody);
            ir.jvm_value_class_generated_any
                .insert(impl_fid, crate::ir::IrValueClassAnyMember::Equals);
            let other = [Ty::nullable(any_ir)];
            let member = Receiver {
                rest: &other,
                ret: bool_ir,
            };
            member_signatures::record(ir, class_id, impl_fid, member);
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
        let fid = add_inst(ir, "equals", vec![any_ir], bool_ir, ibody);
        realized.any_delegators.insert(fid);
        crate::jvm::method_parameters::record_function(ir, fid, &["other"], &[]);
        ir.open_methods.insert(fid);
        if !custom_equals {
            ir.jvm_nullability_unannotated_methods.insert(fid);
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
        for (secondary, sc) in secs.into_iter().enumerate() {
            if !sc.prefix_params.is_empty() {
                return false;
            }
            let crate::ir::CtorDelegateTarget::This {
                target_params,
                target: _,
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
                reframe_value_class_secondary(ir, body, delegated_value, sc.lines.decl_line);
            }
            // A default reads the earlier parameters, which move down with the static layout.
            for &default in sc.defaults.iter().flatten() {
                shift_slots(ir, default);
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
            // Typed as the value class, like the primary's temporary: `this` is the carrier.
            let delegation = ir.add_expr(IrExpr::Variable {
                index: delegated_value,
                ty: x_ir,
                init: Some(call),
                named: false,
            });
            mark_line(ir, delegation, sc.lines.delegation_line);
            stmts.push(delegation);
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
            // The fall-through return belongs to the declaration, like kotlinc's `return $this`.
            let result = ir.add_expr(IrExpr::GetValue(delegated_value));
            let fall_through = ir.add_expr(IrExpr::Return(Some(result)));
            mark_line(ir, fall_through, sc.lines.decl_line);
            if sc.lines.decl_line != 0 {
                ir.mark_implicit_return_end_line(fall_through, sc.lines.decl_line);
            }
            stmts.push(fall_through);
            let body = ir.add_expr(IrExpr::Block { stmts, value: None });
            let constructor = add_static(ir, "constructor-impl", sc.params.clone(), u_ir, body);
            // Preserve the source constructor's entry contract on its static JVM realization.
            // The later representation pass removes a guard when the selected carrier is
            // primitive or admits null; a non-null reference parameter keeps the same guard and
            // source name kotlinc gives the declared constructor.
            ir.functions[constructor as usize].param_checks = sc.param_checks.clone();
            member_signatures::record(ir, class_id, constructor, Constructor(&sc.params));
            if sc.lines.decl_line != 0 {
                ir.fn_decl_lines.insert(constructor, sc.lines.decl_line);
            }
            // Like the primary's, its first parameter is a declared parameter, not a receiver, so
            // the default stub counts it in the mask.
            ir.functions[constructor as usize].dispatch_receiver = None;
            let ordinal = u32::try_from(secondary + 1).expect("constructor ordinal fits in u32");
            realized
                .constructor_impls
                .insert((internal_name, ordinal), constructor);
            ir.jvm_value_class_constructor_impls
                .insert(constructor, ordinal);
            let names = sc
                .named_params
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>();
            // Omitted arguments are filled by `constructor-impl$default`, as for the primary.
            if sc.defaults.iter().any(Option::is_some) {
                let names = names.iter().map(|name| (*name).to_string()).collect();
                ir.fn_params.insert(
                    constructor,
                    crate::ir::FnParamInfo::source_defaults(names, sc.defaults.clone()),
                );
            }
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

fn reframe_value_class_secondary(ir: &mut IrFile, root: ExprId, this_value: u32, decl_line: u32) {
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
    // The body's own `return` (always `Unit`) leaves the static `constructor-impl`, so it yields
    // the constructed value, after evaluating any operand other than the `Unit` singleton.
    let returns = reachable_returns(ir, root);
    for id in returns {
        let IrExpr::Return(operand) = ir.exprs[id as usize] else {
            unreachable!("collected a return")
        };
        let value = ir.add_expr(IrExpr::GetValue(this_value));
        let value = match operand {
            Some(operand) if !matches!(ir.exprs[operand as usize], IrExpr::UnitInstance) => ir
                .add_expr(IrExpr::Block {
                    stmts: vec![operand],
                    value: Some(value),
                }),
            _ => value,
        };
        ir.exprs[id as usize] = IrExpr::Return(Some(value));
        // The value loads on the `return`'s own line; the return itself is the declaration's.
        if decl_line != 0 {
            ir.mark_implicit_return_end_line(id, decl_line);
        }
    }
}

fn mark_line(ir: &mut IrFile, statement: ExprId, line: u32) {
    if line != 0 {
        ir.expr_lines.insert(statement, line);
    }
}

fn reachable_returns(ir: &IrFile, root: ExprId) -> Vec<ExprId> {
    let mut reachable = HashSet::new();
    collect_reachable_scoped(&ir.exprs, root, &mut reachable);
    let mut returns = reachable
        .into_iter()
        .filter(|&id| matches!(ir.exprs[id as usize], IrExpr::Return(_)))
        .collect::<Vec<_>>();
    returns.sort_unstable();
    returns
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

/// Render a non-null nested value class with its static `toString-impl`. The slot already holds
/// that class's carrier, and the call's descriptor names the fully erased carrier. Its logical
/// type stays the nested class, so the `StringBuilder` path appends with `append(Object)`; its
/// physical result is the `String` the call returns, which an `invokedynamic` concat passes as
/// `String`. A nullable property is left as the value erasure stored.
fn nested_value_text(
    ir: &mut IrFile,
    underlying: Ty,
    under: &Under,
    value: ExprId,
    rendered_value_class_text: &mut HashSet<ExprId>,
) -> ExprId {
    if underlying.is_nullable() {
        return value;
    }
    let Some(nested) = underlying.non_null().obj_internal() else {
        return value;
    };
    if !under.contains_key(&nested) {
        return value;
    }
    let carrier = erase(&underlying, under);
    let call = ir.add_expr(IrExpr::Call {
        callee: Callee::Static {
            owner: nested,
            name: "toString-impl".to_string(),
            descriptor: format!("({})Ljava/lang/String;", super::desc(&carrier)),
            inline: InlineKind::None,
        },
        dispatch_receiver: None,
        args: vec![value],
    });
    ir.physical_types.insert(call, Ty::String);
    ir.logical_types.insert(call, Ty::obj_name(nested));
    rendered_value_class_text.insert(call);
    call
}

/// kotlinc's generated-member hash of the sole property, the one a data class gives each of its
/// properties: the declared type's own hash, behind `v == null ? 0 : …` when the type admits null
/// (`String?`, `Int?`, an unbounded `T`).
fn property_hash(ir: &mut IrFile, underlying: Ty) -> ExprId {
    let hash = |ir: &mut IrFile| {
        let value = ir.add_expr(IrExpr::GetValue(0));
        ir.add_expr(IrExpr::Call {
            callee: Callee::Intrinsic {
                operation: crate::ir::IrIntrinsic::GeneratedPropertyHash { ty: underlying },
                ret: Ty::Int,
            },
            dispatch_receiver: None,
            args: vec![value],
        })
    };
    if !underlying.upper_bound_admits_null() {
        return hash(ir);
    }
    let value = ir.add_expr(IrExpr::GetValue(0));
    let null = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null));
    let is_null = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: crate::ir::IrBinOp::RefEq,
        lhs: value,
        rhs: null,
    });
    let zero = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(0)));
    let non_null = hash(ir);
    ir.add_expr(IrExpr::When {
        branches: vec![(Some(is_null), zero), (None, non_null)],
    })
}

/// Decrement every value-slot index (`GetValue`/`SetValue`/`Variable`) reachable from `root` by one —
/// reframing an instance-lowered body (`this` at slot 0) as a static one (params at slot 0).
pub(super) fn shift_slots(ir: &mut IrFile, root: ExprId) {
    let mut reach = HashSet::new();
    collect_reachable_scoped(&ir.exprs, root, &mut reach);
    for id in reach {
        match &mut ir.exprs[id as usize] {
            IrExpr::GetValue(i)
            | IrExpr::SetValue { var: i, .. }
            | IrExpr::Variable { index: i, .. } => {
                *i = i.saturating_sub(1);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn override_edge(
        owner: TypeName,
        implementation: crate::fir::CallableId,
        role: Option<crate::types::SemanticCallRole>,
    ) -> crate::ir::IrFunctionOverride {
        crate::ir::IrFunctionOverride {
            implementation: crate::fir::ResolvedFunctionOverrideTarget::Module(implementation),
            implementation_function: None,
            implementation_owner: owner,
            overridden: crate::fir::ResolvedFunctionOverrideTarget::Module(
                crate::fir::CallableId::from_raw(99),
            ),
            overridden_owner: crate::types::type_name("kotlin/Any"),
            overridden_semantic_role: role,
            collection_barrier: None,
            overridden_is_interface: false,
            name: "equals".to_string(),
            declared_parameters: vec![Ty::obj("kotlin/Any")],
            declared_result: Ty::Boolean,
            applied_parameters: vec![Ty::obj("kotlin/Any")],
            applied_result: Ty::Boolean,
            implementation_parameters: vec![Ty::String],
            implementation_parameter_identities: Vec::new(),
            overridden_parameter_identities: Vec::new(),
            implementation_result: Ty::Boolean,
            suspend: false,
            has_kotlin_superclass_override: false,
            depth: 1,
        }
    }

    #[test]
    fn same_named_function_is_custom_any_only_with_the_checked_override_role() {
        let mut ir = IrFile::default();
        let owner = crate::types::type_name("app/Token");
        let declaration = crate::fir::CallableId::from_raw(7);
        let function = 3;
        ir.checked_callable_functions.insert(declaration, function);
        ir.function_overrides
            .insert(owner, vec![override_edge(owner, declaration, None)]);

        assert!(custom_any_roles(&ir, owner).is_empty());

        ir.function_overrides.get_mut(&owner).unwrap()[0].overridden_semantic_role =
            Some(crate::types::SemanticCallRole::KotlinAnyEquals);
        assert_eq!(
            custom_any_roles(&ir, owner),
            HashMap::from([(function, crate::types::SemanticCallRole::KotlinAnyEquals)])
        );
    }
}
