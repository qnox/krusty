//! JVM erasure of common-IR function type parameters.
//!
//! Checked FIR and common IR retain a type parameter as its semantic identity plus the declaration's
//! complete intersection of upper bounds. A JVM method descriptor instead needs one physical bound:
//! the concrete class bound when present, otherwise the first interface bound, otherwise `Object`.
//! Keeping this conversion here prevents common lowering from re-reading source bound order or
//! committing a backend representation.

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

fn resolve_primary_bound(
    name: &str,
    parameters: &[IrTypeParameter],
    resolved: &mut HashMap<String, Ty>,
    visiting: &mut HashSet<String>,
) -> Ty {
    if let Some(bound) = resolved.get(name) {
        return *bound;
    }
    if !visiting.insert(name.to_owned()) {
        return Ty::obj_name(wk::any());
    }
    let bound = parameters
        .iter()
        .find(|parameter| parameter.semantic_name == name)
        .and_then(declared_primary_bound)
        .map(|bound| match bound.non_null() {
            Ty::TyParam(other, _) => resolve_primary_bound(other, parameters, resolved, visiting),
            concrete => concrete.erased_recv(),
        })
        .unwrap_or_else(|| Ty::obj_name(wk::any()));
    visiting.remove(name);
    resolved.insert(name.to_owned(), bound);
    bound
}

fn physical_type(ty: Ty, erasures: &HashMap<String, Ty>) -> Ty {
    match ty {
        Ty::TyParam(name, _) => erasures.get(name).copied().unwrap_or(ty),
        Ty::Nullable(inner) if matches!(*inner, Ty::TyParam(..)) => {
            Ty::nullable(physical_type(*inner, erasures))
        }
        Ty::PlatformNullable(inner) if matches!(*inner, Ty::TyParam(..)) => {
            Ty::platform_nullable(physical_type(*inner, erasures))
        }
        _ => ty,
    }
}

pub(super) fn lower_function_type_parameters(ir: &mut IrFile) {
    let signatures = ir
        .signatures
        .iter()
        .map(|(&function, signature)| (function, signature.type_params.clone()))
        .collect::<Vec<_>>();
    for (function, parameters) in signatures {
        if parameters.is_empty() {
            continue;
        }
        let mut erasures = HashMap::new();
        for parameter in &parameters {
            resolve_primary_bound(
                &parameter.semantic_name,
                &parameters,
                &mut erasures,
                &mut HashSet::new(),
            );
        }
        let Some(function) = ir.functions.get_mut(function as usize) else {
            continue;
        };
        for parameter in &mut function.params {
            *parameter = physical_type(*parameter, &erasures);
        }
        function.ret = physical_type(function.ret, &erasures);
    }
}
