//! The part of a runtime type test the operand already proves.
//!
//! A JVM type test sees only the target's classifier. Kotlin accepts `x is C<A>` when the rest of
//! `C<A>` follows from what is statically known about `x`: FIR's `findStaticallyKnownSubtype`
//! applies `C`'s own type parameters to the arguments of the operand's type that it can unify through
//! `C`'s supertypes. A parameter that unification does not reach stays the symbolic parameter; one
//! bound to a star projection becomes a star. The caller then asks whether that type is a subtype of
//! the target.

use std::collections::HashMap;

use super::receiver_hierarchy;
use crate::libraries::function_classifiers::supertype_classifier;
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName};

/// A type in the nominal form FIR compares: every function type is the `FunctionN` /
/// `SuspendFunctionN` classifier it is an instance of, so function types and classifier spellings
/// share one hierarchy (`KSuspendFunction1<A, R>` reaches `SuspendFunction1<A, R>`).
pub(crate) fn nominal_type(ty: Ty) -> Ty {
    match ty {
        Ty::Fun(_) => nominal_type(supertype_classifier(ty)),
        Ty::Obj(name, args) if !args.is_empty() => Ty::obj_args_name(
            name,
            &args.iter().copied().map(nominal_type).collect::<Vec<_>>(),
        ),
        Ty::Nullable(inner) => Ty::nullable(nominal_type(*inner)),
        Ty::PlatformNullable(inner) => Ty::platform_nullable(nominal_type(*inner)),
        Ty::InProjection(inner) => Ty::in_projection(nominal_type(*inner)),
        Ty::OutProjection(inner) => Ty::out_projection(nominal_type(*inner)),
        Ty::StarProjection(inner) => Ty::star_projection(nominal_type(*inner)),
        Ty::Intersection(parts) => {
            Ty::intersection(&parts.iter().copied().map(nominal_type).collect::<Vec<_>>())
        }
        other => other,
    }
}

/// FIR's `findStaticallyKnownSubtype(supertype, subtypeClass)` over nominal types. `supertype` is
/// non-null; `is_subtype` is the type checker that picks between two bindings an intersection
/// operand contributes for one parameter. `None` when the classifier has no declaration.
pub(crate) fn statically_known_subtype(
    source: &dyn SymbolSource,
    supertype: Ty,
    subtype_class: TypeName,
    is_subtype: &dyn Fn(Ty, Ty) -> bool,
) -> Option<Ty> {
    if matches!(supertype, Ty::Obj(name, _) if name == subtype_class) {
        return Some(supertype);
    }
    let classifier = source.classifier(subtype_class)?;
    let formals = classifier
        .type_params()
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let symbolic_args = formals
        .iter()
        .enumerate()
        .map(|(index, name)| {
            Ty::ty_param(
                name,
                classifier
                    .type_param_bounds()
                    .get(index)
                    .and_then(|bounds| bounds.first())
                    .copied()
                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any"))),
            )
        })
        .collect::<Vec<_>>();
    let symbolic = nominal_type(Ty::obj_args_name(subtype_class, &symbolic_args));
    let hierarchy = receiver_hierarchy(source, symbolic);
    let normalized = match supertype {
        Ty::Intersection(parts) => parts.to_vec(),
        other => vec![other],
    };
    let mut substitution = HashMap::<&str, Ty>::new();
    for part in normalized {
        let constructor = part.non_null().kotlin_class_internal();
        let with_variables = constructor.and_then(|constructor| {
            hierarchy
                .iter()
                .map(|(applied, _)| *applied)
                .find(|applied| applied.kotlin_class_internal() == Some(constructor))
        });
        let mut result = HashMap::new();
        if let Some(with_variables) = with_variables {
            let unification = Unification {
                variables: &formals,
                is_subtype,
            };
            if !unification.unify(supertype, with_variables, &mut result) {
                result.clear();
            }
        }
        for formal in &formals {
            if let Some(value) = result.get(*formal) {
                let value = match *value {
                    Ty::StarProjection(_) => {
                        Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")))
                    }
                    Ty::InProjection(inner) | Ty::OutProjection(inner) => *inner,
                    other => other,
                };
                substitution.insert(formal, value);
            }
        }
    }
    let arguments = formals
        .iter()
        .zip(&symbolic_args)
        .map(|(formal, symbolic)| substitution.get(formal).copied().unwrap_or(*symbolic))
        .collect::<Vec<_>>();
    Some(nominal_type(Ty::obj_args_name(subtype_class, &arguments)))
}

/// FIR's `doUnify`: bind the `variables` occurring in a type with parameters to the matching parts
/// of an actual type. `false` only for conflicting values of one variable; a shape that cannot be
/// unified contributes nothing and still succeeds.
struct Unification<'a> {
    variables: &'a [&'a str],
    is_subtype: &'a dyn Fn(Ty, Ty) -> bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProjectionKind {
    Invariant,
    In,
    Out,
    Star,
}

fn projection_kind(ty: Ty) -> ProjectionKind {
    match ty {
        Ty::InProjection(_) => ProjectionKind::In,
        Ty::OutProjection(_) => ProjectionKind::Out,
        Ty::StarProjection(_) => ProjectionKind::Star,
        _ => ProjectionKind::Invariant,
    }
}

/// The projected type, at the lower bound of a flexible type. A star has none.
fn projected_type(ty: Ty) -> Option<Ty> {
    let inner = match ty {
        Ty::StarProjection(_) => return None,
        Ty::InProjection(inner) | Ty::OutProjection(inner) => *inner,
        other => other,
    };
    Some(match inner {
        Ty::PlatformNullable(lower) => *lower,
        other => other,
    })
}

fn replace_projected(projection: Ty, ty: Ty) -> Ty {
    match projection {
        Ty::InProjection(_) => Ty::in_projection(ty),
        Ty::OutProjection(_) => Ty::out_projection(ty),
        _ => ty,
    }
}

impl Unification<'_> {
    fn unify(&self, original: Ty, with_parameters: Ty, result: &mut HashMap<String, Ty>) -> bool {
        let original_type = projected_type(original);
        let parameters_type = projected_type(with_parameters);
        if parameters_type == Some(Ty::Error) {
            return true;
        }
        if let Some(Ty::Intersection(parts)) = original_type {
            let mut merged = HashMap::<String, Ty>::new();
            for &part in parts {
                let mut local = HashMap::new();
                if !self.unify(part, with_parameters, &mut local) {
                    return false;
                }
                for (variable, value) in local {
                    let replace = match merged.get(&variable) {
                        None => true,
                        Some(existing) => {
                            projection_kind(value) == ProjectionKind::Invariant
                                && projection_kind(*existing) == ProjectionKind::Invariant
                                && (self.is_subtype)(value, *existing)
                        }
                    };
                    if replace {
                        merged.insert(variable, value);
                    }
                }
            }
            result.extend(merged);
            return true;
        }
        let original_kind = projection_kind(original);
        let parameters_kind = projection_kind(with_parameters);
        if original_kind == parameters_kind
            && matches!(original_kind, ProjectionKind::In | ProjectionKind::Out)
        {
            let (Some(original_type), Some(parameters_type)) = (original_type, parameters_type)
            else {
                return true;
            };
            return self.unify(original_type, parameters_type, result);
        }
        let original_nullable = original_type.is_some_and(Ty::is_nullable);
        let parameters_nullable = parameters_type.is_some_and(Ty::is_nullable);
        if original_nullable && parameters_nullable {
            let (Some(original_type), Some(parameters_type)) = (original_type, parameters_type)
            else {
                return true;
            };
            return self.unify(
                replace_projected(original, original_type.non_null()),
                replace_projected(with_parameters, parameters_type.non_null()),
                result,
            );
        }
        if original_kind != parameters_kind && parameters_kind != ProjectionKind::Invariant {
            return true;
        }
        if let Some(Ty::DefinitelyNotNull(inner)) = parameters_type {
            return self.unify(original, replace_projected(with_parameters, *inner), result);
        }
        if original_kind != ProjectionKind::Star && !original_nullable && parameters_nullable {
            return true;
        }
        if let Some(Ty::TyParam(name, _)) = parameters_type {
            if self.variables.contains(&name) {
                if result.get(name).is_some_and(|known| *known != original) {
                    return false;
                }
                result.insert(name.to_string(), original);
                return true;
            }
        }
        if original_nullable != parameters_nullable || original_kind != parameters_kind {
            return true;
        }
        let (Some(original_type), Some(parameters_type)) = (original_type, parameters_type) else {
            return true;
        };
        let (Ty::Obj(original_name, original_args), Ty::Obj(parameters_name, parameters_args)) =
            (original_type.non_null(), parameters_type.non_null())
        else {
            return true;
        };
        if original_name != parameters_name || original_args.len() != parameters_args.len() {
            return true;
        }
        original_args
            .iter()
            .zip(parameters_args)
            .all(|(&original, &with_parameters)| self.unify(original, with_parameters, result))
    }
}
