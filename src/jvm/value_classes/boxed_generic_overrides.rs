//! Overrides of a generic supertype member that receive a value-class parameter boxed.

use super::*;

/// A member method OVERRIDING a generic supertype method receives its VALUE-CLASS param BOXED: the
/// supertype's erased signature passes `Object`, so the incoming arg is a boxed `X`, not the underlying.
/// The IR's bridge record carries the evidence — a concrete VC param (`Result`) whose supertype-erased
/// counterpart is a generic reference (`Any`), with NO mangled target unboxing it (a degenerate
/// `target_name = None` bridge; a mangled `foo-<hash>` target would unbox in the bridge instead). Mark
/// such a param slot as the BOXED value class so the body unboxes it at each value-class member call —
/// matching kotlinc, which unboxes the incoming box before use. (Only the repr analysis sees this; the
/// emitted method signature is unchanged.)
/// A GENERIC value class (`IC<T>`, its field typed by a type parameter) is left unmarked: its box
/// and unbox differ from a concrete-underlying one's, which krusty can't mark without a conflict.
pub(super) fn mark(
    ir: &mut IrFile,
    under: &Under,
    params: &mut [Vec<Ty>],
    rets: &[Ty],
    slot_types: &mut [HashMap<u32, Ty>],
    suspend_fids: &HashSet<u32>,
) {
    let generic_vcs: std::collections::HashSet<TypeName> = ir
        .classes
        .iter()
        .filter(|c| c.is_value && !c.type_params.is_empty())
        .map(|c| c.fq_name)
        .collect();
    let mut boxed_generic_overrides = HashSet::new();
    for c in &ir.classes {
        for b in &c.bridges {
            // A VALUE-CLASS-returning override is MANGLED with fully UNBOXED params — kotlinc keeps it
            // unboxed. Only a NON-value-class-returning override keeps the erased supertype name and receives
            // its value-class param BOXED. So skip a value-class return (and a mangled-target bridge).
            if b.target_name.is_some()
                || b.concrete_ret
                    .non_null()
                    .obj_internal()
                    .is_some_and(|fq| under.contains_key(&fq))
            {
                continue;
            }
            // The bridge's exact target. A bridge to an implementation this class does not declare
            // (an inherited or external one) has no body here whose slots receive the box.
            let Some(fid) = b.target_function.filter(|fid| c.methods.contains(fid)) else {
                continue;
            };
            let f = &ir.functions[fid as usize];
            // A method MANGLED by a value-class PARAMETER (not only a value-class return) is likewise
            // unboxed in its bridge — `call(Result, IC)` mangles to `call-<hash>` because of the user value
            // class `IC` (kotlinc EXEMPTS a `kotlin.Result` param from mangling), and its bridge unboxes
            // BOTH params. The `target_name`/return checks above miss this shape (non-value-class return,
            // `target_name = None`), so its params would be wrongly marked boxed and double-unboxed at use.
            // Skip when the method is mangled — same predicate the mangle pass below applies.
            let is_file_class = f.dispatch_receiver.is_none();
            if vc_mangle(
                &f.name,
                &params[fid as usize],
                &rets[fid as usize],
                under,
                is_file_class,
                suspend_fids.contains(&fid),
            ) != f.name
            {
                continue;
            }
            let base = u32::from(f.dispatch_receiver.is_some() && !f.is_static);
            for (i, (cp, ep)) in b
                .concrete_params
                .iter()
                .zip(b.erased_params.iter())
                .enumerate()
            {
                if let Some(x) = cp.non_null().obj_internal() {
                    // The supertype must pass a GENERIC `Any`/`Object` at this position — i.e. the param was a
                    // type PARAMETER there (`I<Result>.foo(T)`), so the arg is boxed. A value class that is
                    // CONCRETE in the supertype (`Core.getFor(id: Aid)`) erases to its OWN underlying
                    // (`String`), the method is mangled, and its param arrives UNBOXED — do NOT mark it.
                    let supertype_generic = ep
                        .non_null()
                        .obj_internal()
                        .is_some_and(crate::jvm::jvm_class_map::is_jvm_erased_top);
                    if under.contains_key(&x) && supertype_generic && !generic_vcs.contains(&x) {
                        // Mark BOXED in the body's slot repr AND the call-boundary target, so a
                        // CALLER boxes into this generic slot and the BODY unboxes it.
                        let boxed = Ty::nullable(Ty::obj_name(x));
                        slot_types[fid as usize].insert(base + i as u32, boxed);
                        boxed_generic_overrides.insert(fid);
                        if let Some(p) = params.get_mut(fid as usize).and_then(|v| v.get_mut(i)) {
                            *p = boxed;
                        }
                    }
                }
            }
        }
    }

    super::call_arguments::record_method_parameters(ir, &boxed_generic_overrides, params);
}
