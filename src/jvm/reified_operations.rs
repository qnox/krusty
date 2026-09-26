//! JVM realization of reified operations at declarations and call sites.
//!
//! Common IR retains checked Kotlin operations, substitutions, and exact semantic type-parameter
//! identities. For a declaration, this module realizes those operations as kotlinc-compatible
//! marker instructions before generic erasure. For a call site, it converts each checked semantic
//! substitution into either the concrete JVM classifier or the host reified parameter consumed by
//! the bytecode splicer.

use std::collections::{HashMap, HashSet};

use super::reified_arguments::ReifiedArgument;
use crate::ir::TypeCheckRole;
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

/// The splice operands for the checked reified substitutions attached to one call. Common IR
/// retains semantic [`Ty`] values; the conversion to physical classifier names belongs here at the
/// backend boundary. A `Unit` value uses its stored classifier form, just like every other value
/// that a reified type-bearing instruction materializes. A substitution that is itself a reified
/// type parameter of a declaration in this file has no class yet: that declaration is a reified
/// inline body, so the callee's marker is forwarded under the declaration's own parameter name.
///
/// `render` spells a type as kotlinc does in `null cannot be cast to non-null type …`, which a
/// non-null reified `as` throws.
pub(super) fn splice_type_map(
    ir: &IrFile,
    expression: ExprId,
    render: &dyn Fn(Ty) -> String,
) -> HashMap<String, ReifiedArgument> {
    let Some(substitutions) = ir.reified_call_subst.get(&expression) else {
        return HashMap::new();
    };
    let forwarded = |ty: Ty| {
        let Ty::TyParam(identity, _) = ty.non_null() else {
            return None;
        };
        ir.signatures
            .values()
            .flat_map(|signature| &signature.type_params)
            .find(|parameter| parameter.reified && parameter.semantic_name == identity)
            .map(|parameter| ReifiedArgument::Forwarded {
                name: parameter.name.clone(),
                nullable: ty.is_nullable(),
            })
    };
    substitutions
        .iter()
        .filter_map(|(name, ty)| {
            let argument = forwarded(*ty).or_else(|| {
                Some(ReifiedArgument::Class {
                    internal: reified_class_internal(*ty)?,
                    nullable: ty.is_nullable(),
                    intrinsic: TypeCheckRole::of(*ty),
                    rendered: render(ty.non_null()),
                })
            })?;
            Some((name.clone(), argument))
        })
        .collect()
}

/// The JVM class a reified argument's type-bearing instruction names: a function type's
/// `FunctionN`, an array's descriptor, otherwise the stored classifier's mapped class.
fn reified_class_internal(ty: Ty) -> Option<String> {
    let value = ty.non_null();
    if let Ty::Fun(signature) = value {
        return (!signature.suspend)
            .then(|| super::names::function_interface_internal_name(signature.params.len()));
    }
    if value.is_array() {
        return Some(super::names::instanceof_internal_name(value));
    }
    let internal = stored_value_ty(ty).kotlin_class_internal()?.render();
    Some(super::jvm_class_map::to_jvm_internal(&internal).to_owned())
}

/// Everything a splice of the call `expression` needs to specialize its dependency's reified
/// markers: the JVM classes of its reified arguments, and for `typeOf` markers each argument's
/// realization (plain and nullable) built in this host file.
pub(super) fn splice_arguments(
    ir: &IrFile,
    expression: ExprId,
    facade: &str,
    render: &dyn Fn(Ty) -> String,
) -> super::reified_arguments::ReifiedArguments {
    let classes = splice_type_map(ir, expression, render);
    let mut type_of = HashMap::new();
    if let Some(substitutions) = ir.reified_call_subst.get(&expression) {
        let parameters = super::type_of::TypeParameters::new(ir, facade);
        for (name, ty) in substitutions {
            for (argument, ty) in [(name.clone(), *ty), (format!("{name}?"), Ty::nullable(*ty))] {
                let mut realization = Vec::new();
                if super::type_of::generate(ty, &parameters, &mut realization).is_ok() {
                    type_of.insert(argument, realization);
                }
            }
        }
    }
    super::reified_arguments::ReifiedArguments { classes, type_of }
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
            splice_type_map(&ir, expression, &|_| String::new()),
            HashMap::from([
                (
                    "T".to_owned(),
                    ReifiedArgument::Class {
                        internal: "kotlin/Unit".to_owned(),
                        nullable: false,
                        intrinsic: None,
                        rendered: String::new(),
                    }
                ),
                (
                    "R".to_owned(),
                    ReifiedArgument::Class {
                        internal: "java/lang/String".to_owned(),
                        nullable: false,
                        intrinsic: None,
                        rendered: String::new(),
                    }
                ),
            ])
        );
    }

    #[test]
    fn splice_type_map_forwards_a_reified_parameter_of_the_host() {
        let identity = "T@only";
        let expression = 7;
        let mut ir = IrFile::default();
        ir.signatures.insert(0, reified_signature(identity));
        let parameter = Ty::ty_param(identity, Ty::obj("kotlin/Any"));
        ir.reified_call_subst.insert(
            expression,
            vec![
                ("R".to_owned(), parameter),
                ("S".to_owned(), Ty::nullable(parameter)),
            ],
        );

        assert_eq!(
            splice_type_map(&ir, expression, &|_| String::new()),
            HashMap::from([
                (
                    "R".to_owned(),
                    ReifiedArgument::Forwarded {
                        name: "T".to_owned(),
                        nullable: false,
                    },
                ),
                (
                    "S".to_owned(),
                    ReifiedArgument::Forwarded {
                        name: "T".to_owned(),
                        nullable: true,
                    },
                ),
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
