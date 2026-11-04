//! Every classifier identity one IR file's declarations and expressions name. Boundary passes use
//! this inventory to pull checked classifier facts into the IR for exactly the classifiers the file
//! references, so later phases never reopen a provider or read a fact off a name.

use super::{Callee, IrExpr, IrFile};
use crate::fx_hash::HashSet;
use crate::types::{Ty, TyKind, TypeName};

/// Push every classifier identity carried by `ty`, including nested generic/projection/function
/// positions. Source spellings never participate in this inventory.
pub(crate) fn collect_classifier_names(ty: Ty, out: &mut Vec<TypeName>) {
    match ty.kind() {
        TyKind::Obj(classifier, arguments) => {
            out.push(classifier);
            for argument in arguments {
                collect_classifier_names(*argument, out);
            }
        }
        TyKind::Nullable(inner)
        | TyKind::PlatformNullable(inner)
        | TyKind::MutabilityFlexible(inner) => collect_classifier_names(inner, out),
        TyKind::InProjection(inner)
        | TyKind::OutProjection(inner)
        | TyKind::StarProjection(inner) => collect_classifier_names(inner, out),
        TyKind::TyParam(_, bound) => collect_classifier_names(bound, out),
        TyKind::Fun(signature) => {
            for parameter in &signature.params {
                collect_classifier_names(*parameter, out);
            }
            collect_classifier_names(signature.ret, out);
        }
        _ => {}
    }
}

pub(crate) fn referenced_classifier_names(ir: &IrFile) -> Vec<TypeName> {
    let mut out = Vec::new();
    let mut seen = HashSet::default();
    for function in &ir.functions {
        for parameter in &function.params {
            collect_classifier_names(*parameter, &mut out);
        }
        collect_classifier_names(function.ret, &mut out);
    }
    for class in &ir.classes {
        for field in &class.fields {
            collect_classifier_names(field.ty, &mut out);
        }
        for supertype in &class.supertypes {
            collect_classifier_names(*supertype, &mut out);
        }
        for (_, bound) in &class.type_param_bounds {
            collect_classifier_names(*bound, &mut out);
        }
        for argument in &class.ctor_args {
            collect_classifier_names(argument.ty, &mut out);
        }
        for constructor in &class.secondary_ctors {
            constructor
                .prefix_params
                .iter()
                .chain(&constructor.params)
                .for_each(|ty| collect_classifier_names(*ty, &mut out));
        }
        for entry in &class.enum_entries {
            entry
                .constructor_parameter_types
                .iter()
                .for_each(|ty| collect_classifier_names(*ty, &mut out));
        }
        if let Some(reference) = &class.func_ref {
            reference
                .param_tys
                .iter()
                .chain(&reference.target_param_tys)
                .for_each(|ty| collect_classifier_names(*ty, &mut out));
            collect_classifier_names(reference.ret_ty, &mut out);
            collect_classifier_names(reference.target_ret_ty, &mut out);
            if let Some(parameters) = &reference.reflection_target_param_tys {
                parameters
                    .iter()
                    .for_each(|ty| collect_classifier_names(*ty, &mut out));
            }
            if let Some(result) = reference.reflection_target_ret_ty {
                collect_classifier_names(result, &mut out);
            }
        }
    }
    for ty in ir.logical_types.values() {
        collect_classifier_names(*ty, &mut out);
    }
    for expression in &ir.exprs {
        match expression {
            IrExpr::TypeOp { type_operand, .. } => {
                collect_classifier_names(*type_operand, &mut out)
            }
            IrExpr::Variable { ty, .. } => collect_classifier_names(*ty, &mut out),
            IrExpr::InvokeFunction { params, ret, .. } => {
                params
                    .iter()
                    .for_each(|ty| collect_classifier_names(*ty, &mut out));
                collect_classifier_names(*ret, &mut out);
            }
            IrExpr::PropertyRead { owner, ty, .. } | IrExpr::PropertyWrite { owner, ty, .. } => {
                out.push(*owner);
                collect_classifier_names(*ty, &mut out);
            }
            IrExpr::New {
                internal,
                ctor_params,
                ..
            } => {
                out.push(*internal);
                if let Some(parameters) = ctor_params {
                    parameters
                        .iter()
                        .for_each(|ty| collect_classifier_names(*ty, &mut out));
                }
            }
            IrExpr::RefNew { elem, .. }
            | IrExpr::RefGet { elem, .. }
            | IrExpr::RefSet { elem, .. } => collect_classifier_names(*elem, &mut out),
            IrExpr::Vararg { array_type, .. } | IrExpr::NewArray { array_type, .. } => {
                collect_classifier_names(*array_type, &mut out)
            }
            IrExpr::Call {
                callee: call_callee,
                ..
            } if let Callee::CrossFile { params, ret, .. }
            | Callee::ModuleWithDefaults { params, ret, .. } = &**call_callee =>
            {
                params
                    .iter()
                    .for_each(|ty| collect_classifier_names(*ty, &mut out));
                collect_classifier_names(*ret, &mut out);
            }
            IrExpr::Call {
                callee: call_callee,
                ..
            } if let Callee::Virtual {
                params: Some((parameters, ret)),
                ..
            } = &**call_callee =>
            {
                parameters
                    .iter()
                    .for_each(|ty| collect_classifier_names(*ty, &mut out));
                collect_classifier_names(*ret, &mut out);
            }
            _ => {}
        }
    }
    out.retain(|classifier| seen.insert(*classifier));
    out
}
