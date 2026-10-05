//! Projection conflicts introduced by one typealias application.
//!
//! The parser leaves a use written as the source spelled it. Classifier selection returns the
//! alias that won that use, and this module composes the use-site arguments with that alias's
//! template. A conflict is the use-site projection that this application itself composed with the
//! opposite declaration-site projection; it is not inferred by scanning the finished type.

use std::collections::{HashMap, HashSet};

use crate::ast::TypeRef;
use crate::diag::{DiagnosticIdentity, Span};
use crate::types::Ty;

use super::{definitely_non_null_ty, inaccessible_classifier_message, Checker, CheckerScope};

/// One alias template applied to the arguments of a single use.
pub(super) struct AppliedAlias {
    pub(super) ty: Ty,
    pub(super) conflicts: Vec<Span>,
}

impl Checker<'_> {
    /// Apply source nullability and record the resolved type.
    pub(super) fn publish_type_ref(
        &mut self,
        scope: &CheckerScope<'_>,
        syntax: &TypeRef,
        base: Ty,
        unresolved_segment: Option<crate::symbol_resolver::ClassifierMiss>,
    ) -> Ty {
        let resolved = apply_written_type_modifiers(syntax, base);
        if resolved == Ty::Error {
            if let Some(segment) = unresolved_segment {
                self.unresolved_type_segments
                    .insert((syntax.span.lo, syntax.span.hi), segment);
            }
        }
        if !scope.tparam_contains(&syntax.name) {
            if let Some(internal) = resolved.non_null().kotlin_class_internal() {
                if !syntax.is_import() && !self.suppresses_diagnostic("INVISIBLE_REFERENCE") {
                    if let Some(access) = self.resolver().inaccessible_classifier_access(internal) {
                        self.diags.error_with_identity(
                            syntax.span,
                            DiagnosticIdentity::ClassifierAccess {
                                reference: syntax.span,
                                classifier: internal,
                            },
                            inaccessible_classifier_message(&syntax.name, access),
                        );
                    }
                }
            }
        }
        if resolved != Ty::Error && !syntax.is_import() {
            self.resolved_type_tys
                .insert((syntax.span.lo, syntax.span.hi), resolved);
        }
        resolved
    }

    pub(super) fn report_alias_projection_conflicts(&mut self, conflicts: &[Span], ty: Ty) {
        if conflicts.is_empty() || ty == Ty::Error {
            return;
        }
        let message = format!(
            "conflicting projection in type alias expansion in intermediate type '{}'.",
            ty.source_name()
        );
        for span in conflicts {
            self.diags.error(*span, message.clone());
        }
    }

    pub(super) fn silent_alias_substitution(
        &self,
        scope: &CheckerScope<'_>,
        formals: &[String],
        expansion: Ty,
        arguments: &[TypeRef],
    ) -> Ty {
        if formals.is_empty() {
            return expansion;
        }
        let arguments = arguments
            .iter()
            .map(|argument| {
                let resolved = if argument.is_star_projection() {
                    Ty::Error
                } else {
                    self.type_ref_ty_silent(scope, argument)
                };
                super::projected_typeref_argument(
                    argument,
                    resolved,
                    Ty::nullable(Ty::obj("kotlin/Any")),
                )
            })
            .collect::<Vec<_>>();
        let bindings = formals
            .iter()
            .cloned()
            .zip(arguments)
            .collect::<crate::symbol_resolver::GSigBinds>();
        crate::symbol_resolver::ty_subst(expansion, &bindings)
    }
}

/// Use-site arguments whose projection this template composed with the opposite direction.
///
/// The span is the `in` or `out` keyword of that argument. Arguments are returned in source order,
/// once each, even when the formal occurs in several template positions.
pub(super) fn conflicting_use_site_spans(
    formals: &[String],
    arguments: &[TypeRef],
    expansion: Ty,
    bindings: &HashMap<String, Ty>,
) -> Vec<Span> {
    let mut conflicting = HashSet::new();
    note_template_conflicts(expansion, bindings, &mut conflicting);
    formals
        .iter()
        .zip(arguments)
        .filter_map(|(formal, argument)| {
            conflicting
                .contains(formal)
                .then(|| use_site_projection_span(argument))
                .flatten()
        })
        .collect()
}

fn note_template_conflicts(
    template: Ty,
    bindings: &HashMap<String, Ty>,
    conflicting: &mut HashSet<String>,
) {
    match template {
        Ty::OutProjection(inner) => {
            if matches!(
                crate::symbol_resolver::ty_subst(*inner, bindings),
                Ty::InProjection(_)
            ) {
                if let Some(formal) = projected_formal(*inner) {
                    if bindings.contains_key(&formal) {
                        conflicting.insert(formal);
                    }
                }
            }
            note_template_conflicts(*inner, bindings, conflicting);
        }
        Ty::InProjection(inner) => {
            if matches!(
                crate::symbol_resolver::ty_subst(*inner, bindings),
                Ty::OutProjection(_)
            ) {
                if let Some(formal) = projected_formal(*inner) {
                    if bindings.contains_key(&formal) {
                        conflicting.insert(formal);
                    }
                }
            }
            note_template_conflicts(*inner, bindings, conflicting);
        }
        Ty::Nullable(inner)
        | Ty::PlatformNullable(inner)
        | Ty::DefinitelyNotNull(inner)
        | Ty::StarProjection(inner) => note_template_conflicts(*inner, bindings, conflicting),
        Ty::Obj(_, arguments) => {
            for argument in arguments {
                note_template_conflicts(*argument, bindings, conflicting);
            }
        }
        Ty::Fun(signature) => {
            for parameter in &signature.params {
                note_template_conflicts(*parameter, bindings, conflicting);
            }
            note_template_conflicts(signature.ret, bindings, conflicting);
        }
        Ty::Intersection(parts) => {
            for part in parts {
                note_template_conflicts(*part, bindings, conflicting);
            }
        }
        Ty::TyParam(_, bound) => note_template_conflicts(*bound, bindings, conflicting),
        Ty::Unit | Ty::Null | Ty::Nothing | Ty::Error | Ty::Pending => {}
    }
}

fn projected_formal(inner: Ty) -> Option<String> {
    match inner {
        Ty::TyParam(name, _) => Some(name.to_string()),
        Ty::Nullable(inner) | Ty::PlatformNullable(inner) | Ty::DefinitelyNotNull(inner) => {
            projected_formal(*inner)
        }
        _ => None,
    }
}

fn use_site_projection_span(argument: &TypeRef) -> Option<Span> {
    let prefix = if argument.in_projection() {
        3
    } else if argument.out_projection() {
        4
    } else {
        return None;
    };
    Some(Span::new(
        argument.span.lo.saturating_sub(prefix),
        argument.span.hi,
    ))
}

fn apply_written_type_modifiers(syntax: &TypeRef, base: Ty) -> Ty {
    let base = if syntax.definitely_non_null() {
        definitely_non_null_ty(base)
    } else {
        base
    };
    if syntax.nullable() && base != Ty::Error {
        Ty::nullable(base)
    } else {
        base
    }
}
