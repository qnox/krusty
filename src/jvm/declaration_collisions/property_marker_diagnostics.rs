//! JVM diagnostic types for synthetic property-annotation marker parameters.

use crate::types::Ty;

use super::{MemberMethodDeclaration, PropertyAccessorDeclaration};

pub(super) fn member_method_group_source_order(
    ir: &crate::ir::IrFile,
    class: &crate::ir::IrClass,
    declarations: &[MemberMethodDeclaration],
) -> (u32, u8, u32) {
    declarations
        .iter()
        .map(|declaration| match *declaration {
            MemberMethodDeclaration::Source { offset, function }
            | MemberMethodDeclaration::CompanionBridge { offset, function }
            | MemberMethodDeclaration::GeneratedDataMember { offset, function }
            | MemberMethodDeclaration::DelegationForwarder { offset, function } => (
                ir.fn_source_order.get(&function).copied().unwrap_or(offset),
                0,
                function,
            ),
            MemberMethodDeclaration::GeneratedMethod { offset } => (offset, 1, 0),
            MemberMethodDeclaration::InPlaceDelegate {
                function,
                source_order,
                ..
            } => (source_order, 1, function),
            MemberMethodDeclaration::PropertyAnnotationMarker {
                function,
                source_order,
                ..
            } => (source_order, 2, function),
            MemberMethodDeclaration::PropertyAccessor {
                offset,
                function,
                property,
                setter,
            } => {
                let source_order = match property {
                    PropertyAccessorDeclaration::Member(index) => class
                        .properties
                        .get(index)
                        .map_or(offset, |property| property.source_order),
                    PropertyAccessorDeclaration::MemberExtension(index) => ir
                        .member_ext_props
                        .get(&class.fq_name)
                        .and_then(|properties| properties.get(index))
                        .map_or(offset, |property| property.source_order),
                    PropertyAccessorDeclaration::CompanionBlock(source_order) => source_order,
                };
                (
                    source_order,
                    3 + u8::from(setter),
                    function.unwrap_or(u32::MAX),
                )
            }
        })
        .min()
        .expect("a collision group contains at least two declarations")
}

pub(super) fn marker_parameters(ir: &crate::ir::IrFile, function: crate::ir::FunId) -> Vec<Ty> {
    ir.functions[function as usize]
        .params
        .iter()
        .map(|parameter| replace_type_parameters_with_erased_bounds(*parameter))
        .collect()
}

pub(super) fn marker_source_parameters(
    ir: &crate::ir::IrFile,
    declaration: MemberMethodDeclaration,
) -> Vec<Ty> {
    match declaration {
        MemberMethodDeclaration::Source { function, .. } => {
            super::semantic_function_parameters(ir, function).to_vec()
        }
        MemberMethodDeclaration::PropertyAnnotationMarker { function, .. } => {
            marker_parameters(ir, function)
        }
        _ => unreachable!("the marker/source projection receives its named pair"),
    }
}

pub(super) fn order_marker_sources(ir: &crate::ir::IrFile, listed: &mut [MemberMethodDeclaration]) {
    let mut marker_sources = listed
        .iter()
        .copied()
        .filter(|declaration| {
            matches!(
                declaration,
                MemberMethodDeclaration::PropertyAnnotationMarker { .. }
                    | MemberMethodDeclaration::Source { .. }
            )
        })
        .map(|declaration| (declaration, marker_source_parameters(ir, declaration)))
        .collect::<Vec<_>>();

    // The diagnostic spelling is the primary order, but it is not an identity: distinct erased
    // bounds can both render as `List<*>`. Kotlinc orders a generated marker before a source
    // declaration whenever those displayed parameter keys tie, then retains declaration order
    // within each kind. Express that as one total tuple so hash-map iteration is never observable.
    marker_sources.sort_by(|(left, left_parameters), (right, right_parameters)| {
        qualified_parameter_key(left_parameters)
            .cmp(&qualified_parameter_key(right_parameters))
            .then_with(|| declaration_order(*left).cmp(&declaration_order(*right)))
    });
    let mut marker_sources = marker_sources
        .into_iter()
        .map(|(declaration, _)| declaration);
    for declaration in listed {
        if matches!(
            declaration,
            MemberMethodDeclaration::PropertyAnnotationMarker { .. }
                | MemberMethodDeclaration::Source { .. }
        ) {
            *declaration = marker_sources
                .next()
                .expect("the marker/source partition retains its size");
        }
    }
}

fn declaration_order(declaration: MemberMethodDeclaration) -> (u8, u32, crate::ir::FunId) {
    match declaration {
        MemberMethodDeclaration::PropertyAnnotationMarker {
            function,
            source_order,
            ..
        } => (0, source_order, function),
        MemberMethodDeclaration::Source {
            offset, function, ..
        } => (1, offset, function),
        _ => unreachable!("the marker/source projection receives its named pair"),
    }
}

fn qualified_parameter_key(parameters: &[Ty]) -> Vec<String> {
    parameters
        .iter()
        .map(|parameter| {
            super::qualified_type_name(*parameter)
                .expect("an ordered member-clash type is determined")
        })
        .collect()
}

pub(super) fn declaration_types(
    ir: &crate::ir::IrFile,
    class: &crate::ir::IrClass,
    declaration: MemberMethodDeclaration,
) -> Vec<Ty> {
    let function = match declaration {
        MemberMethodDeclaration::Source { function, .. }
        | MemberMethodDeclaration::CompanionBridge { function, .. }
        | MemberMethodDeclaration::GeneratedDataMember { function, .. }
        | MemberMethodDeclaration::InPlaceDelegate { function, .. }
        | MemberMethodDeclaration::DelegationForwarder { function, .. } => Some(function),
        MemberMethodDeclaration::PropertyAnnotationMarker { function, .. } => {
            let mut types = marker_parameters(ir, function);
            types.push(function_return(ir, function));
            return types;
        }
        MemberMethodDeclaration::PropertyAccessor {
            function: Some(function),
            ..
        } => Some(function),
        MemberMethodDeclaration::PropertyAccessor {
            function: None,
            property: PropertyAccessorDeclaration::Member(index),
            setter,
            ..
        } => {
            let property = &class.properties[index];
            let mut types = property
                .context_params
                .iter()
                .map(|(_, _, ty)| *ty)
                .collect::<Vec<_>>();
            if setter {
                types.push(property.ty);
                types.push(Ty::Unit);
            } else {
                types.push(property.ty);
            }
            return types;
        }
        MemberMethodDeclaration::GeneratedMethod { .. }
        | MemberMethodDeclaration::PropertyAccessor { function: None, .. } => return Vec::new(),
    };
    let function = function.expect("a declaration with semantic types retains its function");
    let mut types = super::semantic_function_parameters(ir, function).to_vec();
    types.push(function_return(ir, function));
    if let Some(signature) = ir.signatures.get(&function) {
        types.extend(
            signature
                .type_params
                .iter()
                .flat_map(|parameter| parameter.bounds.iter().map(|(bound, _)| *bound)),
        );
    }
    types
}

fn function_return(ir: &crate::ir::IrFile, function: crate::ir::FunId) -> Ty {
    ir.vc_declared_sigs
        .get(&function)
        .map(|(_, _, ret)| *ret)
        .or_else(|| {
            ir.signatures
                .get(&function)
                .and_then(|signature| signature.ret)
        })
        .unwrap_or(ir.functions[function as usize].ret)
}

pub(super) fn replace_type_parameters_with_erased_bounds(ty: Ty) -> Ty {
    match ty {
        Ty::TyParam(_, bound) => erased_bound(*bound),
        Ty::Fun(signature) => Ty::fun_with_shape(
            signature
                .params
                .iter()
                .map(|parameter| replace_type_parameters_with_erased_bounds(*parameter))
                .collect(),
            replace_type_parameters_with_erased_bounds(signature.ret),
            signature.context_count,
            signature.has_receiver,
            signature.suspend,
        ),
        Ty::Nullable(inner) => Ty::nullable(replace_type_parameters_with_erased_bounds(*inner)),
        Ty::PlatformNullable(inner) => {
            Ty::platform_nullable(replace_type_parameters_with_erased_bounds(*inner))
        }
        Ty::InProjection(inner) => {
            Ty::in_projection(replace_type_parameters_with_erased_bounds(*inner))
        }
        Ty::OutProjection(inner) => {
            Ty::out_projection(replace_type_parameters_with_erased_bounds(*inner))
        }
        Ty::StarProjection(inner) => {
            Ty::star_projection(replace_type_parameters_with_erased_bounds(*inner))
        }
        Ty::DefinitelyNotNull(inner) => {
            replace_type_parameters_with_erased_bounds(*inner).definitely_non_null()
        }
        Ty::Obj(internal, arguments) if !arguments.is_empty() => Ty::obj_args_name(
            internal,
            &arguments
                .iter()
                .map(|argument| replace_type_parameters_with_erased_bounds(*argument))
                .collect::<Vec<_>>(),
        ),
        Ty::Intersection(parts) => Ty::intersection(
            &parts
                .iter()
                .map(|part| replace_type_parameters_with_erased_bounds(*part))
                .collect::<Vec<_>>(),
        ),
        _ => ty,
    }
}

fn erased_bound(bound: Ty) -> Ty {
    match bound {
        Ty::TyParam(_, next) => erased_bound(*next),
        Ty::Nullable(inner) => Ty::nullable(erased_bound(*inner)),
        Ty::PlatformNullable(inner) => Ty::platform_nullable(erased_bound(*inner)),
        Ty::DefinitelyNotNull(inner) => erased_bound(*inner).definitely_non_null(),
        Ty::Fun(signature) => {
            let classifier = crate::libraries::function_classifiers::function_classifier_identity(
                signature.params.len(),
                signature.suspend,
            );
            let star = Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")));
            Ty::obj_args_name(classifier, &vec![star; signature.params.len() + 1])
        }
        Ty::Obj(internal, arguments) if !arguments.is_empty() => Ty::obj_args_name(
            internal,
            &arguments
                .iter()
                .map(|argument| Ty::star_projection(*argument))
                .collect::<Vec<_>>(),
        ),
        _ => bound,
    }
}
