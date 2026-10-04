//! JVM realization of reified operations at declarations and call sites.
//!
//! Common IR retains checked Kotlin operations, substitutions, and exact semantic type-parameter
//! identities. For a declaration, this module realizes those operations as kotlinc-compatible
//! marker instructions before generic erasure. For a call site, it converts each checked semantic
//! substitution into either the concrete JVM classifier or the host reified parameter consumed by
//! the bytecode splicer.

use std::collections::{HashMap, HashSet};

use super::reified_arguments::ReifiedArgument;
use crate::ir::{ExprId, FunId, IrExpr, IrFile, IrTypeOp, IrTypeParameter};
use crate::types::{stored_value_ty, Ty, TypeName};

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReifiedParameter {
    source_name: String,
    erased: TypeName,
}

fn erased_classifier(ty: Ty) -> Option<TypeName> {
    if let Some(function) = jvm_function_erasure(ty) {
        return Some(crate::types::type_name(&function));
    }
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
                    intrinsic: ir.type_check_role(*ty),
                    rendered: render(ty.non_null()),
                })
            })?;
            Some((name.clone(), argument))
        })
        .collect()
}

/// The JVM class a reified argument's type-bearing instruction names: a function type's
/// `FunctionN` (`Function{N+1}` when it suspends), an array's descriptor, otherwise the stored
/// classifier's mapped class.
fn reified_class_internal(ty: Ty) -> Option<String> {
    if let Some(function) = jvm_function_erasure(ty) {
        return Some(function);
    }
    let value = ty.non_null();
    if value.is_array() {
        return Some(super::names::instanceof_internal_name(value));
    }
    let internal = stored_value_ty(ty).kotlin_class_internal()?.render();
    Some(super::jvm_class_map::to_jvm_internal(&internal).to_owned())
}

/// JVM function interface a function type erases to. A suspend function counts its continuation,
/// so `suspend () -> Unit` and `SuspendFunction0` are both `Function1`.
fn jvm_function_erasure(ty: Ty) -> Option<String> {
    let value = ty.non_null();
    let (arity, suspend) = if let Ty::Fun(signature) = value {
        (signature.params.len(), signature.suspend)
    } else {
        let function = super::function_classifiers::classifier(value.obj_internal()?)?;
        if !function.is_suspend() || function.is_reflective() {
            return None;
        }
        (function.arity(), true)
    };
    Some(super::names::function_interface_internal_name(
        arity + usize::from(suspend),
    ))
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

fn collect_reified_parameters(ir: &IrFile) -> HashMap<String, ReifiedParameter> {
    let mut parameters = HashMap::new();
    for signature in ir.signatures.values() {
        for (identity, parameter) in reified_parameters(&signature.type_params) {
            if let Some(previous) = parameters.insert(identity, parameter.clone()) {
                assert_eq!(
                    previous, parameter,
                    "one semantic reified parameter has consistent declaration facts"
                );
            }
        }
    }
    parameters
}

/// Declarations whose bodies may execute a reification marker: a signature that owns a reified
/// parameter, and the exact lambda implementations `realize` already normalizes. An ordinary
/// anonymous or local object member is absent even when its erased body mentions an enclosing
/// reified identity.
fn reified_marker_functions(
    ir: &IrFile,
    lambdas: &super::lambda_classes::LambdaMethods,
) -> HashSet<FunId> {
    let mut functions = lambdas.functions().collect::<HashSet<_>>();
    for (&function, signature) in &ir.signatures {
        if signature
            .type_params
            .iter()
            .any(|parameter| parameter.reified)
        {
            functions.insert(function);
        }
    }
    functions
}

/// Body and declaration-owned default expressions of `functions`. A default is not a child of the
/// body, so a catch written there is its own root.
fn marker_roots(ir: &IrFile, functions: &HashSet<FunId>) -> Vec<ExprId> {
    let mut roots = Vec::new();
    for &function in functions {
        if let Some(body) = ir
            .functions
            .get(function as usize)
            .and_then(|function| function.body)
        {
            roots.push(body);
        }
        let Some(defaults) = ir
            .fn_params
            .get(&function)
            .and_then(|info| info.defaults.as_ref())
        else {
            continue;
        };
        roots.extend(defaults.iter().copied().flatten());
    }
    roots
}

/// Record which catches are still a reified parameter of a declaration in `functions`.
///
/// The plan is the declaration's source spelling, keyed by the `try` expression and parallel to
/// its catches. Later passes may move a catch onto a fresh `try`, so emission rebuilds the plan
/// from the same declaration facts. A catch whose type is already a class records nothing.
fn record_reified_catches(
    ir: &mut IrFile,
    parameters: &HashMap<String, ReifiedParameter>,
    functions: &HashSet<FunId>,
) {
    ir.reified_catch_markers.clear();
    let mut visited = HashSet::new();
    for root in marker_roots(ir, functions) {
        let mut pending = vec![root];
        while let Some(expression) = pending.pop() {
            if !visited.insert(expression) {
                continue;
            }
            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
            let IrExpr::Try { catches, .. } = ir.expr(expression) else {
                continue;
            };
            let markers = catches
                .iter()
                .map(|catch| parameter(catch.ty, parameters).map(|found| found.source_name.clone()))
                .collect::<Vec<_>>();
            if markers.iter().any(Option::is_some) {
                ir.reified_catch_markers.insert(expression, markers);
            }
        }
    }
}

/// Rebuild the reified-catch plan from declaration parameters. Emission calls this after suspend
/// lowering, which can move a catch onto a new `try`.
pub(super) fn record_catch_markers(
    ir: &mut IrFile,
    lambdas: &super::lambda_classes::LambdaMethods,
) {
    let parameters = collect_reified_parameters(ir);
    let functions = reified_marker_functions(ir, lambdas);
    record_reified_catches(ir, &parameters, &functions);
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
                ..
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

pub(super) fn realize(ir: &mut IrFile, lambdas: &super::lambda_classes::LambdaMethods) {
    // Type operands already carry declaration-qualified semantic parameter identities. Realizing a
    // closure can replace its Lambda node with a class value, but cannot change those identities.
    // Normalize the exact declaration/closure method domain from that checked map rather than
    // rediscovering implementations through expression nodes already consumed by representation.
    // An ordinary object's member can use the same parameter through generic erasure; that is not
    // a declaration or specialized lambda body and must not execute a reification marker.
    let parameters = collect_reified_parameters(ir);
    let functions = reified_marker_functions(ir, lambdas);
    let roots = marker_roots(ir, &functions);
    for root in roots {
        realize_expression_dag(ir, root, &parameters);
    }
    record_reified_catches(ir, &parameters, &functions);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrCatch, IrFunction, IrGenericSig};

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
            type_argument: true,
        });
        function(&mut ir, body, identity);

        realize(&mut ir, &Default::default());

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

        realize(&mut ir, &Default::default());

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

    #[test]
    fn realized_methods_keep_checked_reification_without_lambda_nodes() {
        let identity = "T@declaration";
        let mut ir = IrFile::default();
        let declaration = ir.add_expr(IrExpr::UnitInstance);
        function(&mut ir, declaration, identity);
        let argument = ir.add_expr(IrExpr::GetValue(0));
        let source = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: argument,
            type_operand: Ty::ty_param(identity, Ty::obj("kotlin/Any")),
        });
        let concrete_copy = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: argument,
            type_operand: Ty::String,
        });
        let ordinary = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: argument,
            type_operand: Ty::ty_param("T@ordinary", Ty::obj("kotlin/Any")),
        });
        let erased_object_member = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: argument,
            type_operand: Ty::ty_param(identity, Ty::obj("kotlin/Any")),
        });
        for body in [source, concrete_copy, ordinary, erased_object_member] {
            let mut implementation = ir.functions[0].clone();
            implementation.name = "invoke".to_owned();
            implementation.body = Some(body);
            ir.functions.push(implementation);
        }

        let lambdas = super::super::lambda_classes::LambdaMethods::for_test([1, 2, 3]);
        realize(&mut ir, &lambdas);

        assert!(matches!(
            ir.expr(source),
            IrExpr::ReifiedTypeOp { cast: true, name, .. } if name == "T"
        ));
        assert!(matches!(
            ir.expr(concrete_copy),
            IrExpr::TypeOp {
                type_operand: Ty::String,
                ..
            }
        ));
        assert!(matches!(ir.expr(ordinary), IrExpr::TypeOp { .. }));
        assert!(matches!(
            ir.expr(erased_object_member),
            IrExpr::TypeOp { .. }
        ));
    }

    #[test]
    fn records_a_reified_catch_from_the_declaration_source_name() {
        let identity = "E@eval";
        let mut ir = IrFile::default();
        let thrown = ir.add_expr(IrExpr::UnitInstance);
        let reified_handler = ir.add_expr(IrExpr::UnitInstance);
        let ordinary_handler = ir.add_expr(IrExpr::UnitInstance);
        let reified = ir.add_expr(IrExpr::Try {
            body: thrown,
            catches: vec![
                IrCatch {
                    var: 1,
                    binding: None,
                    ty: Ty::ty_param(identity, Ty::obj("kotlin/Throwable")),
                    body: reified_handler,
                    line: Some(4),
                },
                IrCatch {
                    var: 2,
                    binding: None,
                    ty: Ty::obj("kotlin/Throwable"),
                    body: ordinary_handler,
                    line: None,
                },
            ],
            finally: None,
            result: Ty::Unit,
        });
        let plain_handler = ir.add_expr(IrExpr::UnitInstance);
        let plain = ir.add_expr(IrExpr::Try {
            body: thrown,
            catches: vec![IrCatch {
                var: 3,
                binding: None,
                ty: Ty::obj("kotlin/Throwable"),
                body: plain_handler,
                line: None,
            }],
            finally: None,
            result: Ty::Unit,
        });
        ir.functions.push(IrFunction {
            name: "eval".to_owned(),
            params: vec![],
            ret: Ty::Unit,
            body: Some(reified),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![],
        });
        ir.functions.push(IrFunction {
            name: "other".to_owned(),
            params: vec![],
            ret: Ty::Unit,
            body: Some(plain),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![],
        });
        ir.signatures.insert(
            0,
            IrGenericSig {
                type_params: vec![IrTypeParameter {
                    name: "E".to_owned(),
                    semantic_name: identity.to_owned(),
                    bounds: vec![(Ty::obj("kotlin/Throwable"), false)],
                    variance: Default::default(),
                    reified: true,
                }],
                params: vec![],
                ret: None,
                supers: vec![],
            },
        );

        realize(&mut ir, &Default::default());

        assert_eq!(
            ir.reified_catch_markers.get(&reified),
            Some(&vec![Some("E".to_owned()), None])
        );
        assert!(!ir.reified_catch_markers.contains_key(&plain));
    }

    fn reified_catch(ir: &mut IrFile, identity: &str) -> ExprId {
        let thrown = ir.add_expr(IrExpr::UnitInstance);
        let handler = ir.add_expr(IrExpr::UnitInstance);
        ir.add_expr(IrExpr::Try {
            body: thrown,
            catches: vec![IrCatch {
                var: 1,
                binding: None,
                ty: Ty::ty_param(identity, Ty::obj("kotlin/Throwable")),
                body: handler,
                line: None,
            }],
            finally: None,
            result: Ty::Unit,
        })
    }

    #[test]
    fn an_erased_object_member_catch_is_not_marked() {
        let identity = "E@eval";
        let mut ir = IrFile::default();
        let declaration = reified_catch(&mut ir, identity);
        let lambda = reified_catch(&mut ir, identity);
        let erased_object_member = reified_catch(&mut ir, identity);
        for body in [declaration, lambda, erased_object_member] {
            ir.functions.push(IrFunction {
                name: "eval".to_owned(),
                params: vec![],
                ret: Ty::Unit,
                body: Some(body),
                is_static: true,
                dispatch_receiver: None,
                param_checks: vec![],
            });
        }
        ir.signatures.insert(
            0,
            IrGenericSig {
                type_params: vec![IrTypeParameter {
                    name: "E".to_owned(),
                    semantic_name: identity.to_owned(),
                    bounds: vec![(Ty::obj("kotlin/Throwable"), false)],
                    variance: Default::default(),
                    reified: true,
                }],
                params: vec![],
                ret: None,
                supers: vec![],
            },
        );

        let lambdas = super::super::lambda_classes::LambdaMethods::for_test([1]);
        realize(&mut ir, &lambdas);

        assert!(ir.reified_catch_markers.contains_key(&declaration));
        assert!(ir.reified_catch_markers.contains_key(&lambda));
        assert!(!ir.reified_catch_markers.contains_key(&erased_object_member));
    }

    #[test]
    fn a_default_expression_realizes_operations_and_catches_only_for_the_reified_declaration() {
        let identity = "E@eval";
        let mut ir = IrFile::default();
        let body = ir.add_expr(IrExpr::UnitInstance);
        let operand = ir.add_expr(IrExpr::GetValue(0));
        let type_operation = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: operand,
            type_operand: Ty::ty_param(identity, Ty::obj("kotlin/Throwable")),
        });
        let class_literal = ir.add_expr(IrExpr::KClassLiteral {
            classifier: Some(Ty::ty_param(identity, Ty::obj("kotlin/Throwable"))),
            value: None,
            type_argument: true,
        });
        let declaration_default_catch = reified_catch(&mut ir, identity);
        let declaration_default = ir.add_expr(IrExpr::Block {
            stmts: vec![type_operation, class_literal],
            value: Some(declaration_default_catch),
        });
        let other_default = reified_catch(&mut ir, identity);
        ir.functions.push(IrFunction {
            name: "eval".to_owned(),
            params: vec![Ty::Unit],
            ret: Ty::Unit,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![],
        });
        ir.functions.push(IrFunction {
            name: "other".to_owned(),
            params: vec![Ty::Unit],
            ret: Ty::Unit,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![],
        });
        ir.signatures.insert(
            0,
            IrGenericSig {
                type_params: vec![IrTypeParameter {
                    name: "E".to_owned(),
                    semantic_name: identity.to_owned(),
                    bounds: vec![(Ty::obj("kotlin/Throwable"), false)],
                    variance: Default::default(),
                    reified: true,
                }],
                params: vec![],
                ret: None,
                supers: vec![],
            },
        );
        ir.fn_params.insert(
            0,
            crate::ir::FnParamInfo::source_defaults(
                vec!["block".to_owned()],
                vec![Some(declaration_default)],
            ),
        );
        ir.fn_params.insert(
            1,
            crate::ir::FnParamInfo::source_defaults(
                vec!["block".to_owned()],
                vec![Some(other_default)],
            ),
        );

        realize(&mut ir, &Default::default());

        assert!(matches!(
            ir.expr(type_operation),
            IrExpr::ReifiedTypeOp { cast: true, name, .. } if name == "E"
        ));
        assert!(matches!(
            ir.expr(class_literal),
            IrExpr::ReifiedClassMarker {
                name,
                kclass: true,
                ..
            } if name == "E"
        ));
        assert_eq!(
            ir.reified_catch_markers.get(&declaration_default_catch),
            Some(&vec![Some("E".to_owned())])
        );
        assert!(!ir.reified_catch_markers.contains_key(&other_default));
    }
}
