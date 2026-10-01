//! Adapt a recorded declaration argument to its value-class carrier.
//!
//! Common IR records each supplied argument whose declaration parameter mentions a type parameter.
//! Generic erasure selects the class bound, and value-class lowering projects that bound to its
//! carrier. This pass walks those bounds by semantic identity and rewrites an argument only when
//! the class bound is an unsigned value class (`UInt` to `int`). A reference erasure keeps the
//! frontend operand, so a suspend lambda is not checkcast to `Function1`. Semantic
//! `call_declared_params` stay the Kotlin types; `physical_call_parameters` is the JVM plan.

use crate::ir::{Callee, ExprId, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

/// Declaration ordinals of the parameters that are not in `omitted`.
///
/// `omitted` must be strictly increasing and inside `0..parameter_count`. Duplicates fail that
/// order. An index past the signature fails on its own, including one that is not the last entry.
pub(super) fn argument_ordinals(
    parameter_count: usize,
    omitted: &[u32],
) -> Result<Vec<u32>, String> {
    if omitted.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(format!(
            "omitted parameters {omitted:?} are not strictly increasing"
        ));
    }
    if let Some(parameter) = omitted
        .iter()
        .copied()
        .find(|parameter| *parameter as usize >= parameter_count)
    {
        return Err(format!(
            "omitted parameter {parameter} is outside 0..{parameter_count}"
        ));
    }
    let omitted: std::collections::HashSet<u32> = omitted.iter().copied().collect();
    Ok((0..u32::try_from(parameter_count).unwrap_or(u32::MAX))
        .filter(|parameter| !omitted.contains(parameter))
        .collect())
}

/// `supplied` arguments occupy the physical parameters that are not in `omitted`.
pub(super) fn bind_dependency_arguments(
    parameter_count: usize,
    supplied: usize,
    omitted: &[u32],
) -> Result<(), String> {
    let ordinals = argument_ordinals(parameter_count, omitted)?;
    if ordinals.len() != supplied {
        return Err(format!(
            "call supplies {supplied} arguments for {parameter_count} physical parameters ({} omitted)",
            omitted.len()
        ));
    }
    Ok(())
}

/// Publish the source parameters of a selected dependency call.
///
/// `plan` is the provider's slot list. Source ordinals are the arguments; a dispatch receiver and
/// a suspend continuation are not. A missing plan is an error, not a source-parameter vector.
pub(super) fn publish_dependency_parameters(
    ir: &mut IrFile,
    call: ExprId,
    parameters: Vec<Ty>,
    plan: Option<&[crate::libraries::PhysicalParameterSlot]>,
    supplied: usize,
    omitted: &[u32],
) -> Result<Vec<Ty>, String> {
    let parameters =
        crate::libraries::physical_parameter_plan::source_physical_parameters(&parameters, plan)?;
    bind_dependency_arguments(parameters.len(), supplied, omitted)?;
    if ir.declaration_argument_boundaries.contains_key(&call) {
        ir.physical_call_parameters
            .insert(call, parameters.clone().into_boxed_slice());
    }
    Ok(parameters)
}

/// Rewrite each unsigned class-bound argument to the physical parameter of its declaration ordinal.
pub(super) fn retarget_declaration_arguments(ir: &mut IrFile) -> Result<(), String> {
    let calls = (0..ir.exprs.len())
        .map(|index| u32::try_from(index).expect("too many common IR expressions"))
        .collect::<Vec<_>>();
    for call in calls {
        let Some(boundaries) = ir.declaration_argument_boundaries.remove(&call) else {
            continue;
        };
        let bounds = bound_parameters(ir, call);
        let mut unsigned = Vec::new();
        for boundary in boundaries.iter() {
            if class_bound_is_unsigned(boundary.declaration, &bounds, &mut Vec::new())? {
                unsigned.push(*boundary);
            }
        }
        if unsigned.is_empty() {
            continue;
        }
        let Some(parameters) = physical_parameters(ir, call)? else {
            return Err(format!(
                "call {call} has {} recorded declaration arguments but no physical signature",
                unsigned.len()
            ));
        };
        let arguments = call_arguments(ir, call);
        for boundary in unsigned {
            let argument = boundary.argument;
            if !arguments.contains(&argument) {
                return Err(format!(
                    "call {call} does not own recorded argument {} for declaration parameter {}",
                    boundary.argument, boundary.parameter
                ));
            }
            let physical = parameters.get(boundary.parameter as usize).copied().ok_or_else(|| {
                format!(
                    "call {call} argument {argument} parameter {} is outside the physical signature of {}",
                    boundary.parameter,
                    parameters.len()
                )
            })?;
            adapt_recorded_argument(ir, call, argument, boundary.retarget_coercion, physical)?;
        }
    }
    ir.physical_call_parameters.clear();
    Ok(())
}

enum BoundParameters<'a> {
    Declared(&'a [crate::ir::IrTypeParameter]),
    Referenced(&'a [crate::ir::IrCallableTypeParameter]),
}

fn bound_parameters(ir: &IrFile, call: ExprId) -> BoundParameters<'_> {
    let function = source_function(ir, call);
    if let Some(signature) = function.and_then(|function| ir.signatures.get(&function)) {
        return BoundParameters::Declared(&signature.type_params);
    }
    let target = match ir.expr(call) {
        IrExpr::Call { callee, .. } => match callee {
            Callee::Module { target, .. } | Callee::ModuleWithDefaults { target, .. } => {
                Some(*target)
            }
            Callee::CrossFile {
                module_target: Some(target),
                ..
            }
            | Callee::Virtual {
                module_target: Some(target),
                ..
            } => Some(*target),
            _ => None,
        },
        _ => None,
    };
    target
        .and_then(|target| ir.referenced_module_callables.get(&target))
        .map_or(BoundParameters::Declared(&[]), |callable| {
            BoundParameters::Referenced(&callable.type_parameters)
        })
}

/// Whether `ty`'s class bound is an unsigned value class.
///
/// Declaration bounds are followed by semantic type-parameter identity. A cycle is an invariant
/// error. An interface bound is not a class bound, so a type argument of that interface is not
/// this carrier.
fn class_bound_is_unsigned(
    ty: Ty,
    parameters: &BoundParameters<'_>,
    visiting: &mut Vec<&'static str>,
) -> Result<bool, String> {
    let ty = ty.non_null();
    if matches!(ty, Ty::Fun(_)) {
        return Ok(false);
    }
    if ty.is_unsigned() {
        return Ok(true);
    }
    match ty {
        Ty::DefinitelyNotNull(inner) => class_bound_is_unsigned(*inner, parameters, visiting),
        Ty::TyParam(name, bound) => {
            if visiting.contains(&name) {
                return Err(format!("type parameter {name} has a cyclic class bound"));
            }
            visiting.push(name);
            let declared = parameters.lookup(name)?;
            if let Some(bounds) = declared {
                for &(bound, is_interface) in bounds {
                    let bound = bound.non_null();
                    if bound.is_unsigned()
                        || ((matches!(bound, Ty::TyParam(..)) || !is_interface)
                            && class_bound_is_unsigned(bound, parameters, visiting)?)
                    {
                        visiting.pop();
                        return Ok(true);
                    }
                }
            }
            let result = class_bound_is_unsigned(*bound, parameters, visiting);
            visiting.pop();
            result
        }
        _ => Ok(false),
    }
}

impl BoundParameters<'_> {
    fn lookup(&self, name: &str) -> Result<Option<&[(Ty, bool)]>, String> {
        let mut found: Option<&[(Ty, bool)]> = None;
        match self {
            BoundParameters::Declared(parameters) => {
                for parameter in *parameters {
                    if parameter.semantic_name != name {
                        continue;
                    }
                    if found.is_some() {
                        return Err(format!(
                            "type parameter {name} is not a unique bound identity"
                        ));
                    }
                    found = Some(parameter.bounds.as_slice());
                }
            }
            BoundParameters::Referenced(parameters) => {
                for parameter in *parameters {
                    if parameter.semantic_name != name {
                        continue;
                    }
                    if found.is_some() {
                        return Err(format!(
                            "type parameter {name} is not a unique bound identity"
                        ));
                    }
                    found = Some(parameter.bounds.as_ref());
                }
            }
        }
        Ok(found)
    }
}

/// Primitive JVM slot of a non-null value-class carrier. A nullable bound stays boxed, and a
/// reference erasure (`Any`, a function type, a classifier) is not a carrier. A signed primitive
/// is already its JVM slot: adapting it would replace the boxing a `$default` stub does for
/// `T : Char`.
fn scalar_carrier(physical: Ty) -> Option<Ty> {
    if physical.is_nullable() {
        return None;
    }
    physical.scalar_value_repr()
}

/// Carrier inserted for an argument that is not already an unsigned boundary coercion. Only an
/// unsigned class bound projects to a different slot (`UInt` to `int`).
fn unsigned_projection_carrier(physical: Ty) -> Option<Ty> {
    if physical.is_nullable() {
        return None;
    }
    let root = physical.non_null();
    let unsigned = match root {
        Ty::TyParam(_, bound) => bound.non_null().is_unsigned(),
        other => other.is_unsigned(),
    };
    if !unsigned {
        return None;
    }
    let carrier = physical.scalar_value_repr()?;
    (carrier != root).then_some(carrier)
}

fn physical_parameters(ir: &IrFile, call: ExprId) -> Result<Option<Vec<Ty>>, String> {
    if let Some(parameters) = ir.physical_call_parameters.get(&call) {
        return Ok(Some(parameters.to_vec()));
    }
    let Some(function) = source_function(ir, call) else {
        return Ok(None);
    };
    ir.functions
        .get(function as usize)
        .map(|function| Some(function.params.clone()))
        .ok_or_else(|| format!("call {call} function {function} has no declaration"))
}

fn source_function(ir: &IrFile, call: ExprId) -> Option<u32> {
    match ir.expr(call) {
        IrExpr::Call { callee, .. } => match callee {
            Callee::Local(function)
            | Callee::LocalDefault(function)
            | Callee::LocalWithDefaults { function, .. }
            | Callee::ClassStatic { function, .. }
            | Callee::ClassStaticDefault { function, .. }
            | Callee::ClassStaticWithDefaults { function, .. } => Some(*function),
            _ => None,
        },
        IrExpr::MethodCall { class, index, .. } => ir
            .classes
            .get(*class as usize)
            .and_then(|class| class.methods.get(*index as usize))
            .copied(),
        _ => None,
    }
}

fn call_arguments(ir: &IrFile, call: ExprId) -> Vec<ExprId> {
    match ir.expr(call) {
        IrExpr::Call { args, .. } => args.clone(),
        IrExpr::MethodCall { args, .. } => args.iter().copied().flatten().collect(),
        _ => Vec::new(),
    }
}

fn adapt_recorded_argument(
    ir: &mut IrFile,
    call: ExprId,
    argument: ExprId,
    retarget_coercion: bool,
    physical: Ty,
) -> Result<(), String> {
    if let Some(carrier) = scalar_carrier(physical) {
        if retarget_coercion {
            retarget_boundary_coercion(ir, argument, carrier)?;
            return Ok(());
        }
    }
    let Some(carrier) = unsigned_projection_carrier(physical) else {
        return Ok(());
    };
    let coercion = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: argument,
        type_operand: carrier,
    });
    replace_call_argument(ir, call, argument, coercion)
}

/// Point the lowering-recorded declaration adapter at the selected physical slot.
fn retarget_boundary_coercion(
    ir: &mut IrFile,
    argument: ExprId,
    carrier: Ty,
) -> Result<(), String> {
    let IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        type_operand,
        ..
    } = &mut ir.exprs[argument as usize]
    else {
        return Err(format!(
            "recorded declaration adapter {argument} is not an implicit coercion"
        ));
    };
    *type_operand = carrier;
    Ok(())
}

fn replace_call_argument(
    ir: &mut IrFile,
    call: ExprId,
    from: ExprId,
    to: ExprId,
) -> Result<(), String> {
    let replaced = match &mut ir.exprs[call as usize] {
        IrExpr::Call { args, .. } => args.iter_mut().find(|argument| **argument == from),
        IrExpr::MethodCall { args, .. } => args
            .iter_mut()
            .flatten()
            .find(|argument| **argument == from),
        _ => None,
    };
    let Some(slot) = replaced else {
        return Err(format!(
            "call {call} does not pass recorded argument {from}"
        ));
    };
    *slot = to;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{argument_ordinals, retarget_declaration_arguments};
    use crate::ir::{Callee, IrConst, IrDeclarationArgumentBoundary, IrExpr, IrFile, IrTypeOp};
    use crate::libraries::physical_parameter_plan::source_physical_parameters;
    use crate::libraries::PhysicalParameterSlot;
    use crate::types::Ty;

    #[test]
    fn a_parameter_plan_keeps_source_slots_and_drops_abi_slots() {
        let continuation = Ty::obj("kotlin/coroutines/Continuation");
        let suspend = [Ty::Int, continuation];
        let plan = [
            PhysicalParameterSlot::Source(0),
            PhysicalParameterSlot::Continuation,
        ];
        assert_eq!(
            source_physical_parameters(&suspend, Some(&plan)).unwrap(),
            vec![Ty::Int]
        );
        assert_eq!(
            source_physical_parameters(&[Ty::Int], None),
            Err("a dependency callable has no physical parameter plan".to_string())
        );
        let source_only = [PhysicalParameterSlot::Source(0)];
        assert_eq!(
            source_physical_parameters(&[Ty::Int], Some(&source_only)).unwrap(),
            vec![Ty::Int]
        );
        let dispatch = [
            PhysicalParameterSlot::Dispatch,
            PhysicalParameterSlot::Source(0),
            PhysicalParameterSlot::Continuation,
        ];
        assert_eq!(
            source_physical_parameters(
                &[Ty::obj("kotlin/UInt"), Ty::Int, continuation],
                Some(&dispatch)
            )
            .unwrap(),
            vec![Ty::Int]
        );
        assert!(source_physical_parameters(&suspend, Some(&dispatch)).is_err());
    }

    #[test]
    fn omitted_slots_are_strictly_increasing_and_in_range() {
        assert_eq!(argument_ordinals(4, &[]).unwrap(), vec![0, 1, 2, 3]);
        assert_eq!(argument_ordinals(4, &[1, 3]).unwrap(), vec![0, 2]);
        assert!(argument_ordinals(4, &[1, 1]).is_err());
        assert!(argument_ordinals(4, &[2, 1]).is_err());
        assert!(argument_ordinals(3, &[3]).is_err());
        assert!(argument_ordinals(3, &[0, 4]).is_err());
    }

    #[test]
    fn a_scalar_carrier_adapts_the_recorded_argument() {
        let mut ir = recorded_call(IrExpr::Const(IrConst::Int(3)), Ty::UInt);
        retarget_declaration_arguments(&mut ir).expect("carrier adaptation");
        assert_eq!(argument_target(&ir), Some(Ty::Int));
    }

    #[test]
    fn an_unsigned_boundary_coercion_becomes_the_primitive_carrier() {
        let mut ir = IrFile::default();
        let value = ir.add_expr(IrExpr::Const(IrConst::Int(3)));
        let coercion = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: value,
            type_operand: Ty::UInt,
        });
        let call = call_passing(&mut ir, coercion);
        ir.declaration_argument_boundaries.insert(
            call,
            Box::new([IrDeclarationArgumentBoundary {
                argument: coercion,
                parameter: 0,
                declaration: Ty::UInt,
                retarget_coercion: true,
            }]),
        );
        ir.physical_call_parameters
            .insert(call, Box::new([Ty::Int]));
        retarget_declaration_arguments(&mut ir).expect("unsigned boundary");
        assert_eq!(argument_target(&ir), Some(Ty::Int));
        assert!(ir.declaration_argument_boundaries.is_empty());
        assert!(ir.physical_call_parameters.is_empty());
    }

    #[test]
    fn a_recorded_boundary_without_a_physical_signature_fails() {
        let mut ir = IrFile::default();
        let argument = ir.add_expr(IrExpr::Const(IrConst::Int(3)));
        let owner = ir.add_expr(IrExpr::Const(IrConst::Null));
        ir.declaration_argument_boundaries.insert(
            owner,
            Box::new([IrDeclarationArgumentBoundary {
                argument,
                parameter: 0,
                declaration: Ty::UInt,
                retarget_coercion: false,
            }]),
        );

        assert_eq!(
            retarget_declaration_arguments(&mut ir),
            Err(format!(
                "call {owner} has 1 recorded declaration arguments but no physical signature"
            ))
        );
    }

    #[test]
    fn an_unsigned_type_parameter_boundary_is_retargeted_in_place() {
        let mut ir = IrFile::default();
        let value = ir.add_expr(IrExpr::Const(IrConst::Int(3)));
        let coercion = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: value,
            type_operand: Ty::ty_param("T", Ty::UInt),
        });
        let call = call_passing(&mut ir, coercion);
        ir.declaration_argument_boundaries.insert(
            call,
            Box::new([IrDeclarationArgumentBoundary {
                argument: coercion,
                parameter: 0,
                declaration: Ty::ty_param("T", Ty::UInt),
                retarget_coercion: true,
            }]),
        );
        ir.physical_call_parameters
            .insert(call, Box::new([Ty::Int]));
        retarget_declaration_arguments(&mut ir).expect("unsigned type parameter");
        assert_eq!(call_argument(&ir), coercion);
        assert_eq!(argument_target(&ir), Some(Ty::Int));
    }

    #[test]
    fn a_signed_primitive_slot_keeps_the_frontend_operand() {
        let mut ir = recorded_call(IrExpr::Const(IrConst::Int(1)), Ty::Char);
        retarget_declaration_arguments(&mut ir).expect("signed primitive");
        assert!(
            matches!(ir.expr(call_argument(&ir)), IrExpr::Const(IrConst::Int(1))),
            "a primitive-bound argument keeps the boxing its default stub applies"
        );
    }

    #[test]
    fn a_reference_physical_slot_keeps_the_frontend_operand() {
        let mut ir = recorded_call(IrExpr::Const(IrConst::Int(1)), Ty::fun(vec![], Ty::String));
        retarget_declaration_arguments(&mut ir).expect("reference slot");
        let argument = call_argument(&ir);
        assert!(
            matches!(ir.expr(argument), IrExpr::Const(IrConst::Int(1))),
            "a suspend function slot must not gain a carrier coercion"
        );
    }

    #[test]
    fn a_function_conversion_is_not_rewritten_to_a_scalar() {
        let mut ir = IrFile::default();
        let value = ir.add_expr(IrExpr::Const(IrConst::Null));
        let coercion = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: value,
            type_operand: Ty::fun(vec![Ty::String], Ty::Int),
        });
        let call = call_passing(&mut ir, coercion);
        ir.declaration_argument_boundaries.insert(
            call,
            Box::new([IrDeclarationArgumentBoundary {
                argument: coercion,
                parameter: 0,
                declaration: Ty::fun(vec![Ty::String], Ty::Int),
                retarget_coercion: false,
            }]),
        );
        ir.physical_call_parameters
            .insert(call, Box::new([Ty::Int]));
        retarget_declaration_arguments(&mut ir).expect("function conversion");
        assert_eq!(
            argument_target(&ir),
            Some(Ty::fun(vec![Ty::String], Ty::Int))
        );
    }

    #[test]
    fn a_signed_type_parameter_does_not_need_a_physical_signature() {
        let mut ir = IrFile::default();
        let value = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let coercion = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: value,
            type_operand: Ty::ty_param("T", Ty::Char),
        });
        let call = call_passing(&mut ir, coercion);
        ir.declaration_argument_boundaries.insert(
            call,
            Box::new([IrDeclarationArgumentBoundary {
                argument: coercion,
                parameter: 0,
                declaration: Ty::ty_param("T", Ty::Char),
                retarget_coercion: true,
            }]),
        );
        retarget_declaration_arguments(&mut ir).expect("signed type parameter");
        assert_eq!(argument_target(&ir), Some(Ty::ty_param("T", Ty::Char)));
        assert!(ir.declaration_argument_boundaries.is_empty());
    }

    #[test]
    fn an_intersection_class_bound_uses_the_semantic_parameter_identity() {
        let mut ir = IrFile::default();
        let value = ir.add_expr(IrExpr::Const(IrConst::Int(3)));
        let coercion = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: value,
            type_operand: Ty::ty_param("T", Ty::ty_param("X", Ty::obj("kotlin/Any"))),
        });
        let call = call_passing(&mut ir, coercion);
        ir.signatures.insert(
            0,
            crate::ir::IrGenericSig {
                type_params: vec![
                    crate::ir::IrTypeParameter {
                        name: "T".to_string(),
                        semantic_name: "T".to_string(),
                        bounds: vec![(Ty::ty_param("X", Ty::obj("kotlin/Any")), false)],
                        variance: crate::types::TypeVariance::Invariant,
                        reified: false,
                    },
                    crate::ir::IrTypeParameter {
                        name: "display".to_string(),
                        semantic_name: "X".to_string(),
                        bounds: vec![(Ty::obj("kotlin/Comparable"), true), (Ty::UInt, false)],
                        variance: crate::types::TypeVariance::Invariant,
                        reified: false,
                    },
                ],
                params: vec![Ty::ty_param("T", Ty::ty_param("X", Ty::obj("kotlin/Any")))],
                ret: Some(Ty::Boolean),
                supers: Vec::new(),
            },
        );
        ir.declaration_argument_boundaries.insert(
            call,
            Box::new([IrDeclarationArgumentBoundary {
                argument: coercion,
                parameter: 0,
                declaration: Ty::ty_param("T", Ty::ty_param("X", Ty::obj("kotlin/Any"))),
                retarget_coercion: true,
            }]),
        );
        ir.physical_call_parameters
            .insert(call, Box::new([Ty::Int]));
        retarget_declaration_arguments(&mut ir).expect("intersection carrier");
        assert_eq!(argument_target(&ir), Some(Ty::Int));
    }

    #[test]
    fn a_display_name_does_not_select_a_class_bound() {
        let mut ir = IrFile::default();
        let argument = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let call = call_passing(&mut ir, argument);
        ir.signatures.insert(
            0,
            crate::ir::IrGenericSig {
                type_params: vec![crate::ir::IrTypeParameter {
                    name: "X".to_string(),
                    semantic_name: "other".to_string(),
                    bounds: vec![(Ty::UInt, false)],
                    variance: crate::types::TypeVariance::Invariant,
                    reified: false,
                }],
                params: Vec::new(),
                ret: None,
                supers: Vec::new(),
            },
        );
        ir.declaration_argument_boundaries.insert(
            call,
            Box::new([IrDeclarationArgumentBoundary {
                argument,
                parameter: 0,
                declaration: Ty::ty_param("X", Ty::obj("kotlin/Any")),
                retarget_coercion: false,
            }]),
        );
        retarget_declaration_arguments(&mut ir).expect("display name is not an identity");
        assert!(matches!(
            ir.expr(call_argument(&ir)),
            IrExpr::Const(IrConst::Int(1))
        ));
    }

    #[test]
    fn a_cyclic_class_bound_is_an_invariant_error() {
        let mut ir = IrFile::default();
        let argument = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let call = call_passing(&mut ir, argument);
        ir.signatures.insert(
            0,
            crate::ir::IrGenericSig {
                type_params: vec![
                    crate::ir::IrTypeParameter {
                        name: "T".to_string(),
                        semantic_name: "T".to_string(),
                        bounds: vec![(Ty::ty_param("X", Ty::obj("kotlin/Any")), false)],
                        variance: crate::types::TypeVariance::Invariant,
                        reified: false,
                    },
                    crate::ir::IrTypeParameter {
                        name: "X".to_string(),
                        semantic_name: "X".to_string(),
                        bounds: vec![(Ty::ty_param("T", Ty::obj("kotlin/Any")), false)],
                        variance: crate::types::TypeVariance::Invariant,
                        reified: false,
                    },
                ],
                params: Vec::new(),
                ret: None,
                supers: Vec::new(),
            },
        );
        ir.declaration_argument_boundaries.insert(
            call,
            Box::new([IrDeclarationArgumentBoundary {
                argument,
                parameter: 0,
                declaration: Ty::ty_param("T", Ty::ty_param("X", Ty::obj("kotlin/Any"))),
                retarget_coercion: false,
            }]),
        );
        assert_eq!(
            retarget_declaration_arguments(&mut ir),
            Err("type parameter T has a cyclic class bound".to_string())
        );
    }

    fn recorded_call(argument: IrExpr, physical: Ty) -> IrFile {
        let mut ir = IrFile::default();
        let argument = ir.add_expr(argument);
        let call = call_passing(&mut ir, argument);
        ir.declaration_argument_boundaries.insert(
            call,
            Box::new([IrDeclarationArgumentBoundary {
                argument,
                parameter: 0,
                declaration: physical,
                retarget_coercion: false,
            }]),
        );
        ir.physical_call_parameters
            .insert(call, Box::new([physical]));
        ir
    }

    fn call_passing(ir: &mut IrFile, argument: crate::ir::ExprId) -> crate::ir::ExprId {
        ir.add_expr(IrExpr::Call {
            callee: Callee::Local(0),
            dispatch_receiver: None,
            args: vec![argument],
        })
    }

    fn call_id(ir: &IrFile) -> u32 {
        ir.exprs
            .iter()
            .position(|expr| matches!(expr, IrExpr::Call { .. }))
            .expect("call") as u32
    }

    fn call_argument(ir: &IrFile) -> crate::ir::ExprId {
        let IrExpr::Call { args, .. } = ir.expr(call_id(ir)) else {
            panic!("call");
        };
        args[0]
    }

    fn argument_target(ir: &IrFile) -> Option<Ty> {
        match ir.expr(call_argument(ir)) {
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                type_operand,
                ..
            } => Some(*type_operand),
            _ => None,
        }
    }
}
