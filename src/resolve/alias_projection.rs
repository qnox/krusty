//! Projection conflicts introduced by applying a typealias.
//!
//! The parser keeps a use's original spelling and rewrites the reference to the alias target.
//! Variance is not composed in that rewrite. The resolved expansion flattens a same-direction
//! projection, keeps a star, and nests an opposite one. Only the conflict that this application
//! introduced is reported.

use crate::ast::TypeRef;
use crate::diag::DiagnosticIdentity;
use crate::types::Ty;

use super::{
    definitely_non_null_ty, inaccessible_classifier_message, projected_typeref_argument, Checker,
    CheckerScope,
};

impl Checker<'_> {
    /// The pre-expansion spelling of a reference the parser rewrote to a different alias target.
    pub(super) fn rewritten_alias_spelling(&self, reference: &TypeRef) -> Option<TypeRef> {
        let spelled = self.file.alias_spellings.get(&reference.span)?;
        (spelled.name != reference.name).then(|| spelled.clone())
    }

    /// Apply source nullability and record the resolved type. `syntax` is the written reference
    /// when a same-file alias was rewritten, so a `?` on the alias is not applied twice.
    pub(super) fn publish_type_ref(
        &mut self,
        scope: &CheckerScope<'_>,
        syntax: &TypeRef,
        base: Ty,
        unresolved_segment: Option<String>,
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

    /// Resolve a parser-rewritten alias without diagnostics. Speculative reads use this so a
    /// conflict stays on the authoritative type-reference path.
    pub(super) fn silent_rewritten_alias_ty(
        &self,
        scope: &CheckerScope<'_>,
        reference: &TypeRef,
    ) -> Option<Ty> {
        let spelled = self.rewritten_alias_spelling(reference)?;
        let resolved = self.silent_alias_application(scope, &spelled)?;
        Some(apply_written_type_modifiers(&spelled, resolved))
    }

    /// Record that `resolved` contains an opposite projection this application composed, rather
    /// than one already present in a substituted argument.
    pub(super) fn note_introduced_projection_conflict(&mut self, arguments: &[Ty], resolved: Ty) {
        if self.type_ref_depth == 0 {
            return;
        }
        let argument_conflicts = arguments
            .iter()
            .copied()
            .map(crate::types::projection_conflict_count)
            .sum::<usize>();
        if crate::types::projection_conflict_count(resolved) > argument_conflicts {
            self.alias_projection_conflict = true;
        }
    }

    fn silent_alias_application(
        &self,
        scope: &CheckerScope<'_>,
        reference: &TypeRef,
    ) -> Option<Ty> {
        if reference.is_import() {
            return self.scoped_source_alias_target(scope, &reference.name);
        }
        if let Some(alias) = scope.type_alias(&reference.name) {
            return self.silent_substitute_alias(
                scope,
                alias.formals,
                alias.expansion,
                &reference.targs,
            );
        }
        if let Some(alias) = self.qualified_body_local_type_alias(scope, &reference.name) {
            return self.silent_substitute_alias(
                scope,
                alias.formals,
                alias.expansion,
                &reference.targs,
            );
        }
        let identity = self.scoped_source_alias_identity(scope, &reference.name)?;
        let (formals, expansion) = self.source_alias_expansion(identity)?;
        self.silent_substitute_alias(scope, formals, expansion, &reference.targs)
    }

    fn silent_substitute_alias(
        &self,
        scope: &CheckerScope<'_>,
        formals: Vec<String>,
        expansion: Ty,
        arguments: &[TypeRef],
    ) -> Option<Ty> {
        if formals.len() != arguments.len() {
            return None;
        }
        if formals.is_empty() {
            return Some(expansion);
        }
        let arguments = arguments
            .iter()
            .map(|argument| {
                let resolved = if argument.is_star_projection() {
                    Ty::Error
                } else {
                    self.type_ref_ty_silent(scope, argument)
                };
                projected_typeref_argument(argument, resolved, Ty::nullable(Ty::obj("kotlin/Any")))
            })
            .collect::<Vec<_>>();
        let bindings = formals
            .into_iter()
            .zip(arguments)
            .collect::<crate::symbol_resolver::GSigBinds>();
        Some(crate::symbol_resolver::ty_subst(expansion, &bindings))
    }
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
