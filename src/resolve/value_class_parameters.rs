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
//! Full value classes (`+FullValueClasses` without `@JvmInline`) publish no underlying type and are
//! not followed.

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
        let jvm_inline = cl.annotations.iter().any(|annotation| {
            self.annotation_identity_in_scope(scope, annotation)
                == Some(crate::types::wk::jvm_inline())
        });
        // The parameter count rules of `ValueClassDeclaration::representation` already rejected a
        // declaration kotlinc stops checking before this loop.
        let count = cl.props.len();
        let full = if self.file.full_value_classes {
            if jvm_inline && count != 1 || count == 0 {
                return;
            }
            record.value_underlying.is_none()
        } else {
            if count != 1 {
                return;
            }
            false
        };
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
                || (parameter.is_open && !parameter.is_override);
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
                } else if parameter.is_property && cl.modality == Modality::Sealed {
                    self.diags.error(
                        parameter.declaration_span,
                        "sealed value class primary constructor cannot have property parameters.",
                    );
                }
                continue;
            }
            // An inline value class publishes its single property's declared type over its own
            // type parameters; that is the type kotlinc's loop reads.
            let Some(declaration) = record
                .value_declaration
                .as_ref()
                .filter(|_| ordinal == 0 && parameter.is_property)
            else {
                continue;
            };
            let underlying = declaration.underlying;
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
            match value_class_recursion(
                &self.fed_source(),
                &record,
                underlying,
                &mut HashSet::new(),
            ) {
                Some(Recursion::Plain) => {
                    self.diags
                        .error(parameter.ty.span, "value class cannot be recursive.");
                }
                Some(Recursion::ViaTypeParameters) => self.report_deprecation_error(
                    parameter.ty.span,
                    "ForbidValueClassRecursionViaTypeParameters",
                    self.file.forbid_value_class_recursion_via_type_parameters,
                    "VALUE_CLASS_CANNOT_BE_RECURSIVE_VIA_TYPE_PARAMETERS_WARNING",
                    "value class cannot be recursive.",
                ),
                None => {}
            }
        }
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
        let expanded = match underlying {
            Ty::Unit => "kotlin/Unit",
            Ty::Obj(name, _) if name == crate::types::wk::unit() => "kotlin/Unit",
            Ty::Nothing => "kotlin/Nothing",
            Ty::Obj(name, _) if name == crate::types::wk::nothing() => "kotlin/Nothing",
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

/// kotlinc's `getValueClassTypeRecursionType`: whether following the underlying types of inline
/// value classes from `ty` reaches a type already on `path`, and how `ty` itself entered the walk.
/// `owner` declares the type parameters `ty` may refer to.
fn value_class_recursion(
    source: &dyn crate::symbol_source::SymbolSource,
    owner: &LibraryType,
    ty: Ty,
    path: &mut HashSet<Ty>,
) -> Option<Recursion> {
    let plain = match ty.non_null() {
        Ty::Obj(name, _) => Some(name),
        _ => None,
    };
    let expected = if plain.is_some() {
        Recursion::Plain
    } else {
        Recursion::ViaTypeParameters
    };
    let classifier = plain.or_else(|| single_upper_bound_classifier(owner, ty))?;
    let record = source.classifier(classifier)?;
    let declaration = record.value_declaration.as_ref()?;
    if !path.insert(ty) {
        return Some(expected);
    }
    let recursive = value_class_recursion(source, &record, declaration.underlying, path).is_some();
    path.remove(&ty);
    recursive.then_some(expected)
}

/// kotlinc's `leastUpperBound(...).toRegularClassSymbol()` for a type that is not itself a
/// classifier type: the classifier of the one non-type-parameter upper bound reached through
/// type-parameter bounds. Several bounds intersect into a type that names no single classifier.
fn single_upper_bound_classifier(owner: &LibraryType, ty: Ty) -> Option<TypeName> {
    let mut bounds = Vec::new();
    collect_upper_bounds(owner, ty, &mut HashSet::new(), &mut bounds);
    let any = Ty::Obj(crate::types::wk::any(), &[]);
    if bounds.len() > 1 {
        bounds.retain(|bound| bound.non_null() != any);
    }
    match bounds.as_slice() {
        [bound] => match bound.non_null() {
            Ty::Obj(name, _) => Some(name),
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
    match ty.non_null() {
        Ty::TyParam(name, bound) => {
            if !seen.insert(name) {
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
                None => collect_upper_bounds(owner, *bound, seen, bounds),
            }
        }
        Ty::DefinitelyNotNull(inner) => collect_upper_bounds(owner, *inner, seen, bounds),
        other => {
            if !bounds.contains(&other) {
                bounds.push(other);
            }
        }
    }
}
