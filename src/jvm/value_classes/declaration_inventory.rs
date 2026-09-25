//! Inventory of exact value-class declarations referenced by one JVM IR file.
//!
//! This boundary combines normalized checked classifier facts with common-IR facts, validates that
//! duplicate publications agree, and follows declared underlying types transitively. The pass-local
//! JVM erasure map converts only semantic scalar spellings; boxing, storage, and descriptors remain
//! owned by their later representation operations.

use super::{is_native_unsigned, Under};
use crate::ir::{Callee, IrExpr, IrFile};
use crate::types::{Ty, TypeName};
use std::collections::{HashMap, HashSet};

/// Push every classifier identity carried by `ty`, including nested generic/projection/function
/// positions. Source spellings never participate in this inventory.
pub(super) fn collect_classifier_names(ty: Ty, out: &mut Vec<TypeName>) {
    match ty {
        Ty::Obj(classifier, arguments) => {
            out.push(classifier);
            for argument in arguments {
                collect_classifier_names(*argument, out);
            }
        }
        Ty::Nullable(inner) | Ty::PlatformNullable(inner) => collect_classifier_names(*inner, out),
        Ty::InProjection(inner) | Ty::OutProjection(inner) | Ty::StarProjection(inner) => {
            collect_classifier_names(*inner, out)
        }
        Ty::TyParam(_, bound) => collect_classifier_names(*bound, out),
        Ty::Fun(signature) => {
            for parameter in &signature.params {
                collect_classifier_names(*parameter, out);
            }
            collect_classifier_names(signature.ret, out);
        }
        _ => {}
    }
}

fn referenced_classifier_names(ir: &IrFile) -> Vec<TypeName> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
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
        if let Some(parameters) = &class.enum_entry_of {
            parameters
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
                callee:
                    Callee::CrossFile { params, ret, .. }
                    | Callee::ModuleWithDefaults { params, ret, .. },
                ..
            } => {
                params
                    .iter()
                    .for_each(|ty| collect_classifier_names(*ty, &mut out));
                collect_classifier_names(*ret, &mut out);
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
                    .for_each(|ty| collect_classifier_names(*ty, &mut out));
                collect_classifier_names(*ret, &mut out);
            }
            _ => {}
        }
    }
    out.retain(|classifier| seen.insert(*classifier));
    out
}

pub(super) fn merge_referenced(
    ir: &mut IrFile,
    classifiers: &dyn crate::types::ClassifierFactSource,
    declarations: &mut Under,
) -> Option<HashMap<TypeName, String>> {
    let mut underlying_properties = HashMap::new();
    let mut pending = referenced_classifier_names(ir);
    let mut probed = HashSet::new();
    while let Some(classifier) = pending.pop() {
        if !probed.insert(classifier) || is_native_unsigned(classifier) {
            continue;
        }
        if let Some(underlying) = declarations.get(&classifier).copied() {
            collect_classifier_names(underlying, &mut pending);
            continue;
        }
        if crate::types::prim_array_element(classifier).is_some() {
            continue;
        }

        if let Some(property) = classifiers.classifier_value_property(classifier) {
            crate::trace_compiler!(
                "value_classes",
                "external value class {} underlying property {}",
                classifier,
                property
            );
            underlying_properties.insert(classifier, property);
        }

        let candidates = [
            ir.external_value_class_name(classifier).copied(),
            classifiers.classifier_value_underlying(classifier),
        ];
        let mut declared = None;
        for candidate in candidates.into_iter().flatten() {
            let candidate = candidate.canonical_semantic();
            if let Some(existing) = declared {
                assert_eq!(
                    existing, candidate,
                    "checked providers disagreed about one value-class declaration"
                );
            } else {
                declared = Some(candidate);
            }
        }
        let Some(underlying) = declared else {
            continue;
        };
        ir.insert_external_value_class_name(classifier, underlying);
        declarations.insert(
            classifier,
            underlying.scalar_value_repr().unwrap_or(underlying),
        );
        collect_classifier_names(underlying, &mut pending);
    }
    crate::value_classes::declarations_are_acyclic(declarations).then_some(underlying_properties)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cyclic_checked_facts_decline_instead_of_selecting_an_edge() {
        let first = crate::types::type_name("fixture/First");
        let second = crate::types::type_name("fixture/Second");
        let mut ir = IrFile::default();
        ir.insert_external_value_class_name(first, Ty::obj_name(second));
        ir.insert_external_value_class_name(second, Ty::obj_name(first));
        let mut holder = crate::plugins::synthetic_class("fixture/Holder");
        holder.fields.push(crate::ir::IrField::new(
            "value".to_string(),
            Ty::obj_name(first),
        ));
        ir.add_class(holder);
        let mut declarations = Under::new();

        assert_eq!(
            merge_referenced(
                &mut ir,
                &crate::libraries::EmptySymbolSource,
                &mut declarations,
            ),
            None
        );
    }
}
