//! The classifiers one file's IR references.

use crate::types::{Ty, TypeName};
use std::collections::HashSet;

use super::{Callee, IrExpr, IrFile};

/// Push every classifier identity carried by `ty`, including nested generic/projection/function
/// positions. Source spellings never participate in this inventory.
pub fn collect_classifiers(ty: Ty, out: &mut Vec<TypeName>) {
    match ty {
        Ty::Obj(classifier, arguments) => {
            out.push(classifier);
            for argument in arguments {
                collect_classifiers(*argument, out);
            }
        }
        Ty::Nullable(inner) | Ty::PlatformNullable(inner) => collect_classifiers(*inner, out),
        Ty::InProjection(inner) | Ty::OutProjection(inner) | Ty::StarProjection(inner) => {
            collect_classifiers(*inner, out)
        }
        Ty::TyParam(_, bound) => collect_classifiers(*bound, out),
        Ty::Fun(signature) => {
            for parameter in &signature.params {
                collect_classifiers(*parameter, out);
            }
            collect_classifiers(signature.ret, out);
        }
        _ => {}
    }
}

impl IrFile {
    /// Every classifier identity this file's IR names, in first-mention order and without repeats:
    /// declarations' signatures, fields, supertypes and bounds, constructor and reference shapes,
    /// the logical types, and the types expressions carry. The one walk each backend's value-class
    /// inventory starts from, so two targets cannot disagree about which classifiers a file uses.
    pub fn referenced_classifiers(&self) -> Vec<TypeName> {
        let ir = self;
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for function in &ir.functions {
            for parameter in &function.params {
                collect_classifiers(*parameter, &mut out);
            }
            collect_classifiers(function.ret, &mut out);
        }
        for class in &ir.classes {
            for field in &class.fields {
                collect_classifiers(field.ty, &mut out);
            }
            for supertype in &class.supertypes {
                collect_classifiers(*supertype, &mut out);
            }
            for (_, bound) in &class.type_param_bounds {
                collect_classifiers(*bound, &mut out);
            }
            for argument in &class.ctor_args {
                collect_classifiers(argument.ty, &mut out);
            }
            for constructor in &class.secondary_ctors {
                constructor
                    .prefix_params
                    .iter()
                    .chain(&constructor.params)
                    .for_each(|ty| collect_classifiers(*ty, &mut out));
            }
            for entry in &class.enum_entries {
                entry
                    .constructor_parameter_types
                    .iter()
                    .for_each(|ty| collect_classifiers(*ty, &mut out));
            }
            if let Some(parameters) = &class.enum_entry_of {
                parameters
                    .iter()
                    .for_each(|ty| collect_classifiers(*ty, &mut out));
            }
            if let Some(reference) = &class.func_ref {
                reference
                    .param_tys
                    .iter()
                    .chain(&reference.target_param_tys)
                    .for_each(|ty| collect_classifiers(*ty, &mut out));
                collect_classifiers(reference.ret_ty, &mut out);
                collect_classifiers(reference.target_ret_ty, &mut out);
                if let Some(parameters) = &reference.reflection_target_param_tys {
                    parameters
                        .iter()
                        .for_each(|ty| collect_classifiers(*ty, &mut out));
                }
                if let Some(result) = reference.reflection_target_ret_ty {
                    collect_classifiers(result, &mut out);
                }
            }
        }
        for ty in ir.logical_types.values() {
            collect_classifiers(*ty, &mut out);
        }
        for expression in &ir.exprs {
            match expression {
                IrExpr::TypeOp { type_operand, .. } => collect_classifiers(*type_operand, &mut out),
                IrExpr::Variable { ty, .. } => collect_classifiers(*ty, &mut out),
                IrExpr::InvokeFunction { params, ret, .. } => {
                    params
                        .iter()
                        .for_each(|ty| collect_classifiers(*ty, &mut out));
                    collect_classifiers(*ret, &mut out);
                }
                IrExpr::PropertyRead { owner, ty, .. }
                | IrExpr::PropertyWrite { owner, ty, .. } => {
                    out.push(*owner);
                    collect_classifiers(*ty, &mut out);
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
                            .for_each(|ty| collect_classifiers(*ty, &mut out));
                    }
                }
                IrExpr::RefNew { elem, .. }
                | IrExpr::RefGet { elem, .. }
                | IrExpr::RefSet { elem, .. } => collect_classifiers(*elem, &mut out),
                IrExpr::Vararg { array_type, .. } | IrExpr::NewArray { array_type, .. } => {
                    collect_classifiers(*array_type, &mut out)
                }
                IrExpr::Call {
                    callee:
                        Callee::CrossFile { params, ret, .. }
                        | Callee::ModuleWithDefaults { params, ret, .. },
                    ..
                } => {
                    params
                        .iter()
                        .for_each(|ty| collect_classifiers(*ty, &mut out));
                    collect_classifiers(*ret, &mut out);
                }
                IrExpr::Call {
                    callee:
                        Callee::Virtual {
                            params: Some((parameters, ret)),
                            ..
                        },
                    ..
                } => {
                    parameters
                        .iter()
                        .for_each(|ty| collect_classifiers(*ty, &mut out));
                    collect_classifiers(*ret, &mut out);
                }
                _ => {}
            }
        }
        out.retain(|classifier| seen.insert(*classifier));
        out
    }
}
