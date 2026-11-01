//! Semantic classifier applications that carry the type parameters an inner or local classifier
//! captures from its enclosing declarations, after its own.

use crate::resolve::{ClassSig, SymbolTable};
use crate::types::{Ty, TyKind};

/// The classifier applied to its own type parameters followed by those it captures.
pub(super) fn semantic_classifier_self(signature: &ClassSig) -> Ty {
    let own = signature
        .type_params
        .iter()
        .enumerate()
        .map(|(ordinal, parameter)| {
            Ty::ty_param(
                parameter,
                signature
                    .type_param_bounds
                    .get(ordinal)
                    .copied()
                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any"))),
            )
        });
    let captured = signature
        .captured_type_parameters
        .type_params
        .iter()
        .enumerate()
        .map(|(ordinal, parameter)| {
            Ty::ty_param(
                parameter,
                signature
                    .captured_type_parameters
                    .type_param_bounds
                    .get(ordinal)
                    .copied()
                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any"))),
            )
        });
    let arguments = own.chain(captured).collect::<Vec<_>>();
    Ty::obj_args_name(signature.internal, &arguments)
}

/// Complete source classifier applications emitted by the transitional checked-signature table.
/// Source syntax writes only an inner/local classifier's own arguments; the stable semantic type
/// carries captured declaration arguments after them. Compact explicit-header resolution will
/// eventually be the sole producer, but every signature crossing this bridge must already obey the
/// final representation invariant.
pub(super) fn semantic_type_with_classifier_captures(table: &SymbolTable, ty: Ty) -> Ty {
    match ty.kind() {
        TyKind::Obj(owner, arguments) => {
            let mut arguments = arguments
                .iter()
                .copied()
                .map(|argument| semantic_type_with_classifier_captures(table, argument))
                .collect::<Vec<_>>();
            if let Some(classifier) = table.class_by_type_name(owner) {
                let own = classifier.type_params.len();
                let captured = &classifier.captured_type_parameters;
                if arguments.len() >= own && arguments.len() < own + captured.type_params.len() {
                    arguments.extend(
                        captured
                            .type_params
                            .iter()
                            .enumerate()
                            .skip(arguments.len().saturating_sub(own))
                            .map(|(ordinal, parameter)| {
                                Ty::ty_param(
                                    parameter,
                                    captured
                                        .type_param_bounds
                                        .get(ordinal)
                                        .copied()
                                        .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any"))),
                                )
                            }),
                    );
                }
            }
            Ty::obj_args_name(owner, &arguments)
        }
        TyKind::Fun(function) => Ty::fun_with_shape(
            function
                .params
                .iter()
                .copied()
                .map(|parameter| semantic_type_with_classifier_captures(table, parameter))
                .collect(),
            semantic_type_with_classifier_captures(table, function.ret),
            function.context_count,
            function.has_receiver,
            function.suspend,
        ),
        TyKind::Nullable(inner) => {
            Ty::nullable(semantic_type_with_classifier_captures(table, inner))
        }
        TyKind::PlatformNullable(inner) => {
            Ty::platform_nullable(semantic_type_with_classifier_captures(table, inner))
        }
        TyKind::InProjection(inner) => {
            Ty::in_projection(semantic_type_with_classifier_captures(table, inner))
        }
        TyKind::OutProjection(inner) => {
            Ty::out_projection(semantic_type_with_classifier_captures(table, inner))
        }
        TyKind::StarProjection(inner) => {
            Ty::star_projection(semantic_type_with_classifier_captures(table, inner))
        }
        _ => ty,
    }
}
