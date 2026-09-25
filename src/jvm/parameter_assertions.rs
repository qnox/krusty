//! JVM realization of Kotlin non-null parameter contracts.
//!
//! Checked FIR/common IR retain semantic parameter types and source names. Whether those contracts
//! become `Intrinsics.checkNotNullParameter` calls is a JVM backend choice, made here before type
//! erasure. Value-class lowering may subsequently remove a guard when the selected carrier is a
//! primitive. No frontend phase records an intrinsic name or makes a JVM representation decision.

use crate::ir::{FunId, IrFile, IrParameterRole};
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
    // A guard quotes the parameter's name. A value parameter whose provider publishes none (a
    // Java-declared parameter of a mapped collection interface, forwarded by delegation) has no
    // spelling to quote, and a name is never invented for it.
    let unnamed = ir
        .function_parameter_identities(function)
        .map(|identities| {
            identities
                .iter()
                .map(|identity| {
                    identity.role == IrParameterRole::Value && identity.source_name.is_none()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let checks = &mut ir.functions[function as usize].param_checks;
    checks.resize(parameters.len(), None);
    for (ordinal, ty) in parameters.into_iter().enumerate() {
        if checks[ordinal].is_some()
            || declared_nullable.get(ordinal).copied().unwrap_or(false)
            || unnamed.get(ordinal).copied().unwrap_or(false)
            || !requires_reference_guard(ty)
        {
            continue;
        }
        checks[ordinal] = Some(crate::ir::IrParameterCheck::NonNull);
    }
}

pub(super) fn realize(ir: &mut IrFile) {
    let mut functions = ir
        .checked_callable_functions
        .values()
        .copied()
        .collect::<HashSet<_>>();
    // Property accessors are source declarations materialized after ordinary callable lowering.
    // Consume their exact common-IR identity edges rather than recovering accessor shape from a
    // JVM method name. This also covers context and extension-receiver parameters.
    for layout in ir.local_property_layouts.values() {
        let (getter, setter) = match layout {
            crate::ir::IrLocalPropertyLayout::TopLevelStorage { getter, setter, .. }
            | crate::ir::IrLocalPropertyLayout::Member { getter, setter, .. } => (*getter, *setter),
            crate::ir::IrLocalPropertyLayout::TopLevelAccessor { getter, setter, .. }
            | crate::ir::IrLocalPropertyLayout::MemberExtension { getter, setter, .. } => {
                (Some(*getter), *setter)
            }
        };
        functions.extend(getter);
        functions.extend(setter);
    }
    // Interface-delegation forwarders are compiler-generated but public, and kotlinc checks their
    // parameters like any source override's. Their exact identities are the override edges they
    // publish.
    for edges in ir.function_overrides.values() {
        functions.extend(edges.iter().filter_map(|edge| edge.implementation_function));
    }
    for edges in ir.property_overrides.values() {
        for edge in edges {
            functions.extend(edge.implementation_getter);
            functions.extend(edge.implementation_setter);
        }
    }
    for function in functions {
        realize_function(ir, function);
    }

    for class in &mut ir.classes {
        if !class.is_source_declared {
            continue;
        }
        let assertion_names = crate::jvm::parameter_names::constructor_assertions(&class.ctor_args);
        for (parameter, assertion_name) in class.ctor_args.iter_mut().zip(assertion_names) {
            if parameter.check.is_some() {
                continue;
            }
            let Some(name) = assertion_name else {
                continue;
            };
            let Some(declared) = parameter.declared_ty else {
                continue;
            };
            if requires_reference_guard(declared) {
                parameter.check = Some(name);
            }
        }
    }
}
