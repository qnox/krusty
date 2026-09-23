//! JVM realization of Kotlin non-null parameter contracts.
//!
//! Checked FIR/common IR retain semantic parameter types and source names. Whether those contracts
//! become `Intrinsics.checkNotNullParameter` calls is a JVM backend choice, made here before type
//! erasure. Value-class lowering may subsequently remove a guard when the selected carrier is a
//! primitive. No frontend phase records an intrinsic name or makes a JVM representation decision.

use crate::ir::{FunId, IrFile};
use crate::types::Ty;
use std::collections::HashSet;

fn requires_reference_guard(ty: Ty) -> bool {
    ty.is_reference() && !ty.upper_bound_admits_null()
}

fn realize_function(ir: &mut IrFile, function: FunId) {
    if ir.private_methods.contains(&function) {
        return;
    }
    let Some(parameters) = ir
        .functions
        .get(function as usize)
        .map(|value| value.params.clone())
    else {
        return;
    };
    let declared_nullable = ir
        .fn_param_declared_nullable
        .get(&function)
        .cloned()
        .unwrap_or_default();
    let checks = &mut ir.functions[function as usize].param_checks;
    checks.resize(parameters.len(), None);
    for (ordinal, ty) in parameters.into_iter().enumerate() {
        if checks[ordinal].is_some()
            || declared_nullable.get(ordinal).copied().unwrap_or(false)
            || !requires_reference_guard(ty)
        {
            continue;
        }
        checks[ordinal] = Some(crate::ir::IrParameterCheck::NonNull);
    }
}

pub(super) fn realize(ir: &mut IrFile) {
    let functions = ir
        .checked_callable_functions
        .values()
        .copied()
        .collect::<HashSet<_>>();
    for function in functions {
        realize_function(ir, function);
    }

    for class in &mut ir.classes {
        if !class.is_source_declared {
            continue;
        }
        for parameter in &mut class.ctor_args {
            if parameter.check.is_some() {
                continue;
            }
            let Some(name) = parameter.name.as_ref() else {
                continue;
            };
            let Some(declared) = parameter.declared_ty else {
                continue;
            };
            if requires_reference_guard(declared) {
                parameter.check = Some(name.clone());
            }
        }
    }
}
