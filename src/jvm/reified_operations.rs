//! JVM realization of reified operations at declarations and call sites.
//!
//! Common IR retains checked Kotlin operations, substitutions, and exact semantic type-parameter
//! identities. For a declaration, this module realizes those operations as kotlinc-compatible
//! marker instructions before generic erasure. For a call site, it converts the checked semantic
//! substitutions into the JVM classifier names consumed by the bytecode splicer.

use std::collections::{HashMap, HashSet};

use crate::ir::{ExprId, IrExpr, IrFile, IrTypeOp, IrTypeParameter};
use crate::types::{stored_value_ty, Ty, TypeName};

#[derive(Clone)]
struct ReifiedParameter {
    source_name: String,
    erased: TypeName,
}

fn erased_classifier(ty: Ty) -> Option<TypeName> {
    let classifier = ty.non_null().obj_internal()?;
    Some(super::jvm_class_map::to_jvm_type_name(classifier))
}

fn reified_parameters(parameters: &[IrTypeParameter]) -> HashMap<String, ReifiedParameter> {
    let erasures = super::generic_erasure::parameter_erasures(parameters);
    parameters
        .iter()
        .filter(|parameter| parameter.reified)
        .filter_map(|parameter| {
            let erased = erased_classifier(*erasures.get(&parameter.semantic_name)?)?;
            Some((
                parameter.semantic_name.clone(),
                ReifiedParameter {
                    source_name: parameter.name.clone(),
                    erased,
                },
            ))
        })
        .collect()
}

fn parameter(ty: Ty, parameters: &HashMap<String, ReifiedParameter>) -> Option<&ReifiedParameter> {
    let Ty::TyParam(identity, _) = ty.non_null() else {
        return None;
    };
    parameters.get(identity)
}

/// JVM class names for the checked reified substitutions attached to one call. Common IR retains
/// semantic [`Ty`] values; the conversion to physical classifier names belongs here at the backend
/// boundary. A `Unit` value uses its stored classifier form, just like every other value that a
/// reified type-bearing instruction materializes.
pub(super) fn splice_type_map(ir: &IrFile, expression: ExprId) -> HashMap<String, String> {
    ir.reified_call_subst
        .get(&expression)
        .map(|substitutions| {
            substitutions
                .iter()
                .filter_map(|(name, ty)| {
                    let internal = stored_value_ty(*ty).kotlin_class_internal()?.render();
                    let internal = super::jvm_class_map::to_jvm_internal(&internal);
                    Some((name.clone(), internal.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn realize_expression_dag(
    ir: &mut IrFile,
    root: ExprId,
    parameters: &HashMap<String, ReifiedParameter>,
) {
    let mut pending = vec![root];
    let mut visited = HashSet::new();
    while let Some(expression) = pending.pop() {
        if !visited.insert(expression) {
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        let replacement = match ir.expr(expression).clone() {
            IrExpr::KClassLiteral {
                classifier: Some(classifier),
                value: None,
            } => parameter(classifier, parameters).map(|parameter| IrExpr::ReifiedClassMarker {
                name: parameter.source_name.clone(),
                erased: parameter.erased,
                kclass: true,
            }),
            IrExpr::TypeOp {
                op,
                arg,
                type_operand,
            } => parameter(type_operand, parameters).and_then(|parameter| {
                let (cast, negated) = match op {
                    IrTypeOp::InstanceOf => (false, false),
                    IrTypeOp::NotInstanceOf => (false, true),
                    IrTypeOp::Cast | IrTypeOp::CastNonNull => (true, false),
                    IrTypeOp::SafeCast | IrTypeOp::ImplicitCoercion => return None,
                };
                Some(IrExpr::ReifiedTypeOp {
                    cast,
                    negated,
                    arg,
                    name: parameter.source_name.clone(),
                    erased: parameter.erased,
                })
            }),
            _ => None,
        };
        if let Some(replacement) = replacement {
            ir.exprs[expression as usize] = replacement;
        }
    }
}

pub(super) fn realize(ir: &mut IrFile) {
    let functions = ir
        .signatures
        .iter()
        .filter_map(|(&function, signature)| {
            let parameters = reified_parameters(&signature.type_params);
            (!parameters.is_empty()).then_some((function, parameters))
        })
        .collect::<Vec<_>>();
    for (function, parameters) in functions {
        let Some(root) = ir
            .functions
            .get(function as usize)
            .and_then(|function| function.body)
        else {
            continue;
        };
        realize_expression_dag(ir, root, &parameters);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrFunction, IrGenericSig};

    fn reified_signature(identity: &str) -> IrGenericSig {
        IrGenericSig {
            type_params: vec![IrTypeParameter {
                name: "T".to_owned(),
                semantic_name: identity.to_owned(),
                bounds: vec![(Ty::obj("kotlin/Any"), false)],
                variance: Default::default(),
                reified: true,
            }],
            params: vec![],
            ret: None,
            supers: vec![],
        }
    }

    fn function(ir: &mut IrFile, body: ExprId, identity: &str) {
        ir.functions.push(IrFunction {
            name: "useT".to_owned(),
            params: vec![],
            ret: Ty::obj("kotlin/Any"),
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![],
        });
        ir.signatures.insert(0, reified_signature(identity));
    }

    #[test]
    fn splice_type_map_uses_the_stored_unit_classifier() {
        let expression = 7;
        let mut ir = IrFile::default();
        ir.reified_call_subst.insert(
            expression,
            vec![("T".to_owned(), Ty::Unit), ("R".to_owned(), Ty::String)],
        );

        assert_eq!(
            splice_type_map(&ir, expression),
            HashMap::from([
                ("T".to_owned(), "kotlin/Unit".to_owned()),
                ("R".to_owned(), "java/lang/String".to_owned()),
            ])
        );
    }

    #[test]
    fn realizes_a_reified_class_literal_with_jvm_erasure() {
        let identity = "T@useT";
        let mut ir = IrFile::default();
        let body = ir.add_expr(IrExpr::KClassLiteral {
            classifier: Some(Ty::ty_param(identity, Ty::obj("kotlin/Any"))),
            value: None,
        });
        function(&mut ir, body, identity);

        realize(&mut ir);

        assert!(matches!(
            ir.expr(body),
            IrExpr::ReifiedClassMarker {
                name,
                erased,
                kclass: true,
            } if name == "T" && erased.matches("java/lang/Object")
        ));
    }

    #[test]
    fn realizes_a_reified_instance_test_without_touching_its_operand() {
        let identity = "T@useT";
        let mut ir = IrFile::default();
        let operand = ir.add_expr(IrExpr::GetValue(0));
        let body = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::InstanceOf,
            arg: operand,
            type_operand: Ty::ty_param(identity, Ty::obj("kotlin/Any")),
        });
        function(&mut ir, body, identity);

        realize(&mut ir);

        assert!(matches!(
            ir.expr(body),
            IrExpr::ReifiedTypeOp {
                cast: false,
                negated: false,
                arg,
                name,
                erased,
            } if *arg == operand && name == "T" && erased.matches("java/lang/Object")
        ));
    }
}
