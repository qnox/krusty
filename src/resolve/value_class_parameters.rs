//! kotlinc's value-class primary-constructor parameter checks, the parameter loop of
//! `FirValueClassDeclarationChecker`.
//!
//! Each primary-constructor parameter of a value class reports at most one of, in this order:
//! - `VALUE_CLASS_CONSTRUCTOR_NOT_FINAL_READ_ONLY_PARAMETER` at the parameter declaration, for a
//!   final value class's parameter that is not a final read-only property (`var`, a plain
//!   parameter, `vararg`, `open`);
//! - `VALUE_CLASS_HAS_INAPPLICABLE_PARAMETER_TYPE` at the parameter type, for an inline value class
//!   whose underlying type expands to `Unit` or `Nothing`;
//! - `VALUE_CLASS_CANNOT_BE_RECURSIVE` at the parameter type, when following the underlying types of
//!   inline value classes reaches a type already on the path. A path that leaves a classifier
//!   through a type parameter's bound (`value class V<T : V<T>>(val x: T)`) is the deprecation
//!   `VALUE_CLASS_CANNOT_BE_RECURSIVE_VIA_TYPE_PARAMETERS`: a warning until
//!   `ForbidValueClassRecursionViaTypeParameters` makes it the error.
//!
//! kotlinc follows declared underlying types without substituting type arguments: `W<V>` reaches
//! `W`'s own `T`, whose bound is not a value class, so `value class V(val w: W<V>)` is accepted.
//! A Full value class follows every selected primary-constructor field when its occurrence is
//! non-null, and also follows its sole field through a nullable occurrence; nullable multi-field
//! Full values end the path.

use crate::types::TyKind;
use std::collections::HashSet;

use super::*;
use crate::libraries::LibraryType;

/// How a recursive underlying type reaches its value class again.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Recursion {
    /// Through classifier types only: always an error.
    Plain,
    /// Through a type parameter's upper bound: the deprecation.
    ViaTypeParameters,
}

/// The target-aware value-class kind selected during signature collection.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SelectedValueClassKind {
    Inline,
    Full,
}

/// Target-aware value-class classification and the semantic primary-constructor parameter types
/// selected with it. Full classes have one entry per field; inline classes have their one
/// underlying property.
struct SelectedValueClass {
    kind: SelectedValueClassKind,
    parameters: Vec<Ty>,
}

impl Checker<'_> {
    /// Run kotlinc's parameter loop over a value class declaration `cl`, whose classifier is `owner`.
    pub(super) fn check_value_class_parameters(
        &mut self,
        scope: &CheckerScope<'_>,
        cl: &ClassDecl,
        owner: Option<TypeName>,
    ) {
        if self.discover_anonymous_captures
            || !cl.is_value
            || cl.singleton
            || cl.kind != ClassKind::Class
            || cl.primary_constructor_parameters_span.is_none()
        {
            return;
        }
        let Some(owner) = owner else {
            return;
        };
        let Some(record) = self.resolver().classifier(owner) else {
            return;
        };
        // Signature collection already interpreted `@JvmInline` for this target and rejected
        // invalid shapes. Common checking consumes that decision and never re-reads the annotation.
        let selected = self.selected_value_class(owner, &record);
        let full = selected
            .as_ref()
            .is_some_and(|selected| selected.kind == SelectedValueClassKind::Full);
        let value_prefix = if self.file.full_value_classes {
            "final value"
        } else {
            "value"
        };
        let class_final = cl.modality == Modality::Final;
        for (ordinal, parameter) in cl.props.iter().enumerate() {
            let not_final_read_only = !parameter.is_property
                || parameter.is_vararg
                || parameter.is_var
                || parameter.has_open_modifier;
            if class_final && not_final_read_only {
                self.diags.error(
                    parameter.declaration_span,
                    format!(
                        "{value_prefix} class primary constructor must only have final read-only \
                         ('val') property parameters."
                    ),
                );
                continue;
            }
            if full {
                if parameter.is_property && cl.modality == Modality::Abstract {
                    self.diags.error(
                        parameter.declaration_span,
                        "abstract value class primary constructor cannot have property parameters.",
                    );
                    continue;
                } else if parameter.is_property && cl.modality == Modality::Sealed {
                    self.diags.error(
                        parameter.declaration_span,
                        "sealed value class primary constructor cannot have property parameters.",
                    );
                    continue;
                }
            }
            let Some(selected) = selected.as_ref() else {
                continue;
            };
            let Some(underlying) = selected.parameters.get(ordinal).copied() else {
                continue;
            };
            if selected.kind == SelectedValueClassKind::Inline {
                if let Some(rendered) =
                    self.inapplicable_value_class_parameter(scope, parameter, underlying)
                {
                    let prefix = if self.file.full_value_classes {
                        "@JvmInline value"
                    } else {
                        "value"
                    };
                    self.diags.error(
                        parameter.ty.span,
                        format!("{prefix} class cannot have value parameter of type '{rendered}'."),
                    );
                    continue;
                }
            }
            match self.value_class_recursion(&record, underlying, &mut HashSet::new()) {
                Some(Recursion::Plain) => {
                    self.diags
                        .error(parameter.ty.span, "value class cannot be recursive.");
                }
                Some(Recursion::ViaTypeParameters) => {
                    // The warning is inside the parameter declaration, so the parameter's own
                    // `@Suppress` silences it too.
                    let depth = self.push_declaration_policies(
                        scope,
                        &parameter.annotations,
                        &parameter.annotation_args,
                    );
                    self.report_deprecation_error(
                        parameter.ty.span,
                        self.file.forbid_value_class_recursion_via_type_parameters,
                        self.file
                            .value_class_recursion_via_type_parameters_warning
                            .as_deref(),
                        crate::diag::CompilerWarningName::ValueClassCannotBeRecursiveViaTypeParameters,
                        "value class cannot be recursive.",
                    );
                    self.active_lexical_policies.truncate(depth);
                }
                None => {}
            }
        }
    }

    /// Read the selected kind from the declaration representation owned by this checker mode. The
    /// authoritative checker consumes stable declaration flags; pre-finalization inference consumes
    /// the same decision from its temporary signature graph. Neither path reinterprets target rules.
    fn selected_value_class(
        &self,
        owner: TypeName,
        record: &LibraryType,
    ) -> Option<SelectedValueClass> {
        if let Some(index) = self.resolved_index {
            if let Some(declaration) = record.stable_declaration {
                let flags = index
                    .declaration_header(declaration)
                    .expect("a source value class must retain its resolved declaration header")
                    .flags;
                let kind = if flags.has(crate::fir::DeclarationFlags::VALUE) {
                    SelectedValueClassKind::Inline
                } else if flags.has(crate::fir::DeclarationFlags::FULL_VALUE) {
                    SelectedValueClassKind::Full
                } else {
                    return None;
                };
                let constructor = index
                    .owned_declaration(declaration, crate::fir::DeclarationKind::Constructor, 0)
                    .expect("an accepted source value class must retain its primary constructor");
                let parameters = index
                    .signature(constructor)
                    .expect("a source value class must retain its primary-constructor signature")
                    .parameters
                    .iter()
                    .map(|parameter| parameter.get())
                    .collect();
                return Some(SelectedValueClass { kind, parameters });
            }
            return record
                .value_declaration
                .as_ref()
                .map(|declaration| SelectedValueClass {
                    kind: SelectedValueClassKind::Inline,
                    parameters: vec![declaration.underlying],
                });
        }
        if let Some(class) = self.module.legacy_symbols()?.class_by_type_name(owner) {
            let kind = if class.value_field.is_some() {
                SelectedValueClassKind::Inline
            } else if class.full_value {
                SelectedValueClassKind::Full
            } else {
                return None;
            };
            return Some(SelectedValueClass {
                kind,
                parameters: class.ctor_params.clone(),
            });
        }
        record
            .value_declaration
            .as_ref()
            .map(|declaration| SelectedValueClass {
                kind: SelectedValueClassKind::Inline,
                parameters: vec![declaration.underlying],
            })
    }

    /// Whether following selected value-class constructor fields from `ty` reaches a type already
    /// on `path`, and how `ty` itself entered the walk. `owner` declares type parameters in `ty`.
    fn value_class_recursion(
        &self,
        owner: &LibraryType,
        ty: Ty,
        path: &mut HashSet<Ty>,
    ) -> Option<Recursion> {
        let (classifier, expected) = match ty.non_null().kind() {
            TyKind::Obj(name, _) => (name, Recursion::Plain),
            _ => (
                single_upper_bound_classifier(owner, ty)?,
                Recursion::ViaTypeParameters,
            ),
        };
        let record = self.fed_source().classifier(classifier)?;
        let selected = self.selected_value_class(classifier, &record)?;
        // FIR expands a nullable inline representation and a nullable one-field full
        // representation. A nullable multi-field full value is boxed and ends this path.
        if ty.is_nullable()
            && selected.kind == SelectedValueClassKind::Full
            && selected.parameters.len() != 1
        {
            return None;
        }
        if !path.insert(ty) {
            return Some(expected);
        }
        let recursive = selected.parameters.into_iter().any(|parameter| {
            self.value_class_recursion(&record, parameter, path)
                .is_some()
        });
        path.remove(&ty);
        recursive.then_some(expected)
    }

    /// kotlinc's `isInapplicableParameterType`: the fully expanded underlying type is non-null
    /// `Unit` or `Nothing`. The rendering is kotlinc's type `toString`, which names the written
    /// alias before the type it expands to (`{p/U=} kotlin/Unit`).
    fn inapplicable_value_class_parameter(
        &self,
        scope: &CheckerScope<'_>,
        parameter: &PropParam,
        underlying: Ty,
    ) -> Option<String> {
        let expanded = match underlying.kind() {
            TyKind::Unit => "kotlin/Unit",
            TyKind::Obj(name, _) if name == crate::types::wk::unit() => "kotlin/Unit",
            TyKind::Nothing => "kotlin/Nothing",
            TyKind::Obj(name, _) if name == crate::types::wk::nothing() => "kotlin/Nothing",
            _ => return None,
        };
        let alias = self
            .select_type_classifier_binding(scope, &parameter.ty.name)
            .2
            .and_then(|alias| alias.identity);
        Some(match alias {
            Some(alias) => format!("{{{}=}} {expanded}", alias.render()),
            None => expanded.to_string(),
        })
    }
}

/// kotlinc's `leastUpperBound(...).toRegularClassSymbol()` for a type that is not itself a
/// classifier type: the classifier of the one non-type-parameter upper bound reached through
/// type-parameter bounds. Several bounds intersect into a type that names no single classifier.
fn single_upper_bound_classifier(owner: &LibraryType, ty: Ty) -> Option<TypeName> {
    let mut bounds = Vec::new();
    collect_upper_bounds(owner, ty, &mut HashSet::new(), &mut bounds);
    let any = Ty::Obj(crate::types::wk::any(), crate::types::TyList::EMPTY);
    if bounds.len() > 1 {
        bounds.retain(|bound| bound.non_null() != any);
    }
    match bounds.as_slice() {
        [bound] => match bound.non_null().kind() {
            TyKind::Obj(name, _) => Some(name),
            _ => None,
        },
        _ => None,
    }
}

/// The upper bounds of `ty` that are not type parameters, following type-parameter bounds
/// (kotlinc's `collectUpperBounds`). A type parameter `owner` declares contributes every bound its
/// declaration lists, `where` clauses included.
fn collect_upper_bounds(
    owner: &LibraryType,
    ty: Ty,
    seen: &mut HashSet<&'static str>,
    bounds: &mut Vec<Ty>,
) {
    {
        let other = ty.non_null();
        match other.kind() {
            TyKind::TyParam(name, bound) => {
                if !seen.insert(name.as_str()) {
                    return;
                }
                let declared = owner
                    .type_parameters
                    .type_params
                    .iter()
                    .position(|parameter| parameter == name)
                    .and_then(|index| owner.type_parameters.type_param_bounds.get(index))
                    .filter(|declared| !declared.is_empty());
                match declared {
                    Some(declared) => {
                        for bound in declared {
                            collect_upper_bounds(owner, *bound, seen, bounds);
                        }
                    }
                    None => collect_upper_bounds(owner, bound, seen, bounds),
                }
            }
            TyKind::DefinitelyNotNull(inner) => collect_upper_bounds(owner, inner, seen, bounds),
            _ => {
                if !bounds.contains(&other) {
                    bounds.push(other);
                }
            }
        }
    }
}
