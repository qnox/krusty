//! JVM erasure of common-IR function type parameters.
//!
//! Checked FIR and common IR retain a type parameter as its semantic identity plus the declaration's
//! complete intersection of upper bounds. A JVM method descriptor instead needs one physical bound:
//! the concrete class bound when present, otherwise the first interface bound, otherwise `Object`.
//! A method type parameter may be bounded by a type parameter of an enclosing declaration. Common
//! IR records the exact non-local declarations its bounds reference; the backend does not
//! reconstruct an owner from a JVM name. Keeping the conversion here prevents common lowering from
//! committing a backend representation.

use super::default_parameter_representation::primitive_bounded_type_parameter;
use crate::ir::{IrFile, IrTypeParameter};
use crate::types::{wk, Ty};
use std::collections::{HashMap, HashSet};

fn declared_primary_bound(parameter: &IrTypeParameter) -> Option<Ty> {
    parameter
        .bounds
        .iter()
        .find(|(bound, is_interface)| {
            !*is_interface && !matches!(bound.non_null(), Ty::TyParam(..))
        })
        .or_else(|| parameter.bounds.first())
        .map(|(bound, _)| *bound)
}

fn parameter_named<'a>(
    name: &str,
    parameters: &'a [IrTypeParameter],
    referenced: &'a [IrTypeParameter],
) -> Option<&'a IrTypeParameter> {
    parameters
        .iter()
        .chain(referenced)
        .find(|parameter| parameter.semantic_name == name)
}

fn resolve_primary_bound(
    name: &str,
    parameters: &[IrTypeParameter],
    referenced: &[IrTypeParameter],
    resolved: &mut HashMap<String, Ty>,
    visiting: &mut HashSet<String>,
) -> Ty {
    if let Some(bound) = resolved.get(name) {
        return *bound;
    }
    if !visiting.insert(name.to_owned()) {
        return Ty::obj_name(wk::any());
    }
    let bound = parameter_named(name, parameters, referenced)
        .and_then(declared_primary_bound)
        .map(|bound| match bound.non_null() {
            Ty::TyParam(other, _) => {
                resolve_primary_bound(other, parameters, referenced, resolved, visiting)
            }
            concrete => concrete.erased_recv(),
        })
        .unwrap_or_else(|| Ty::obj_name(wk::any()));
    visiting.remove(name);
    resolved.insert(name.to_owned(), bound);
    bound
}

/// A type-parameter occurrence erases to its primary bound. kotlinc's type mapper keeps the
/// occurrence's nullability on that bound (`T : Int?` maps as `Int?`, so `Integer`, not `int`; and
/// `T : IC?` maps as the nullable value class), and a bare `T` is nullable when a bound along its
/// chain is. A reference bound erases to the same class either way. A non-null occurrence bounded
/// by a JVM primitive stays the type parameter: its descriptor is already the primitive, and its
/// `$default` stub takes the JDK wrapper instead.
fn physical_type(ty: Ty, erasures: &HashMap<String, Ty>) -> Ty {
    match ty {
        Ty::TyParam(name, _) => match erasures.get(name).copied() {
            Some(_) if primitive_bounded_type_parameter(ty).is_some() => ty,
            Some(erasure) if ty.upper_bound_admits_null() => Ty::nullable(erasure),
            Some(erasure) => erasure,
            None => ty,
        },
        Ty::Nullable(inner) if matches!(*inner, Ty::TyParam(..)) => {
            Ty::nullable(physical_type(*inner, erasures))
        }
        Ty::PlatformNullable(inner) if matches!(*inner, Ty::TyParam(..)) => {
            Ty::platform_nullable(physical_type(*inner, erasures))
        }
        _ => ty,
    }
}

/// JVM primary erasure for each declaration-owned type parameter. Reified-operation realization
/// consumes the same map before descriptors erase their occurrences, so marker placeholders and
/// method signatures cannot disagree about an intersection's physical class bound.
/// Erase `parameters` with the same primary class bound `lower_function_type_parameters` writes
/// onto a same-file function. A copied signature — a same-module call — uses this so its physical
/// parameter vector is that function's vector.
pub(super) fn erased_parameters(parameters: &[Ty], type_parameters: &[IrTypeParameter]) -> Vec<Ty> {
    erase_with(parameters, &parameter_erasures(type_parameters))
}

/// Same erasure as [`erased_parameters`] for a referenced module callable, which copies the
/// semantic name and bounds without the declaration's variance or reified flag.
pub(super) fn erased_callable_parameters(
    parameters: &[Ty],
    type_parameters: &[crate::ir::IrCallableTypeParameter],
) -> Vec<Ty> {
    let type_parameters = type_parameters
        .iter()
        .map(|parameter| IrTypeParameter {
            name: parameter.semantic_name.clone(),
            semantic_name: parameter.semantic_name.clone(),
            bounds: parameter.bounds.to_vec(),
            variance: crate::types::TypeVariance::Invariant,
            reified: false,
        })
        .collect::<Vec<_>>();
    erased_parameters(parameters, &type_parameters)
}

fn erase_with(parameters: &[Ty], erasures: &HashMap<String, Ty>) -> Vec<Ty> {
    parameters
        .iter()
        .copied()
        .map(|parameter| physical_type(parameter, erasures))
        .collect()
}

pub(super) fn parameter_erasures(parameters: &[IrTypeParameter]) -> HashMap<String, Ty> {
    parameter_erasures_with(parameters, &[])
}

/// Erase `parameters` the way [`parameter_erasures`] does, also consulting the exact non-local
/// declarations common lowering recorded from their bounds.
pub(super) fn parameter_erasures_with(
    parameters: &[IrTypeParameter],
    referenced: &[IrTypeParameter],
) -> HashMap<String, Ty> {
    let mut erasures = HashMap::new();
    for parameter in parameters {
        resolve_primary_bound(
            &parameter.semantic_name,
            parameters,
            referenced,
            &mut erasures,
            &mut HashSet::new(),
        );
    }
    erasures
}

/// Exact non-local type-parameter declarations referenced by `function`'s own bounds.
pub(super) fn bound_type_parameters(ir: &IrFile, function: u32) -> Vec<IrTypeParameter> {
    ir.callable_bound_type_parameters
        .get(&function)
        .cloned()
        .unwrap_or_default()
}

pub(super) fn lower_function_type_parameters(ir: &mut IrFile) {
    let signatures = ir
        .signatures
        .iter()
        .map(|(&function, signature)| (function, signature.type_params.clone()))
        .collect::<Vec<_>>();
    let signatures = signatures
        .into_iter()
        .map(|(function, parameters)| {
            let referenced = bound_type_parameters(ir, function);
            (function, parameters, referenced)
        })
        .collect::<Vec<_>>();
    for (function, parameters, referenced) in signatures {
        if parameters.is_empty() {
            continue;
        }
        let erasures = parameter_erasures_with(&parameters, &referenced);
        let Some(function) = ir.functions.get_mut(function as usize) else {
            continue;
        };
        for parameter in &mut function.params {
            *parameter = physical_type(*parameter, &erasures);
        }
        function.ret = physical_type(function.ret, &erasures);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TypeVariance;

    fn parameter(source_name: &str, semantic_name: &str, bound: Ty) -> IrTypeParameter {
        IrTypeParameter {
            name: source_name.to_owned(),
            semantic_name: semantic_name.to_owned(),
            bounds: vec![(bound, false)],
            variance: TypeVariance::Invariant,
            reified: false,
        }
    }

    #[test]
    fn a_method_parameter_erases_through_an_enclosing_class_parameter() {
        let class_parameter = parameter("S", "classifier#S", Ty::obj("sample/Entity"));
        let method_parameter = parameter(
            "T",
            "callable#T",
            Ty::ty_param("classifier#S", Ty::obj("sample/Entity")),
        );

        let erasures = parameter_erasures_with(&[method_parameter], &[class_parameter]);

        assert_eq!(
            erasures.get("callable#T").copied(),
            Some(Ty::obj("sample/Entity"))
        );
    }

    #[test]
    fn a_method_parameter_shadows_an_enclosing_parameter_with_the_same_source_spelling() {
        let class_parameter = parameter("T", "classifier#T", Ty::obj("sample/Entity"));
        let method_parameter = parameter("T", "callable#T", Ty::obj("sample/Text"));

        let erasures = parameter_erasures_with(&[method_parameter], &[class_parameter]);

        assert_eq!(
            erasures.get("callable#T").copied(),
            Some(Ty::obj("sample/Text"))
        );
    }
}
