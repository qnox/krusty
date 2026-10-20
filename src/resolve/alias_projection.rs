//! Projection conflicts introduced by one typealias application.
//!
//! The parser leaves a use written as the source spelled it. Classifier selection returns the
//! alias that won that use, and this module composes the use-site arguments with that alias's
//! template. A conflict is the use-site projection that this application itself composed with the
//! opposite declaration-site projection; it is not inferred by scanning the finished type.

use crate::fx_hash::{HashMap, HashSet};

use crate::ast::TypeRef;
use crate::diag::{DiagnosticIdentity, Span};
use crate::types::{Ty, TypeName};

use super::{
    definitely_non_null_ty, function_type_ref_shape, inaccessible_classifier_message, Checker,
    CheckerScope,
};

/// The type arguments one use applies an alias to.
#[derive(Clone, Copy)]
pub(super) enum AliasArguments<'r> {
    /// The arguments written at the use, which must match the alias's arity.
    Written(&'r [TypeRef]),
    /// A class literal's left-hand side written without arguments: kotlinc applies the alias to a
    /// star projection of each of its parameters (`Arr::class` is `KClass<Arr<*>>`).
    StarProjected,
}

impl<'r> AliasArguments<'r> {
    pub(super) fn of(reference: &'r TypeRef) -> Self {
        if reference.is_class_literal_lhs() && reference.targs.is_empty() {
            Self::StarProjected
        } else {
            Self::Written(&reference.targs)
        }
    }
}

/// Apply a generated star for every omitted alias parameter, then give every retained classifier
/// argument the readable bound owned by that expanded classifier. Alias parameters themselves
/// cannot declare bounds, but `typealias Alias<T> = Box<T>` must still produce `Box<*>` whose star
/// reads as `Box`'s declared `T` bound.
pub(super) fn star_projected_alias_type(
    formals: &[String],
    expansion: Ty,
    classifier: &mut dyn FnMut(TypeName) -> Option<std::sync::Arc<crate::libraries::LibraryType>>,
) -> Ty {
    let star = Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")));
    let bindings = formals
        .iter()
        .map(|formal| (formal.clone(), star))
        .collect::<crate::symbol_resolver::GSigBinds>();
    let applied = crate::types::ty_subst_alias_expansion(expansion, &bindings);
    alias_target_star_bounds(applied, classifier)
}

fn alias_target_star_bounds(
    ty: Ty,
    classifier: &mut dyn FnMut(TypeName) -> Option<std::sync::Arc<crate::libraries::LibraryType>>,
) -> Ty {
    match ty {
        Ty::Nullable(inner) => Ty::nullable(alias_target_star_bounds(*inner, classifier)),
        Ty::PlatformNullable(inner) => {
            Ty::platform_nullable(alias_target_star_bounds(*inner, classifier))
        }
        Ty::DefinitelyNotNull(inner) => Ty::DefinitelyNotNull(crate::types::intern_ty(
            alias_target_star_bounds(*inner, classifier),
        )),
        Ty::InProjection(inner) => Ty::in_projection(alias_target_star_bounds(*inner, classifier)),
        Ty::OutProjection(inner) => {
            Ty::out_projection(alias_target_star_bounds(*inner, classifier))
        }
        // A retained star already carries the bound of its owning classifier. Only a star in the
        // argument list below has that owner available; descending through the stored bound can
        // also revisit a recursive F-bound indefinitely.
        Ty::StarProjection(_) => ty,
        Ty::Obj(owner, arguments) if !arguments.is_empty() => {
            let parsed = arguments
                .iter()
                .map(|argument| {
                    if generated_star_argument(*argument) {
                        None
                    } else {
                        Some(alias_target_star_bounds(*argument, classifier))
                    }
                })
                .collect::<Vec<_>>();
            let mut resolved = parsed
                .iter()
                .zip(arguments)
                .map(|(parsed, original)| parsed.unwrap_or(*original))
                .collect::<Vec<_>>();
            if parsed.iter().any(Option::is_none) {
                let declaration = classifier(owner)
                    .expect("resolved typealias expansion target has no classifier declaration");
                let bindings = super::projected_classifier_argument_bindings(
                    declaration.type_params(),
                    &parsed,
                );
                let default_bound = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
                for (index, argument) in parsed.iter().enumerate() {
                    if argument.is_some() {
                        continue;
                    }
                    let upper_bound = declaration
                        .type_param_bounds()
                        .get(index)
                        .and_then(|bounds| bounds.first())
                        .copied()
                        .map(|bound| {
                            crate::symbol_resolver::ty_subst_keep_unbound(bound, &bindings)
                        })
                        .unwrap_or(default_bound);
                    resolved[index] = Ty::star_projection(upper_bound);
                }
            }
            Ty::obj_args_name(owner, &resolved)
        }
        Ty::Fun(signature) => Ty::fun_with_shape(
            signature
                .params
                .iter()
                .map(|parameter| alias_target_star_bounds(*parameter, classifier))
                .collect::<Vec<_>>(),
            alias_target_star_bounds(signature.ret, classifier),
            signature.context_count,
            signature.has_receiver,
            signature.suspend,
        ),
        Ty::Intersection(parts) => Ty::intersection(
            &parts
                .iter()
                .map(|part| alias_target_star_bounds(*part, classifier))
                .collect::<Vec<_>>(),
        ),
        // Alias formals have already been substituted. Any remaining variable belongs to an
        // enclosing declaration and keeps its declaration-owned bound; following an F-bound here
        // would revisit the same variable.
        Ty::TyParam(_, _) => ty,
        other => other,
    }
}

/// Whether an expanded classifier argument is one of this alias application's generated stars.
/// Substituting a star for a formal used as `T?`, `in T`, or `out T` wraps the projection in the
/// occurrence's modifier, but Kotlin still projects the owning classifier argument as `*`; the
/// owner supplies its bound below. Source syntax cannot write one of these wrappers around a star
/// in an alias application with omitted arguments, so it arises from the generated alias-formal
/// binding rather than from an independently written target argument.
fn generated_star_argument(ty: Ty) -> bool {
    match ty {
        Ty::StarProjection(_) => true,
        Ty::Nullable(inner)
        | Ty::PlatformNullable(inner)
        | Ty::DefinitelyNotNull(inner)
        | Ty::InProjection(inner)
        | Ty::OutProjection(inner) => generated_star_argument(*inner),
        _ => false,
    }
}

/// Exact alias template selected before substitution.
#[derive(Clone)]
pub(super) struct SelectedAliasApplication {
    pub(super) formals: Vec<String>,
    pub(super) expansion: Ty,
    pub(super) expansion_spelling: crate::spelling::Spelled,
}

/// One alias template applied to the arguments of a single use.
pub(super) struct AppliedAlias {
    pub(super) ty: Ty,
    pub(super) conflicts: Vec<Span>,
    /// A later diagnostic walk consumes this semantic handoff instead of resolving the spelling.
    pub(super) selected: Option<SelectedAliasApplication>,
}

impl Checker<'_> {
    /// Retain the exact alias selected for one type occurrence while the authoritative scope walk
    /// is still in hand. The class-variance post-check reads this bounded side table.
    pub(super) fn retain_selected_alias_application(
        &mut self,
        reference: &TypeRef,
        applied: &AppliedAlias,
    ) {
        if reference.is_import() {
            return;
        }
        if let Some(selected) = &applied.selected {
            self.selected_type_alias_applications
                .insert((reference.span.lo, reference.span.hi), selected.clone());
        }
    }

    /// Restore alias provenance for a declaration occurrence whose semantic type and spelling were
    /// finalized by Pass 1. `Spelled::alias` is the qualified identity selected there, so this walk
    /// never repeats scope lookup from a source name.
    pub(super) fn retain_finalized_alias_applications(
        &mut self,
        reference: &TypeRef,
        spelled: &crate::spelling::Spelled,
    ) {
        self.retain_finalized_type_use_annotations(reference, spelled);
        if let Some(identity) = spelled.alias {
            if let Some(binding) = self.source_alias_binding(identity) {
                self.selected_type_alias_applications.insert(
                    (reference.span.lo, reference.span.hi),
                    SelectedAliasApplication {
                        formals: binding.formals,
                        expansion: binding.expansion,
                        expansion_spelling: binding.expansion_spelling,
                    },
                );
            }
        }

        let references = if function_type_ref_shape(reference).is_some() {
            reference
                .fun_params
                .iter()
                .chain(reference.arg.as_deref())
                .collect::<Vec<_>>()
        } else {
            reference.targs.iter().collect::<Vec<_>>()
        };
        if spelled.alias.is_some() {
            for (child, (_, child_spelling)) in references.iter().zip(&spelled.alias_args) {
                self.retain_finalized_alias_applications(child, child_spelling);
            }
        } else {
            for (child, child_spelling) in references.iter().zip(&spelled.args) {
                self.retain_finalized_alias_applications(child, child_spelling);
            }
        }
    }

    /// Resolve a reference to a scoped source `typealias` by substituting the use's type arguments
    /// into the collected expansion. Same-file and cross-file spellings use this one semantic
    /// operation.
    pub(super) fn scoped_source_alias_ty(
        &mut self,
        scope: &CheckerScope<'_>,
        reference: &TypeRef,
    ) -> Option<AppliedAlias> {
        // A detached import selects the alias declaration; it is not an application of a generic
        // alias with an empty argument list. Ordinary uses of the imported spelling still enter
        // below and must provide or infer their own arguments.
        if reference.is_import() {
            return self
                .scoped_source_alias_target(scope, &reference.name)
                .map(|ty| AppliedAlias {
                    ty,
                    conflicts: Vec::new(),
                    selected: None,
                });
        }
        if let Some(alias) = scope.type_alias(&reference.name) {
            return Some(self.alias_application_ty_with_spelling(
                scope,
                alias.formals,
                alias.expansion,
                alias.expansion_spelling,
                &reference.name,
                AliasArguments::of(reference),
                reference.span,
            ));
        }
        let identity = self.scoped_source_alias_identity(scope, &reference.name)?;
        Some(self.source_alias_application_ty(
            scope,
            identity,
            &reference.name,
            AliasArguments::of(reference),
            reference.span,
        ))
    }

    /// Apply one already-scoped source alias to use-site type arguments. Type syntax and
    /// constructor syntax share this exact expansion.
    pub(super) fn source_alias_application_ty(
        &mut self,
        scope: &CheckerScope<'_>,
        identity: TypeName,
        display_name: &str,
        arguments: AliasArguments<'_>,
        span: Span,
    ) -> AppliedAlias {
        let Some(binding) = self.source_alias_binding(identity) else {
            return AppliedAlias {
                ty: Ty::Error,
                conflicts: Vec::new(),
                selected: None,
            };
        };
        self.alias_application_ty_with_spelling(
            scope,
            binding.formals,
            binding.expansion,
            binding.expansion_spelling,
            display_name,
            arguments,
            span,
        )
    }

    pub(super) fn alias_application_ty(
        &mut self,
        scope: &CheckerScope<'_>,
        formals: Vec<String>,
        expansion: Ty,
        display_name: &str,
        arguments: AliasArguments<'_>,
        span: Span,
    ) -> AppliedAlias {
        self.alias_application_ty_with_spelling(
            scope,
            formals,
            expansion,
            Default::default(),
            display_name,
            arguments,
            span,
        )
    }

    pub(super) fn alias_application_ty_with_spelling(
        &mut self,
        scope: &CheckerScope<'_>,
        formals: Vec<String>,
        expansion: Ty,
        expansion_spelling: crate::spelling::Spelled,
        display_name: &str,
        arguments: AliasArguments<'_>,
        span: Span,
    ) -> AppliedAlias {
        let selected = SelectedAliasApplication {
            formals: formals.clone(),
            expansion,
            expansion_spelling,
        };
        let arguments = match arguments {
            AliasArguments::Written(arguments) => arguments,
            AliasArguments::StarProjected => {
                // A star projection composes with no declaration-site variance to a conflict.
                return AppliedAlias {
                    ty: star_projected_alias_type(&formals, expansion, &mut |owner| {
                        crate::symbol_source::SymbolSource::classifier(&self.fed_source(), owner)
                    }),
                    conflicts: Vec::new(),
                    selected: Some(selected),
                };
            }
        };
        if formals.len() != arguments.len() {
            self.diags.error(
                span,
                format!(
                    "wrong number of type arguments for type alias '{}': expected {}, found {}.",
                    display_name,
                    formals.len(),
                    arguments.len()
                ),
            );
            return AppliedAlias {
                ty: Ty::Error,
                conflicts: Vec::new(),
                selected: Some(selected),
            };
        }
        if formals.is_empty() {
            self.check_alias_application_bounds(&formals, expansion, arguments, &[], span);
            return AppliedAlias {
                ty: expansion,
                conflicts: Vec::new(),
                selected: Some(selected),
            };
        }
        // Preserve use-site projections during substitution. Alias parameters cannot declare a
        // bound, so a star projection's readable upper bound is always `Any?`.
        let args = arguments
            .iter()
            .map(|argument| {
                let resolved = if argument.is_star_projection() {
                    Ty::Error
                } else {
                    self.type_ref_ty(scope, argument)
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
            .zip(args.iter().copied())
            .collect::<crate::symbol_resolver::GSigBinds>();
        let resolved = crate::types::ty_subst_alias_expansion(expansion, &bindings);
        self.check_alias_application_bounds(&formals, expansion, arguments, &args, span);
        let conflicts = conflicting_use_site_spans(
            &formals,
            arguments,
            expansion,
            &bindings,
            &self.file.type_projection_spans,
        );
        AppliedAlias {
            ty: resolved,
            conflicts,
            selected: Some(selected),
        }
    }

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
            if !scope.tparam_contains(&syntax.name) {
                self.check_type_reference_usage(syntax, resolved);
            }
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
        crate::types::ty_subst_alias_expansion(expansion, &bindings)
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
    projection_spans: &HashMap<Span, Span>,
) -> Vec<Span> {
    let mut conflicting = HashSet::default();
    note_template_conflicts(expansion, bindings, &mut conflicting);
    formals
        .iter()
        .zip(arguments)
        .filter_map(|(formal, argument)| {
            conflicting
                .contains(formal)
                .then(|| use_site_projection_span(argument, projection_spans))
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

fn use_site_projection_span(
    argument: &TypeRef,
    projection_spans: &HashMap<Span, Span>,
) -> Option<Span> {
    if !argument.in_projection() && !argument.out_projection() {
        return None;
    }
    Some(*projection_spans.get(&argument.span).unwrap_or_else(|| {
        panic!(
            "projected type argument {:?} must retain its keyword span",
            argument.span
        )
    }))
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
