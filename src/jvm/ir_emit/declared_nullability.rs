//! kotlinc's synthesized `@NotNull`/`@Nullable` on a declared function's result and parameters.
//!
//! A reference result or parameter is annotated with its declared nullability. A value-class
//! member's static replacement leaves its carrier unannotated, and so does every position whose type
//! is a bare type parameter with a nullable bound. The value-class interface entry, which stands in
//! for that member on the box, carries the same annotations less the carrier.

use super::body_has_reified_markers;
use crate::ir::IrFile;
use crate::types::Ty;

/// The nullability annotation type descriptors of one function, in physical parameter order.
pub(super) struct DeclaredNullability {
    pub(super) result: Option<&'static str>,
    pub(super) parameters: Vec<Option<&'static str>>,
}

pub(super) fn declared_nullability(ir: &IrFile, fid: u32) -> DeclaredNullability {
    let f = &ir.functions[fid as usize];
    let ann_of = |physical: Ty, semantic: Ty| -> Option<&'static str> {
        let d = crate::jvm::names::type_descriptor(physical);
        if !(d.starts_with('L') || d.starts_with('[')) {
            return None;
        }
        Some(if matches!(semantic, Ty::Nullable(_)) {
            "Lorg/jetbrains/annotations/Nullable;"
        } else {
            "Lorg/jetbrains/annotations/NotNull;"
        })
    };
    // A bare type parameter with a nullable upper bound has no fixed nullability annotation. A
    // non-null bound (`T : Any`) is different: kotlinc publishes `@NotNull` on that occurrence.
    let gsig = ir.signatures.get(&fid);
    let member_sem = ir.member_semantic_sigs.get(&fid);
    let unannotated_type_parameter =
        |ty: Ty| matches!(ty, Ty::TyParam(_, bound) if bound.is_nullable());
    let value_class_declaration = ir.vc_declared_sigs.get(&fid);
    // A LAMBDA IMPL (`<fn>$lambda$N`) is a synthetic realization — kotlinc gives it debug tables
    // but NO nullability annotations.
    let lambda_impl = ir.lambda_own_params_from.contains_key(&fid);
    let reified_body = f
        .body
        .is_some_and(|body| body_has_reified_markers(ir, body));
    let ret_ann = (!lambda_impl
        && !reified_body
        && gsig.is_none_or(|g| !g.ret.is_some_and(unannotated_type_parameter))
        && member_sem.is_none_or(|(_, r)| !unannotated_type_parameter(*r)))
    .then(|| {
        let semantic = value_class_declaration
            .map(|(_, _, result)| *result)
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
            });
            let carrier_receiver = i == 0 && ir.jvm_value_class_receiver_impls.contains(&fid);
            if lambda_impl || reified_body || is_unannotated_tparam || carrier_receiver {
                None
            } else {
                let semantic = source_index
                    .and_then(|index| {
                        value_class_declaration.and_then(|(_, parameters, _)| parameters.get(index))
                    })
                    .copied()
                    .unwrap_or(*t);
                if source_index
                    .and_then(|index| declared_nullable.and_then(|values| values.get(index)))
                    .copied()
                    .unwrap_or(false)
                {
                    ann_of(*t, Ty::nullable(semantic))
                } else {
                    ann_of(*t, semantic)
                }
            }
        })
        .collect();
    DeclaredNullability {
        result: ret_ann,
        parameters: param_anns,
    }
}
