//! What a function REFERENCE's carrier calls and reflects when a value class stands in its
//! signature.
//!
//! A carrier has two signatures: the public `FunctionN.invoke(Object…)Object` shape stays
//! logical, while the call to the selected target follows JVM value-class erasure and mangling.
//! The target was selected and recorded when the carrier was realized; this pass derives its
//! physical name, descriptor and box/unbox adapters from those recorded facts.

use super::*;

pub(super) fn realize(ir: &mut IrFile, callable_under: &Under, renamed_functions: &HashSet<u32>) {
    for c in &mut ir.classes {
        let owner_fq = c.fq_name();
        let Some(fr) = &mut c.func_ref else {
            continue;
        };
        // The carrier calls an exact generated common-IR adapter. Its signature has already gone
        // through the function erasure/boxing pass, so reuse that physical ABI verbatim instead of
        // independently erasing the adapter's logical function type.
        let local_target = fr.local_target.map(|target| {
            let function = &ir.functions[target as usize];
            (function.name.clone(), function.params.clone(), function.ret)
        });
        let first_call_arg = match fr.dispatch {
            crate::ir::FrDispatch::VirtualUnbound => 1usize,
            _ => usize::from(fr.reflection_receiver_parameter),
        };
        // The lowerer records the already-selected callable's exact target signature. Do not rebuild it
        // from `(owner, name, arity)`: overloads with equal arity are deliberately indistinguishable by
        // that key, and whichever declaration was visited last would corrupt every other reference.
        let target_decl_params = fr
            .reflection_target_param_tys
            .clone()
            .unwrap_or_else(|| fr.target_param_tys[first_call_arg..].to_vec());
        let target_decl_ret = fr.reflection_target_ret_ty.unwrap_or(fr.target_ret_ty);
        // A bound extension reference on a value-class receiver (`Z(42)::test`) targets a facade
        // static whose leading parameter is that receiver; the emitter unboxes it at `invoke`.
        let staticbound = matches!(fr.dispatch, crate::ir::FrDispatch::StaticBound);
        let reflection_base = fr.reflection_name.as_deref().unwrap_or(&fr.fn_name);
        // How the reflected name is realized follows from what was selected, never from its
        // spelling. A source declaration's JVM name derives from its semantic signature: a
        // value-class member is reflected as its static implementation over the carrier
        // (`name-impl`, or its hash-mangled name) taking the receiver first. A constructor stays
        // `<init>`, reached through kotlinc's `DefaultConstructorMarker` accessor when it declares a
        // value-class parameter. A dependency declaration keeps the name and signature its provider
        // published.
        let mangled_reflection_name = match fr.reflected {
            crate::ir::ReflectedCallable::Source => match fr
                .owner_class
                .filter(|owner| callable_under.contains_key(owner))
            {
                Some(owner) => {
                    if let Some(parameters) = &mut fr.reflection_target_param_tys {
                        parameters.insert(0, Ty::obj_name(owner));
                    }
                    vc_member_impl_name(
                        reflection_base,
                        &target_decl_params,
                        &target_decl_ret,
                        callable_under,
                        fr.declaration_suspend,
                    )
                }
                None => vc_mangle(
                    reflection_base,
                    &target_decl_params,
                    &target_decl_ret,
                    callable_under,
                    fr.owner_class.is_none(),
                    fr.declaration_suspend,
                ),
            },
            // A local function is reflected by its lifted name, final only at emission.
            crate::ir::ReflectedCallable::Constructor
            | crate::ir::ReflectedCallable::Physical
            | crate::ir::ReflectedCallable::LocalFunction(_) => reflection_base.to_string(),
        };
        let hidden_constructor = matches!(fr.reflected, crate::ir::ReflectedCallable::Constructor)
            && fr
                .owner_class
                .is_some_and(|owner| !callable_under.contains_key(&owner))
            && target_decl_params.iter().any(|parameter| {
                parameter
                    .non_null()
                    .obj_internal()
                    .is_some_and(|fq| callable_under.contains_key(&fq))
            });
        fr.reflection_name =
            (mangled_reflection_name != fr.fn_name).then_some(mangled_reflection_name);
        if let Some((name, _, _)) = &local_target {
            fr.call_name = name.clone();
        }
        // Preserve classpath erasure already recorded in the target shape.
        let erase_src = fr.target_param_tys.clone();
        let erase_ret = fr.target_ret_ty;
        // A StaticBound receiver that is a VALUE CLASS is captured boxed (`Object`) but the mangled target
        // takes the erased underlying — record it so the emitter unboxes the receiver at `invoke`.
        if staticbound {
            fr.staticbound_recv_unbox = erase_src
                .get(fr.field_capture_count as usize)
                .and_then(|t| t.non_null().obj_internal())
                .filter(|fq| callable_under.contains_key(fq));
        }
        if let Some((_, parameters, result)) = local_target {
            fr.target_param_tys = parameters;
            fr.target_ret_ty = result;
        } else {
            fr.target_param_tys = erase_src.iter().map(|t| erase(t, callable_under)).collect();
            fr.target_ret_ty = erase(&erase_ret, callable_under);
        }
        if let Some(parameters) = &mut fr.reflection_target_param_tys {
            for parameter in parameters {
                *parameter = erase(parameter, callable_under);
            }
        }
        if let Some(result) = &mut fr.reflection_target_ret_ty {
            *result = erase(result, callable_under);
        }
        if hidden_constructor {
            if let Some(parameters) = &mut fr.reflection_target_param_tys {
                parameters.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
            }
        }
        let target_offset = usize::from(staticbound);
        fr.unbox_params = fr
            .param_tys
            .iter()
            .enumerate()
            .map(|(i, logical)| {
                let target = fr.target_param_tys.get(i + target_offset)?;
                let fq = logical.non_null().obj_internal()?;
                // `FunctionN.invoke` always receives the logical value as an Object. Adapt it to
                // the selected callable's *physical* parameter, not merely to a type with different
                // nullability. In particular, `Value` passed to `fun f(Value?)` stays the boxed
                // `Value`: the nullable primitive-backed value class is itself the target JVM
                // reference. Unboxing it would call `f(int)` although only `f(Value)` exists.
                (callable_under.contains_key(&fq)
                    && logical != target
                    && target.non_null().obj_internal() != Some(fq))
                .then_some(fq)
            })
            .collect();
        fr.unbox_param_nullable = fr
            .param_tys
            .iter()
            // Null-preserving unboxing depends on what `FunctionN.invoke` may receive. An
            // adapted reference can narrow a nullable declaration parameter to a non-null
            // function parameter; using the declaration's nullability then fabricates a null
            // branch for a primitive target slot and produces an impossible stack-map join.
            .map(|parameter| parameter.is_nullable())
            .collect();
        fr.box_ret = fr.ret_ty.non_null().obj_internal().and_then(|fq| {
            (callable_under.contains_key(&fq)
                && target_decl_ret.non_null().obj_internal() == Some(fq)
                && fr.ret_ty != fr.target_ret_ty)
                .then_some(fq)
        });
        fr.invoke_renamed = fr
            .invoke
            .is_some_and(|invoke| renamed_functions.contains(&invoke));
        crate::trace_compiler!(
            "value_classes",
            "func_ref {} call_name={} ret_ty={:?} target_ret={:?} box_ret={:?}",
            owner_fq,
            fr.call_name,
            fr.ret_ty,
            fr.target_ret_ty,
            fr.box_ret
        );
    }
}
