//! kotlinc's synthesized `@NotNull`/`@Nullable` on a declared function's result and parameters.
//!
//! A reference result or parameter is annotated with its declared nullability, classified by
//! `declared_type_nullability`. A value-class member's static replacement leaves its carrier
//! unannotated, and so does every position whose type is a bare type parameter with a nullable bound.
//! A value class's `constructor-impl` declares the class's own constructor parameters and returns the
//! class type, so its result is always `@NotNull` whatever the carrier. The value-class interface entry, which stands in
//! for that member on the box, carries the same annotations less the carrier. A suspend
//! function's result is its CPS `Object`, always `@Nullable`.

use super::body_has_reified_markers;
use crate::ir::IrFile;
use crate::jvm::declared_type_nullability::{declared_annotation, is_unannotated_type_parameter};
use crate::types::Ty;

/// The nullability annotation type descriptors of one function, in physical parameter order.
pub(super) struct DeclaredNullability {
    pub(super) result: Option<&'static str>,
    pub(super) parameters: Vec<Option<&'static str>>,
}

pub(super) fn declared_nullability(ir: &IrFile, fid: u32) -> DeclaredNullability {
    let f = &ir.functions[fid as usize];
    let ann_of = |physical: Ty, semantic: Ty| declared_annotation(semantic, physical);
    // A bare type parameter whose upper-bound chain admits null has no fixed nullability
    // annotation. A non-null bound (`T : Any`) or `T & Any` is different: kotlinc publishes
    // `@NotNull` on that occurrence.
    let gsig = ir.signatures.get(&fid);
    let member_sem = ir.member_semantic_sigs.get(&fid);
    let unannotated_type_parameter = is_unannotated_type_parameter;
    let value_class_declaration = ir.vc_declared_sigs.get(&fid);
    // A synthesized value-class `constructor-impl` has already been rewritten to its carrier in
    // `f.params`, but its recorded representation signature still owns the source parameters. A
    // non-null `N` whose carrier is `String?` is therefore `@NotNull`, not `@Nullable`, at the
    // constructor boundary. Physical carrier nullability must not replace that semantic fact.
    let value_class_constructor = ir
        .jvm_value_class_constructor_impls
        .contains_key(&fid)
        .then(|| ir.jvm_value_class_member_signatures.get(&fid))
        .flatten();
    let value_class_parameters = value_class_declaration
        .map(|(_, parameters, _)| parameters.as_slice())
        .or_else(|| value_class_constructor.map(|signature| signature.params.as_slice()));
    // A LAMBDA IMPL (`<fn>$lambda$N`) is a synthetic realization — kotlinc gives it debug tables
    // but NO nullability annotations.
    let lambda_impl = ir.lambda_own_params_from.contains_key(&fid);
    let reified_body = f
        .body
        .is_some_and(|body| body_has_reified_markers(ir, body));
    // A suspend function returns its result or `COROUTINE_SUSPENDED` through the CPS `Object`,
    // which kotlinc annotates `@Nullable` whatever the declared result.
    let suspend = ir.suspend_funs.contains(&fid);
    let ret_ann = (!lambda_impl
        && !reified_body
        && (suspend
            || (gsig.is_none_or(|g| !g.ret.is_some_and(unannotated_type_parameter))
                && member_sem.is_none_or(|(_, r)| !unannotated_type_parameter(*r)))))
    .then(|| {
        if suspend {
            return ann_of(f.ret, Ty::nullable(f.ret));
        }
        // `constructor-impl` returns the value class itself, never null whatever its carrier.
        let semantic = value_class_declaration
            .map(|(_, _, result)| *result)
            .or_else(|| value_class_constructor.and_then(|signature| signature.ret))
            .unwrap_or(f.ret);
        ann_of(f.ret, semantic)
    })
    .flatten();
    // A parameter's declared `?` lives in a side-table (not in `f.params`, which stays non-null for the
    // mangle); consult it so a nullable reference parameter is annotated `@Nullable`, not `@NotNull`.
    let declared_nullable = ir.fn_param_declared_nullable.get(&fid);
    // A value-class member's static `-impl` has one backend-generated carrier before its source
    // parameters. Declaration-side facts never acquire that physical prefix.
    let value_class_receiver_prefix = usize::from(ir.jvm_value_class_receiver_impls.contains(&fid));
    let param_anns: Vec<Option<&'static str>> = f
        .params
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let source_index = i.checked_sub(value_class_receiver_prefix);
            let is_unannotated_tparam = source_index.is_some_and(|index| {
                gsig.and_then(|g| g.params.get(index))
                    .copied()
                    .is_some_and(unannotated_type_parameter)
                    || member_sem
                        .and_then(|(ps, _)| ps.get(index))
                        .copied()
                        .is_some_and(unannotated_type_parameter)
                    || value_class_parameters
                        .and_then(|parameters| parameters.get(index))
                        .copied()
                        .is_some_and(unannotated_type_parameter)
            });
            let carrier_receiver = i == 0 && ir.jvm_value_class_receiver_impls.contains(&fid);
            if lambda_impl || reified_body || is_unannotated_tparam || carrier_receiver {
                None
            } else {
                let semantic = source_index
                    .and_then(|index| {
                        value_class_parameters.and_then(|parameters| parameters.get(index))
                    })
                    .copied()
                    .unwrap_or(*t);
                let nullable = if value_class_parameters.is_some() {
                    semantic.is_nullable()
                } else {
                    source_index
                        .and_then(|index| declared_nullable.and_then(|values| values.get(index)))
                        .copied()
                        .unwrap_or(false)
                };
                // A parameter is a value slot: a `Unit` one is the `kotlin.Unit` reference.
                let physical = crate::jvm::method_descriptors::jvm_value_ty(t);
                if nullable {
                    ann_of(physical, Ty::nullable(semantic))
                } else {
                    ann_of(physical, semantic)
                }
            }
        })
        .collect();
    DeclaredNullability {
        result: ret_ann,
        parameters: param_anns,
    }
}
