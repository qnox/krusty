//! What a function REFERENCE's carrier calls and reflects when a value class stands in its
//! signature.
//!
//! A carrier has two signatures: the public `FunctionN.invoke(Object…)Object` shape stays
//! logical, while the call to the selected target follows JVM value-class erasure and mangling.
//! The target was selected and recorded when the carrier was realized; this pass derives its
//! physical name, descriptor and box/unbox adapters from those recorded facts.

use super::*;

pub(super) fn realize(
    ir: &mut IrFile,
    callable_under: &Under,
    suspend_sig: &HashSet<(Option<TypeName>, String, usize)>,
    renamed_functions: &HashSet<u32>,
) {
    for c in &mut ir.classes {
        let owner_fq = c.fq_name();
        let Some(fr) = &mut c.func_ref else {
            continue;
        };
        let local_target = fr.local_target.and_then(|target| {
            ir.functions
                .get(target as usize)
                .map(|function| (function.name.clone(), function.params.clone(), function.ret))
        });
        let first_call_arg = match fr.dispatch {
            crate::ir::FrDispatch::VirtualUnbound => 1usize,
            _ => usize::from(fr.reflection_receiver_parameter),
        };
        let call_owner = fr.call_owner;
        // The lowerer records the already-selected callable's exact target signature. Do not rebuild it
        // from `(owner, name, arity)`: overloads with equal arity are deliberately indistinguishable by
        // that key, and whichever declaration was visited last would corrupt every other reference.
        let target_decl_params = fr
            .reflection_target_param_tys
            .clone()
            .unwrap_or_else(|| fr.target_param_tys[first_call_arg..].to_vec());
        let target_decl_ret = fr.reflection_target_ret_ty.unwrap_or(fr.target_ret_ty);
        // A BOUND extension reference on a VALUE-CLASS receiver (`Z(42)::test`, `FrDispatch::StaticBound`)
        // targets a facade static whose leading param is the receiver — that receiver lives in
        // `target_param_tys` (the `target_override`), NOT in the invoke `param_tys`. Mangle against that
        // full sig (so `test` → `test-<hash>`), treat it as a file-class member, and erase THAT sig (so the
        // target descriptor keeps the receiver `int`, not an empty `()`), else the impl calls a
        // non-existent unmangled `test()`.
        let staticbound = matches!(fr.dispatch, crate::ir::FrDispatch::StaticBound);
        let call_is_file_class =
            matches!(fr.dispatch, crate::ir::FrDispatch::Static) || staticbound;
        let call_mangle_params = if staticbound {
            fr.target_param_tys.clone()
        } else {
            fr.target_param_tys[first_call_arg..].to_vec()
        };
        let fr_suspend =
            suspend_sig.contains(&(call_owner, fr.call_name.clone(), target_decl_params.len()));
        // Mangling is IDEMPOTENT here. A target from a DEPENDENCY already carries its final JVM name —
        // kotlinc mangled it when that dependency was built, and the lowerer recorded that physical
        // name — so a second pass produced `decode-X4E9McA-X4E9McA`: a method that exists nowhere, and
        // a reflection signature kotlin-reflect cannot resolve. `vc_mangle_once` leaves a name that
        // already carries exactly the suffix this signature would append. (Origin cannot be the test:
        // this pass sees one FILE, so a sibling source file's target looks foreign while its own run
        // does mangle it — skipping there emitted a call to an unmangled method that never exists.)
        let mangle_call_once = |base: &str| {
            vc_mangle_once(
                base,
                &call_mangle_params,
                &fr.target_ret_ty,
                callable_under,
                call_is_file_class,
                fr_suspend,
            )
        };
        let mangle_reflection_once = |base: &str| {
            vc_mangle_once(
                base,
                &target_decl_params,
                &target_decl_ret,
                callable_under,
                fr.owner_class.is_none(),
                fr_suspend,
            )
        };
        // A structural adapter invokes an exact generated common-IR function. Its signature has
        // already gone through the function erasure/boxing pass above; reuse that physical ABI
        // verbatim instead of independently erasing the adapter's logical function type.
        let mangled_call_name = local_target
            .as_ref()
            .map(|(name, _, _)| name.clone())
            .unwrap_or_else(|| mangle_call_once(&fr.call_name));
        let reflection_base = fr.reflection_name.as_deref().unwrap_or(&fr.fn_name);
        // A source value-class member is reflected as its static implementation over the carrier:
        // `name-impl`, or its hash-mangled name, taking the receiver first. A dependency target
        // already carries that physical name, and a constructor is reflected as `<init>`.
        let value_class_member = match fr.reflected {
            crate::ir::ReflectedCallable::Source => fr
                .owner_class
                .filter(|owner| callable_under.contains_key(owner)),
            crate::ir::ReflectedCallable::Constructor | crate::ir::ReflectedCallable::Physical => {
                None
            }
        };
        let mangled_reflection_name = match value_class_member {
            Some(owner) => {
                if let Some(parameters) = &mut fr.reflection_target_param_tys {
                    parameters.insert(0, Ty::obj_name(owner));
                }
                vc_member_impl_name(
                    reflection_base,
                    &target_decl_params,
                    &target_decl_ret,
                    callable_under,
                    fr_suspend,
                )
            }
            None => mangle_reflection_once(reflection_base),
        };
        fr.reflection_name =
            (mangled_reflection_name != fr.fn_name).then_some(mangled_reflection_name);
        fr.call_name = mangled_call_name;
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
